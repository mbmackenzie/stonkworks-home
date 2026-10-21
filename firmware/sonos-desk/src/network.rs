use std::ffi::CStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::mqtt::client::{
    EspMqttClient, EventPayload, Mqtt5ConnectionPropertyConfig, MqttClientConfiguration,
    MqttProtocolVersion, QoS,
};
use esp_idf_svc::sntp::{EspSntp, SntpConf, SyncStatus};
use esp_idf_svc::tls::X509;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use sonos_desk_firmware::config::Config;

pub fn run(modem: Modem<'static>, config: Config, ca: &'static CStr) -> anyhow::Result<()> {
    let event_loop = EspSystemEventLoop::take()?;
    // None keeps Wi-Fi settings in RAM rather than storing another credential copy in NVS.
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, event_loop.clone(), None)?, event_loop)?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: config
            .wifi
            .ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid SSID"))?,
        password: config
            .wifi
            .password
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid Wi-Fi passphrase"))?,
        // ESP-IDF treats this as a minimum security threshold, not a list of modes.
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;

    let mut time_config = SntpConf::default();
    time_config.servers.fill("");
    time_config.servers[0] = &config.ntp_server;
    let sntp = EspSntp::new(&time_config)?;
    let mut clock_ready = false;
    let mut backoff_seconds = 1_u64;

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
            mqtt_session(&wifi, &config, ca, &mut backoff_seconds)
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

enum MqttEvent {
    Connected,
    Subscribed,
    Availability,
    Disconnected,
    Error,
}

fn mqtt_session(
    wifi: &BlockingWifi<EspWifi<'static>>,
    config: &Config,
    ca: &'static CStr,
    backoff_seconds: &mut u64,
) -> anyhow::Result<()> {
    let topic = format!(
        "stonkworks/v1/sonos-desk/{}/availability",
        config.mqtt.instance
    );
    let url = format!("mqtts://{}:{}", config.mqtt.host, config.mqtt.port);
    let (sender, receiver) = mpsc::sync_channel(8);
    let overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = overflow.clone();
    println!("MQTT: connecting with verified TLS and MQTT 5");

    let mut client = EspMqttClient::new_cb(
        &url,
        &MqttClientConfiguration {
            client_id: Some(&config.mqtt.client_id),
            username: Some(&config.mqtt.username),
            password: Some(&config.mqtt.password),
            protocol_version: Some(MqttProtocolVersion::V5),
            mqtt5_connection_property: Some(Mqtt5ConnectionPropertyConfig {
                session_expiry_interval: Some(0),
                ..Default::default()
            }),
            server_certificate: Some(X509::pem(ca)),
            reconnect_timeout: None, // The owner below creates a fresh client on reconnect.
            keep_alive_interval: Some(Duration::from_secs(15)),
            network_timeout: Duration::from_secs(5),
            ..Default::default() // Clean Start; certificate and hostname checks stay enabled.
        },
        move |event| {
            let message = match event.payload() {
                EventPayload::Connected(_) => MqttEvent::Connected,
                EventPayload::Subscribed(_) => MqttEvent::Subscribed,
                EventPayload::Received { .. } => MqttEvent::Availability,
                EventPayload::Disconnected => MqttEvent::Disconnected,
                EventPayload::Error(_) => MqttEvent::Error,
                _ => return,
            };
            // Never block the SDK callback or print a payload containing private data.
            if sender.try_send(message).is_err() {
                callback_overflow.store(true, Ordering::Relaxed);
            }
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut connected = false;
    loop {
        anyhow::ensure!(
            wifi.is_up()? && !overflow.load(Ordering::Relaxed),
            "Connection lost"
        );
        anyhow::ensure!(
            connected || Instant::now() < deadline,
            "MQTT connection timed out"
        );
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(MqttEvent::Connected) => {
                connected = true;
                *backoff_seconds = 1;
                println!("MQTT: connected over verified TLS");
                client.subscribe(&topic, QoS::AtLeastOnce)?;
            }
            Ok(MqttEvent::Subscribed) => {
                println!("MQTT: subscription acknowledged; waiting for service availability")
            }
            Ok(MqttEvent::Availability) => {
                println!("MQTT: service availability received (controls remain disabled)")
            }
            Ok(MqttEvent::Disconnected | MqttEvent::Error) => {
                anyhow::bail!("MQTT disconnected; check broker, credentials and CA");
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("MQTT event stream closed"),
        }
    }
    // Returning drops the client and its outbox before the owner retries.
}
