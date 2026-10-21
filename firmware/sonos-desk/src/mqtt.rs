//! The connection owner alone subscribes/publishes. Callbacks only copy bounded events.
use esp_idf_svc::mqtt::client::{
    Details, EspMqttClient, EventPayload, Mqtt5ConnectionPropertyConfig, MqttClientConfiguration,
    MqttProtocolVersion, QoS,
};
use esp_idf_svc::tls::X509;
use esp_idf_svc::{handle::RawHandle, sys};
use sonos_desk_firmware::{
    config::Config,
    protocol::{Assembly, Command},
};
use std::ffi::{CStr, c_void};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::time::Duration;

pub enum MqttEvent {
    Connected,
    Subscribed(u32),
    Message(String, Vec<u8>),
}
struct Callback {
    sender: SyncSender<MqttEvent>,
    dead: AtomicBool,
    enabled: AtomicBool,
    queued: AtomicUsize,
    gate: Arc<AtomicU32>,
}
impl Callback {
    fn fail(&self) {
        self.dead.store(true, Ordering::Release);
        self.gate.store(0, Ordering::Release);
    }
    fn send(&self, event: MqttEvent) {
        let size = if let MqttEvent::Message(_, data) = &event {
            data.len()
        } else {
            0
        };
        if self.queued.fetch_add(size, Ordering::Relaxed) + size > 64 * 1024 {
            self.queued.fetch_sub(size, Ordering::Relaxed);
            self.fail();
            return;
        }
        if self.sender.try_send(event).is_err() {
            self.queued.fetch_sub(size, Ordering::Relaxed);
            self.fail();
        }
    }
}

pub struct Connection {
    // Drop the SDK client (and join its callbacks) before releasing callback context.
    client: EspMqttClient<'static>,
    callback: Arc<Callback>,
    receiver: Receiver<MqttEvent>,
    // Setter stores a pointer. Keep properties alive even if publish fails before consuming them.
    properties: Box<sys::esp_mqtt5_publish_property_config_t>,
}

impl Connection {
    pub fn new(config: &Config, ca: &'static CStr, gate: Arc<AtomicU32>) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(16);
        let callback = Arc::new(Callback {
            sender,
            dead: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            gate,
        });
        let cb = callback.clone();
        let mut assembly = Assembly::default();
        let settings = MqttClientConfiguration {
            client_id: Some(&config.mqtt.client_id),
            username: Some(&config.mqtt.username),
            password: Some(&config.mqtt.password),
            protocol_version: Some(MqttProtocolVersion::V5),
            mqtt5_connection_property: Some(Mqtt5ConnectionPropertyConfig {
                session_expiry_interval: Some(0),
                ..Default::default()
            }),
            server_certificate: Some(X509::pem(ca)),
            reconnect_timeout: None,
            keep_alive_interval: Some(Duration::from_secs(15)),
            network_timeout: Duration::from_secs(1),
            buffer_size: 2048,
            out_buffer_size: 2048,
            outbox_limit: Some(8192),
            ..Default::default()
        };
        let client = EspMqttClient::new_cb(
            &format!("mqtts://{}:{}", config.mqtt.host, config.mqtt.port),
            &settings,
            move |event| {
                if !cb.enabled.load(Ordering::Acquire) || cb.dead.load(Ordering::Acquire) {
                    return;
                }
                match event.payload() {
                    EventPayload::Connected(_) => cb.send(MqttEvent::Connected),
                    EventPayload::Received {
                        topic,
                        data,
                        details,
                        ..
                    } => {
                        let (total, offset) = match details {
                            Details::Complete => (data.len(), 0),
                            Details::InitialChunk(chunk) => (chunk.total_data_size, 0),
                            Details::SubsequentChunk(chunk) => {
                                (chunk.total_data_size, chunk.current_data_offset)
                            }
                        };
                        match assembly.push(topic, data, total, offset) {
                            Ok(Some((topic, data))) => cb.send(MqttEvent::Message(topic, data)),
                            Ok(None) => {}
                            Err(_) => cb.fail(),
                        }
                    }
                    EventPayload::Disconnected | EventPayload::Error(_) => cb.fail(),
                    _ => {} // SUBACK grants are checked by the raw SDK handler below.
                }
            },
        )?;
        // The safe wrapper omits retransmit timeout and SUBACK grant codes.
        // Stop before reconfiguration: no subscriptions/publications have been issued.
        sys::esp!(unsafe { sys::esp_mqtt_client_stop(client.handle()) })?;
        // SDK preserves existing URI/identity strings when their replacement is null.
        // Other settings are replaced, so retain the same TLS/session/buffer policy.
        let mut raw = sys::esp_mqtt_client_config_t::default();
        raw.broker.verification.certificate = ca.as_ptr();
        raw.broker.verification.certificate_len = ca.to_bytes_with_nul().len();
        raw.session.protocol_ver = sys::esp_mqtt_protocol_ver_t_MQTT_PROTOCOL_V_5;
        raw.session.keepalive = 15;
        raw.session.message_retransmit_timeout = 10_000;
        raw.network.disable_auto_reconnect = true;
        raw.network.timeout_ms = 1000;
        raw.buffer.size = 2048;
        raw.buffer.out_size = 2048;
        raw.outbox.limit = 8192;
        sys::esp!(unsafe { sys::esp_mqtt_set_config(client.handle(), &raw) })?;
        // Arc storage has a stable address and outlives SDK destruction.
        sys::esp!(unsafe {
            sys::esp_mqtt_client_register_event(
                client.handle(),
                sys::esp_mqtt_event_id_t_MQTT_EVENT_SUBSCRIBED,
                Some(suback),
                Arc::as_ptr(&callback) as *mut c_void,
            )
        })?;
        callback.enabled.store(true, Ordering::Release);
        sys::esp!(unsafe { sys::esp_mqtt_client_start(client.handle()) })?;
        Ok(Self {
            client,
            callback,
            receiver,
            properties: Box::default(),
        })
    }

