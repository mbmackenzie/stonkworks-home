use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
    mpsc::SyncSender,
};
use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::gpio::{self, PinDriver, Pins, Pull};

pub struct InputPins {
    pub previous: gpio::Gpio3<'static>,
    pub play_pause: gpio::Gpio5<'static>,
    pub next: gpio::Gpio17<'static>,
    pub mute: gpio::Gpio2<'static>,
    pub a: gpio::Gpio0<'static>,
    pub b: gpio::Gpio1<'static>,
}

use sonos_desk_firmware::{
    inputs::{InputEvent, Inputs, Levels},
    link::DisplayLink,
};

pub struct InputSink {
    pub gate: Arc<AtomicU32>,
    pub sender: SyncSender<(u32, u64, InputEvent)>,
    pub link: Arc<DisplayLink>,
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

    run_control_inputs(
        InputPins {
            previous: pins.gpio3,
            play_pause: pins.gpio5,
            next: pins.gpio17,
            mute: pins.gpio2,
            a: pins.gpio0,
            b: pins.gpio1,
        },
        start,
        sink,
    )
}

pub fn run_control_inputs(
    pins: InputPins,
    start: Instant,
    sink: Option<InputSink>,
) -> anyhow::Result<()> {
    // Pull-ups hold inputs high until a switch connects them to ground.
    let previous = PinDriver::input(pins.previous, Pull::Up)?;
    let play_pause = PinDriver::input(pins.play_pause, Pull::Up)?;
    let next = PinDriver::input(pins.next, Pull::Up)?;
    let mute = PinDriver::input(pins.mute, Pull::Up)?;
    let a = PinDriver::input(pins.a, Pull::Up)?;
    let b = PinDriver::input(pins.b, Pull::Up)?;

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
            let Some(sink) = &sink else { continue };
            sink.link.bump_activity();
            if input == InputEvent::Refresh {
                sink.link.bump_force(); // Local to the display; the controller never sees it.
                continue;
            }
            if generation != 0 && sink.sender.try_send((generation, now_ms, input)).is_err() {
                println!("Input: busy; dropped");
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
}
