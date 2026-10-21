#[cfg(target_os = "espidf")]
mod app;
#[cfg(target_os = "espidf")]
mod device;
#[cfg(target_os = "espidf")]
mod mqtt;
#[cfg(target_os = "espidf")]
mod network;
#[cfg(target_os = "espidf")]
mod wifi;

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    app::run(false)
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    eprintln!("Build network-check for --target riscv32imac-esp-espidf; see README.md.");
    std::process::exit(1);
}
