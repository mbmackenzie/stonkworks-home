use crate::mqtt::{Connection, MqttEvent};
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::sntp::{EspSntp, SntpConf, SyncStatus};
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use sonos_desk_firmware::{
    config::Config,
    controller::{Controller, ControllerConfig, Effect, Event, Time},
    inputs::InputEvent,
    protocol::Observation,
};
use std::ffi::CStr;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
    mpsc::Receiver,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub struct ControlIo {
    pub gate: Arc<AtomicU32>,
    pub receiver: Receiver<(u32, u64, InputEvent)>,
    pub start: Instant,
}

pub fn run(
    modem: Modem<'static>,
    config: Config,
    ca: &'static CStr,
    io: Option<ControlIo>,
) -> anyhow::Result<()> {
    let mut wifi = crate::wifi::start(modem, &config.wifi)?;

    let mut time_config = SntpConf::default();
    time_config.servers.fill("");
    time_config.servers[0] = &config.ntp_server;
    let sntp = EspSntp::new(&time_config)?;
    let mut clock_ready = false;
    let mut backoff_seconds = 1_u64;

    let mut generation = 0_u32;
    loop {
        // The library owns Wi-Fi, SNTP and TLS. We only sequence them and report status.
        let result = (|| -> anyhow::Result<()> {
            if !wifi.is_up()? {
                println!("Wi-Fi: connecting");
                if !wifi.is_connected()? {
                    wifi.connect()?;
                }
                wifi.wait_netif_up()?;
                println!("Wi-Fi: connected; IP address acquired");
            }
            if !clock_ready {
                println!("Clock: waiting for configured NTP server");
                let deadline = Instant::now() + Duration::from_secs(30);
                while !matches!(sntp.get_sync_status(), SyncStatus::Completed) {
                    anyhow::ensure!(
                        wifi.is_up()? && Instant::now() < deadline,
                        "Clock not synchronized"
                    );
                    thread::sleep(Duration::from_millis(100));
                }
                clock_ready = true;
                println!("Clock: synchronized; TLS certificate validation enabled");
            }
            mqtt_session(
                &wifi,
                &config,
                ca,
                &mut backoff_seconds,
                io.as_ref(),
                &mut generation,
            )
        })();

        if result.is_err() {
            println!("Network: unavailable; retrying (inputs continue, no commands sent)");
        }
        // A small random offset prevents several controllers reconnecting together.
        let jitter = u64::from(unsafe { esp_idf_svc::sys::esp_random() } % 251);
        let delay_ms = (backoff_seconds * 1000).saturating_sub(jitter).max(1000);
        thread::sleep(Duration::from_millis(delay_ms));
        backoff_seconds = (backoff_seconds * 2).min(30);
    }
}

