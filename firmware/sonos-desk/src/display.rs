use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{self, AnyInputPin, PinDriver, Pins, Pull},
    spi::{self, SPI2, SpiDeviceDriver},
    units::Hertz,
};
use sonos_desk_firmware::panel;
use ssd1677::Interface;
use std::thread;
use std::time::Duration;

/// Hardware comparison (2026-10-21): more passes did not reduce text ghosting.
const PARTIAL_PASSES: u8 = 1;

pub struct DisplayPins {
    pub power: gpio::Gpio4<'static>,
    pub reset: gpio::Gpio22<'static>,
    pub dc: gpio::Gpio20<'static>,
    pub busy: gpio::Gpio14<'static>,
    pub clock: gpio::Gpio19<'static>,
    pub data: gpio::Gpio18<'static>,
    pub cs: gpio::Gpio21<'static>,
}

impl From<Pins> for DisplayPins {
    fn from(p: Pins) -> Self {
        Self {
            power: p.gpio4,
            reset: p.gpio22,
            dc: p.gpio20,
            busy: p.gpio14,
            clock: p.gpio19,
            data: p.gpio18,
            cs: p.gpio21,
        }
    }
}

#[allow(dead_code)] // One-shot diagnostic adapter; controller uses Panel directly.
pub fn run(
    spi: SPI2<'static>,
    pins: Pins,
    load: impl FnOnce() -> anyhow::Result<panel::Frame>,
) -> anyhow::Result<()> {
    let mut display = Panel::new(spi, pins.into())?;
    let result = load().and_then(|frame| display.draw(&frame));
    if let Err(error) = result {
        println!("Display: stopped: {error}");
    }
    loop {
        thread::park();
    }
}

pub struct Panel {
    power: PinDriver<'static, gpio::Output>,
    reset: PinDriver<'static, gpio::Output>,
    dc: PinDriver<'static, gpio::Output>,
    busy: PinDriver<'static, gpio::Input>,
    spi: SpiDeviceDriver<'static, spi::SpiDriver<'static>>,
    powered: bool,
}

impl Panel {
    pub fn new(spi: SPI2<'static>, pins: DisplayPins) -> anyhow::Result<Self> {
        let mut power = PinDriver::output(pins.power)?;
        power.set_low()?;

        let mut reset = PinDriver::output(pins.reset)?;
        reset.set_low()?;
        let mut dc = PinDriver::output(pins.dc)?;
        dc.set_low()?;
        let busy = PinDriver::input(pins.busy, Pull::Down)?;
        let spi = SpiDeviceDriver::new_single(
            spi,
            pins.clock,
            pins.data,
            None::<AnyInputPin>,
            Some(pins.cs),
            &spi::config::DriverConfig::new(),
            &spi::config::Config::new().baudrate(Hertz(1_000_000)),
        )?;
        Ok(Self {
            power,
            reset,
            dc,
            busy,
            spi,
            powered: false,
        })
    }

    fn power_up(&mut self) -> anyhow::Result<()> {
        if !self.powered {
            println!("Display: power on; SPI mode 0 at 1 MHz");
            self.power.set_high()?;
            thread::sleep(Duration::from_millis(100));
            self.powered = true;
        }
        Ok(())
    }

    fn power_down(&mut self) -> anyhow::Result<()> {
        self.powered = false;
        self.power.set_low()?;
        Ok(())
    }

    /// Exact HAT reset timing from Waveshare, rather than the generic driver's reset.
    fn reset_pulse(&mut self) -> anyhow::Result<()> {
        self.reset.set_high()?;
        thread::sleep(Duration::from_millis(20));
        self.reset.set_low()?;
        thread::sleep(Duration::from_millis(2));
        self.reset.set_high()?;
        thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    /// Partially refresh `region`; `old` is the frame the panel shows, `new` replaces it.
    /// PWR stays high after every successful draw: the panel sleeps with its RAM retained
    /// (deep sleep mode 1), the one configuration the hardware spike verified clean. Any
    /// failure powers off, and the worker then stops drawing until reset.
    #[allow(dead_code)] // The one-shot diagnostics share this file but only draw in full.
    pub fn draw_partial(
        &mut self,
        old: &[u8; 48_000],
        new: &[u8; 48_000],
        region: panel::Region,
    ) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let result = (|| -> anyhow::Result<()> {
            self.power_up()?;
            self.reset_pulse()?;
            let mut io =
                Interface::new(&mut self.spi, &mut self.dc, &mut self.reset, &mut self.busy);
            io.set_busy_timeout(15_000);
            panel::show_partial(&mut io, old, new, region, PARTIAL_PASSES, &mut FreeRtos)
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            println!(
                "Display: partial refresh finished in {} ms",
                started.elapsed().as_millis()
            );
            Ok(())
        })();
        if result.is_err() {
            self.power_down()?;
        }
        result
    }

    /// A full refresh with the panel's normal (slow, thorough) waveform.
    pub fn draw(&mut self, frame: &[u8; 48_000]) -> anyhow::Result<()> {
        self.draw_with(frame, false)
    }

    /// A full refresh; `fast` uses the shorter waveform that flashes fewer times.
    pub fn draw_with(&mut self, frame: &[u8; 48_000], fast: bool) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let result = (|| -> anyhow::Result<()> {
            self.power_up()?;
            self.reset_pulse()?;
            let mut io =
                Interface::new(&mut self.spi, &mut self.dc, &mut self.reset, &mut self.busy);
            io.set_busy_timeout(15_000);
            println!("Display: drawing one frame; BUSY timeout 15 seconds per wait");
            panel::show_frame_with(&mut io, frame, fast, &mut FreeRtos)
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            println!(
                "Display: {} refresh finished in {} ms; sleep command sent",
                if fast { "fast" } else { "full" },
                started.elapsed().as_millis()
            );
            Ok(())
        })();
        if result.is_err() {
            self.power_down()?;
        } else {
            println!("Display: image retained; PWR stays high, panel asleep");
        }
        result
    }
}
