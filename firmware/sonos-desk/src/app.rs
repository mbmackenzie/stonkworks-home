use crate::{device, network};
use sonos_desk_firmware::{config::Config, link::DisplayLink};
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
    let link = Arc::new(DisplayLink::default());
    let (sender, receiver) = mpsc::sync_channel(8);
    let sink = controls.then(|| device::InputSink {
        gate: gate.clone(),
        sender,
        link: link.clone(),
    });
    let network_gate = gate.clone();
    let network_link = link.clone();
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
                        gate: network_gate,
                        link: network_link,
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
    #[cfg(feature = "controller")]
    if controls {
        let p = peripherals.pins;
        let input_pins = device::InputPins {
            previous: p.gpio3,
            play_pause: p.gpio5,
            next: p.gpio17,
            mute: p.gpio2,
            a: p.gpio0,
            b: p.gpio1,
        };
        let display_pins = crate::display::DisplayPins {
            power: p.gpio4,
            reset: p.gpio22,
            dc: p.gpio20,
            busy: p.gpio14,
            clock: p.gpio19,
            data: p.gpio18,
            cs: p.gpio21,
        };
        std::thread::Builder::new()
            .name("display".into())
            .stack_size(24 * 1024)
            .spawn(move || {
                let result = (|| -> anyhow::Result<()> {
                    let panel = crate::display::Panel::new(peripherals.spi2, display_pins)?;
                    let config =
                        Config::parse(include_str!(concat!(env!("OUT_DIR"), "/config.local.toml")))
                            .map_err(|message| anyhow::anyhow!("{message}"))?;
                    if config.http_base_url.is_none() {
                        println!("Display: no http_base_url configured; disabled");
                        loop {
                            std::thread::park();
                        }
                    }
                    crate::updater::run(panel, config, gate, link);
                    Ok(())
                })();
                if result.is_err() {
                    println!("Display: setup failed; controls continue");
                }
            })?;
        return device::run_control_inputs(input_pins, start, sink);
    }
    device::run_inputs(peripherals.pins, start, sink)
}
