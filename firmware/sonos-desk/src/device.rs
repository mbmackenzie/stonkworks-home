use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::gpio::{PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use sonos_desk_firmware::inputs::{Inputs, Levels};

pub fn run_inputs() -> anyhow::Result<()> {
    let pins = Peripherals::take()?.pins;

    // Keep the display off. PWR is active-high in Waveshare's driver.
    // Hold the drivers for the lifetime of this loop; dropping resets a pin.
    let mut power = PinDriver::output(pins.gpio4)?;
    power.set_low()?;
    let mut reset = PinDriver::output(pins.gpio22)?;
    reset.set_low()?;
    let mut data = PinDriver::output(pins.gpio18)?;
    data.set_low()?;
    let mut clock = PinDriver::output(pins.gpio19)?;
    clock.set_low()?;
    let mut dc = PinDriver::output(pins.gpio20)?;
    dc.set_low()?;
    let mut cs = PinDriver::output(pins.gpio21)?;
    cs.set_high()?;
    let _busy = PinDriver::input(pins.gpio14, Pull::Floating)?;

    // Pull-ups hold inputs high until a switch connects them to ground.
    let previous = PinDriver::input(pins.gpio3, Pull::Up)?;
    let play_pause = PinDriver::input(pins.gpio5, Pull::Up)?;
    let next = PinDriver::input(pins.gpio17, Pull::Up)?;
    let mute = PinDriver::input(pins.gpio2, Pull::Up)?;
    let a = PinDriver::input(pins.gpio0, Pull::Up)?;
    let b = PinDriver::input(pins.gpio1, Pull::Up)?;

    let read = || Levels {
        previous: previous.is_low(),
        play_pause: play_pause.is_low(),
        next: next.is_low(),
        mute: mute.is_low(),
        a: a.is_high(),
        b: b.is_high(),
    };
    let start = Instant::now();
    let mut inputs = Inputs::new(read());
    println!("Input check ready: buttons and encoder only; no Sonos commands.");

    loop {
        let now_ms = start.elapsed().as_millis() as u64;
        for event in inputs.sample(read(), now_ms) {
            println!("{now_ms:>8} ms  {event:?}");
        }
        thread::sleep(Duration::from_millis(1));
    }
}
