use serde::Deserialize;

// Deliberately no Debug implementation: configuration contains passwords.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub wifi: Wifi,
    pub mqtt: Mqtt,
    pub ntp_server: String,
    pub room_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wifi {
    pub ssid: String,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mqtt {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub client_id: String,
    pub instance: String,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        if text.len() > 8192 {
            return Err("Configuration exceeds 8 KiB");
        }
        // TOML errors can quote the input, so return only our fixed message.
        let config: Self = toml::from_str(text).map_err(|_| "Invalid configuration TOML")?;
        if config.wifi.ssid.is_empty()
            || config.wifi.ssid.len() > 32
            || config.wifi.ssid.contains('\0')
        {
            return Err("Wi-Fi SSID must contain 1–32 bytes without NUL");
        }
        if !(8..=63).contains(&config.wifi.password.len())
            || !config.wifi.password.is_ascii()
            || config.wifi.password.contains('\0')
        {
            return Err("Wi-Fi requires an 8–63 byte ASCII WPA2/WPA3 passphrase");
        }
        if !hostname(&config.mqtt.host) || !hostname(&config.ntp_server) {
            return Err("Broker and NTP settings require a DNS hostname or IPv4 address");
        }
        if config.mqtt.port == 0 {
            return Err("MQTT TLS port must be nonzero");
        }
        if config.room_id.as_ref().is_some_and(|id| !identifier(id)) {
            return Err("Room ID requires 1–64 letters, digits, underscores or hyphens");
        }
        if !identifier(&config.mqtt.client_id) || !identifier(&config.mqtt.instance) {
            return Err(
                "Client ID and instance require 1–64 letters, digits, underscores or hyphens",
            );
        }
        if [&config.mqtt.username, &config.mqtt.password]
            .iter()
            .any(|value| value.is_empty() || value.len() > 1024 || value.contains('\0'))
        {
            return Err("MQTT credentials must contain 1–1024 bytes without NUL");
        }
        Ok(config)
    }
}

fn identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn hostname(value: &str) -> bool {
    (1..=253).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}
