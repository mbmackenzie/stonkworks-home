use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
    mpsc::SyncSender,
};
use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::gpio::{PinDriver, Pins, Pull};
use sonos_desk_firmware::inputs::{InputEvent, Inputs, Levels};

pub struct InputSink {
    pub gate: Arc<AtomicU32>,
    pub sender: SyncSender<(u32, u64, InputEvent)>,
}

pub fn run_inputs(pins: Pins, start: Instant, sink: Option<InputSink>) -> anyhow::Result<()> {
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
    let mut inputs = Inputs::new(read());
    let mut generation = 0;
    println!(
        "Inputs: ready; {}",
        if sink.is_some() {
            "commands require fresh online state"
        } else {
            "logging only; no Sonos commands"
        }
    );

    loop {
        let now_ms = start.elapsed().as_millis() as u64;
        let current = sink.as_ref().map_or(0, |s| s.gate.load(Ordering::Acquire));
        let levels = read();
        if current != generation {
            inputs.disarm(levels);
            generation = current;
        }
        for input in inputs.sample(levels, now_ms) {
            println!("{now_ms:>8} ms  {input:?}");
            if let Some(sink) = &sink
                && generation != 0
                && sink.sender.try_send((generation, now_ms, input)).is_err()
            {
                println!("Input: busy; dropped");
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
}
