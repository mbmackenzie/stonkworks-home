#[cfg(target_os = "espidf")]
mod device;

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    device::run_inputs(peripherals.pins)
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    eprintln!("Build input-check for --target riscv32imac-esp-espidf; see README.md.");
    std::process::exit(1);
}