fn mqtt_session(
    wifi: &BlockingWifi<EspWifi<'static>>,
    config: &Config,
    ca: &'static CStr,
    backoff_seconds: &mut u64,
    io: Option<&ControlIo>,
    generation: &mut u32,
) -> anyhow::Result<()> {
    let base = format!("stonkworks/v1/sonos-desk/{}/", config.mqtt.instance);
    let gate = io.map_or_else(|| Arc::new(AtomicU32::new(0)), |io| io.gate.clone());
    let mut controller = io.map(|_| {
        Controller::new(ControllerConfig {
            instance: config.mqtt.instance.clone(),
            client_id: config.mqtt.client_id.clone(),
            room_id: config.room_id.clone().unwrap(),
        })
    });
    println!("MQTT: connecting with verified TLS and MQTT 5");
    let mut connection = Connection::new(config, ca, gate.clone())?;
    let start = io.map_or_else(Instant::now, |io| io.start);
    let now = || Time {
        ms: start.elapsed().as_millis() as u64,
        utc_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as i64),
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut subscribed = false;
    let mut subscriptions = Vec::new();
    let result = (|| -> anyhow::Result<()> {
        loop {
            anyhow::ensure!(wifi.is_up()? && connection.alive(), "Connection lost");
            anyhow::ensure!(
                subscribed || Instant::now() < deadline,
                "MQTT subscriptions timed out"
            );
            // Drain network observations before accepting physical input.
            while let Some(event) = connection.next() {
                match event {
                    MqttEvent::Connected => {
                        println!("MQTT: connected over verified TLS");
                        *backoff_seconds = 1;
                        let mut topics = vec!["availability".to_string()];
                        if let Some(controller) = &controller {
                            topics.extend([
                                "rooms".into(),
                                format!("rooms/{}/state", controller.config.room_id),
                                format!("rooms/{}/availability", controller.config.room_id),
                                format!("clients/{}/results", controller.config.client_id),
                            ]);
                        }
                        for topic in topics {
                            subscriptions.push(connection.subscribe(&format!("{base}{topic}"))?);
                        }
                    }
                    MqttEvent::Subscribed(id) => {
                        if let Some(index) = subscriptions.iter().position(|pending| *pending == id)
                        {
                            subscriptions.swap_remove(index);
                        }
                        if subscriptions.is_empty() {
                            subscribed = true;
                            println!("MQTT: required QoS 1 subscriptions acknowledged");
                            if let Some(controller) = &mut controller {
                                apply(
                                    controller,
                                    Event::Subscribed,
                                    now(),
                                    &mut connection,
                                    &gate,
                                    generation,
                                )?;
                            }
                        }
                    }
                    MqttEvent::Message(topic, data) => {
                        if let Some(controller) = &mut controller {
                            let relative = topic
                                .strip_prefix(&base)
                                .ok_or_else(|| anyhow::anyhow!("Unexpected topic"))?;
                            let observation =
                                Observation::parse(relative, &data, &controller.config)
                                    .map_err(|_| anyhow::anyhow!("Invalid MQTT update"))?;
                            apply(
                                controller,
                                Event::Observation(observation),
                                now(),
                                &mut connection,
                                &gate,
                                generation,
                            )?;
                        } else {
                            println!(
                                "MQTT: service availability received (controls remain disabled)"
                            );
                        }
                    }
                }
            }
            if let Some(controller) = &mut controller {
                apply(
                    controller,
                    Event::Tick,
                    now(),
                    &mut connection,
                    &gate,
                    generation,
                )?;
                if let Some(io) = io {
                    while let Ok((input_generation, input_ms, input)) = io.receiver.try_recv() {
                        let time = now();
                        if gate.load(Ordering::Acquire) == input_generation
                            && input_generation != 0
                            && time.ms.saturating_sub(input_ms) < 100
                        {
                            apply(
                                controller,
                                Event::Input(input),
                                time,
                                &mut connection,
                                &gate,
                                generation,
                            )?;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    gate.store(0, Ordering::Release);
    if let Some(controller) = &mut controller {
        for effect in controller.handle(Event::Disconnected, now(), &mut random_id) {
            if let Effect::Log(message) = effect {
                println!("{message}");
            }
        }
        println!("Controller: unavailable; inputs discarded");
    }
    result // Dropping Connection destroys the SDK outbox; reconnect starts empty.
}

fn apply(
    controller: &mut Controller,
    event: Event,
    now: Time,
    connection: &mut Connection,
    gate: &AtomicU32,
    generation: &mut u32,
) -> anyhow::Result<()> {
    for effect in controller.handle(event, now, &mut random_id) {
        match effect {
            Effect::Ready(ready) => {
                if ready {
                    anyhow::ensure!(connection.alive(), "Connection lost");
                    *generation = generation.wrapping_add(1).max(1);
                    gate.store(*generation, Ordering::Release);
                    println!("Controller: ready");
                } else {
                    gate.store(0, Ordering::Release);
                    println!("Controller: unavailable; inputs discarded");
                }
            }
            Effect::Publish(command) => {
                if connection.publish(&command, now.ms).is_err() {
                    gate.store(0, Ordering::Release);
                    controller.handle(
                        Event::PublishFailed(command.request_id),
                        now,
                        &mut random_id,
                    );
                    println!("Command: outcome uncertain; connection discarded, no retry");
                    anyhow::bail!("Publish failed");
                }
            }
            Effect::Log(message) => println!("{message}"),
        }
    }
    Ok(())
}

fn random_id() -> Uuid {
    let mut bytes = [0_u8; 16];
    // Wi-Fi remains enabled, so ESP-IDF's hardware RNG has its RF entropy source.
    unsafe { esp_idf_svc::sys::esp_fill_random(bytes.as_mut_ptr().cast(), bytes.len()) };
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}
