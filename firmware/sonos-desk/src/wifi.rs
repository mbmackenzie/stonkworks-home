use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use sonos_desk_firmware::config::Wifi;

pub fn start(
    modem: Modem<'static>,
    config: &Wifi,
) -> anyhow::Result<BlockingWifi<EspWifi<'static>>> {
    let event_loop = EspSystemEventLoop::take()?;
    // None keeps Wi-Fi settings in RAM rather than storing another credential copy in NVS.
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, event_loop.clone(), None)?, event_loop)?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: config
            .ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid SSID"))?,
        password: config
            .password
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid Wi-Fi passphrase"))?,
        // ESP-IDF treats this as a minimum security threshold, not a list of modes.
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;

    Ok(wifi)
}
