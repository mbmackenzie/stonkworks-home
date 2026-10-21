#[cfg(target_os = "espidf")]
mod display;

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    // Keep display setup and buffers away from ESP-IDF's small main-task stack.
    std::thread::Builder::new()
        .name("display-check".into())
        .stack_size(12 * 1024)
        .spawn(move || {
            if let Err(error) = display::run(peripherals.spi2, peripherals.pins, || {
                Ok(sonos_desk_firmware::panel::pattern())
            }) {
                println!("Display setup failed: {error}");
            }
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("Display worker failed"))?;
    Ok(())
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    eprintln!("Build display-check for --target riscv32imac-esp-espidf; see README.md.");
    std::process::exit(1);
}
