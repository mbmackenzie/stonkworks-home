#[cfg(target_os = "espidf")]
mod display;
#[cfg(target_os = "espidf")]
mod frame_http;
#[cfg(target_os = "espidf")]
mod wifi;

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    use sonos_desk_firmware::{config::Config, frame};
    esp_idf_svc::sys::link_patches();
    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    std::thread::Builder::new()
        .name("frame-check".into())
        .stack_size(24 * 1024)
        .spawn(move || {
            let result = display::run(peripherals.spi2, peripherals.pins, || {
                let config =
                    Config::parse(include_str!(concat!(env!("OUT_DIR"), "/config.local.toml")))
                        .map_err(|message| anyhow::anyhow!("{message}"))?;
                let mut frame = sonos_desk_firmware::panel::pattern(); // Reused as the receive buffer.
                let png = {
                    let mut wifi = wifi::start(peripherals.modem, &config.wifi)?;
                    wifi.connect()?;
                    wifi.wait_netif_up()?;
                    let response = frame_http::fetch_connected(&config, None, &mut frame)?;
                    drop(response.etag);
                    match response.body {
                        frame_http::Body::Bitmap => None,
                        frame_http::Body::Png(bytes) => Some(bytes),
                        frame_http::Body::Unchanged => anyhow::bail!("Unexpected HTTP 304"),
                    }
                }; // Close Wi-Fi before decoding the diagnostic frame.
                if let Some(bytes) = png {
                    frame::decode_png_into(&bytes, &mut frame)
                        .map_err(|message| anyhow::anyhow!("{message}"))?;
                }
                println!("Frame: validated 800x480 mono frame; ready for display");
                Ok(frame)
            });
            if let Err(error) = result {
                println!("Frame setup failed: {error}");
            }
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("Frame worker failed"))?;
    Ok(())
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    eprintln!("Build frame-check for --target riscv32imac-esp-espidf; see README.md.");
    std::process::exit(1);
}
