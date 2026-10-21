use crate::{device, network};
use sonos_desk_firmware::config::Config;
use std::ffi::CStr;
use std::sync::{Arc, atomic::AtomicU32, mpsc};
use std::time::Instant;

pub fn run(controls: bool) -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    let ca = CStr::from_bytes_with_nul(
        concat!(
            include_str!(concat!(env!("OUT_DIR"), "/broker-ca.local.pem")),
            "\0"
        )
        .as_bytes(),
    )?;
    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    let start = Instant::now();
    let gate = Arc::new(AtomicU32::new(0));
    let (sender, receiver) = mpsc::sync_channel(8);
    let sink = controls.then(|| device::InputSink {
        gate: gate.clone(),
        sender,
    });
    std::thread::Builder::new()
        .name("network".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            match Config::parse(include_str!(concat!(env!("OUT_DIR"), "/config.local.toml"))) {
                Ok(config) => {
                    if controls && config.room_id.is_none() {
                        println!("Controller: set room_id in local configuration");
                        return;
                    }
                    let io = controls.then_some(network::ControlIo {
                        gate,
                        receiver,
                        start,
                    });
                    if network::run(peripherals.modem, config, ca, io).is_err() {
                        println!("Network setup failed; inputs still available.");
                    }
                }
                Err(message) => println!("Network configuration: {message}"),
            }
        })?;
    device::run_inputs(peripherals.pins, start, sink)
}