    pub fn alive(&self) -> bool {
        !self.callback.dead.load(Ordering::Acquire)
    }
    pub fn next(&self) -> Option<MqttEvent> {
        let event = self.receiver.try_recv().ok()?;
        if let MqttEvent::Message(_, data) = &event {
            self.callback
                .queued
                .fetch_sub(data.len(), Ordering::Relaxed);
        }
        Some(event)
    }
    pub fn subscribe(&mut self, topic: &str) -> anyhow::Result<u32> {
        Ok(self.client.subscribe(topic, QoS::AtLeastOnce)?)
    }
    pub fn publish(&mut self, command: &Command, now_ms: u64) -> anyhow::Result<()> {
        anyhow::ensure!(self.alive(), "MQTT unavailable");
        let expiry = command
            .expiry_seconds(now_ms)
            .ok_or_else(|| anyhow::anyhow!("Command expired"))?;
        let payload = command.payload()?;
        self.properties.message_expiry_interval = expiry;
        // Only this owner calls the setter/publish pair. Properties remain allocated through Drop.
        sys::esp!(unsafe {
            sys::esp_mqtt5_client_set_publish_property(self.client.handle(), &*self.properties)
        })?;
        self.client
            .publish(&command.topic, QoS::AtLeastOnce, false, &payload)?;
        println!(
            "Command: sent {} (QoS 1, retain off, expiry {}s)",
            command.command, expiry
        );
        Ok(())
    }
}

unsafe extern "C" fn suback(arg: *mut c_void, _: sys::esp_event_base_t, _: i32, data: *mut c_void) {
    // SDK owns event only for this callback; registration context stays alive in Connection.
    let callback = unsafe { &*(arg as *const Callback) };
    if !callback.enabled.load(Ordering::Acquire) {
        return;
    }
    let event = unsafe { &*(data as *const sys::esp_mqtt_event_t) };
    if event.data_len != 1 || event.data.is_null() || unsafe { *event.data.cast::<u8>() } != 1 {
        callback.fail(); // Refused or downgraded subscription cannot enable controls.
    } else {
        callback.send(MqttEvent::Subscribed(event.msg_id as u32));
    }
}
