use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{AnyInputPin, PinDriver, Pins, Pull},
    spi::{self, SPI2, SpiDeviceDriver},
    units::Hertz,
};
use sonos_desk_firmware::panel;
use ssd1677::Interface;
use std::thread;
use std::time::Duration;

pub fn run(
    spi: SPI2<'static>,
    pins: Pins,
    load: impl FnOnce() -> anyhow::Result<panel::Frame>,
) -> anyhow::Result<()> {
    let mut power = PinDriver::output(pins.gpio4)?;
    power.set_low()?;
    let result = (|| -> anyhow::Result<()> {
        let mut frame = load()?; // Fetch/decode with panel power held off.
        let mut reset = PinDriver::output(pins.gpio22)?;
        reset.set_low()?;
        let mut dc = PinDriver::output(pins.gpio20)?;
        dc.set_low()?;
        let busy = PinDriver::input(pins.gpio14, Pull::Down)?;
        let spi = SpiDeviceDriver::new_single(
            spi,
            pins.gpio19,
            pins.gpio18,
            None::<AnyInputPin>,
            Some(pins.gpio21),
            &spi::config::DriverConfig::new(),
            &spi::config::Config::new().baudrate(Hertz(1_000_000)),
        )?;
        println!("Display: power on; SPI mode 0 at 1 MHz");
        power.set_high()?;
        thread::sleep(Duration::from_millis(100));
        // Exact HAT reset timing from Waveshare, rather than the generic driver's reset.
        reset.set_high()?;
        thread::sleep(Duration::from_millis(20));
        reset.set_low()?;
        thread::sleep(Duration::from_millis(2));
        reset.set_high()?;
        thread::sleep(Duration::from_millis(20));
        let mut io = Interface::new(spi, dc, reset, busy);
        io.set_busy_timeout(15_000);
        println!("Display: drawing one frame; BUSY timeout 15 seconds per wait");
        panel::show_frame(&mut io, &mut frame, &mut FreeRtos)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        println!("Display: refresh finished; sleep command sent");
        Ok(())
    })();
    power.set_low()?;
    match result {
        Ok(()) => println!("Display: power off; image retained"),
        Err(error) => println!("Display: stopped: {error}; power off; check wiring before reset"),
    }
    // Hold PWR low. No network, Sonos commands, or automatic refresh/retry loop.
    loop {
        thread::park();
    }
}
