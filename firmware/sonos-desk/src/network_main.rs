#[cfg(target_os = "espidf")]
mod device;
#[cfg(target_os = "espidf")]
mod network;

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    use sonos_desk_firmware::config::Config;
    use std::ffi::CStr;

    esp_idf_svc::sys::link_patches();
    let ca = CStr::from_bytes_with_nul(
        concat!(
            include_str!(concat!(env!("OUT_DIR"), "/broker-ca.local.pem")),
            "\0"
        )
        .as_bytes(),
    )?;
    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;

    // Networking can wait on the LAN without stopping the GPIO sampling loop.
    std::thread::Builder::new()
        .name("network".into())
        .stack_size(12 * 1024)
        .spawn(move || {
            match Config::parse(include_str!(concat!(env!("OUT_DIR"), "/config.local.toml"))) {
                Ok(config) => {
                    if network::run(peripherals.modem, config, ca).is_err() {
                        println!("Network setup failed; inputs still available.");
                    }
                }
                Err(message) => println!("Network configuration: {message}"),
            }
        })?;
    device::run_inputs(peripherals.pins)
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    eprintln!("Build network-check for --target riscv32imac-esp-espidf; see README.md.");
    std::process::exit(1);
}
