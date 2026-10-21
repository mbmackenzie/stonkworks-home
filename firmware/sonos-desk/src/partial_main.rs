//! Spike only (throwaway): measure partial refreshes on the 3.97-inch panel.
#[cfg(target_os = "espidf")]
mod display;

#[cfg(target_os = "espidf")]
mod spike {
    use crate::display::{DisplayPins, Panel};
    use std::{thread, time::{Duration, Instant}};

    type Frame = Box<[u8; 48_000]>;

    fn black(frame: &mut Frame, x: usize, y: usize) {
        frame[y * 100 + x / 8] &= !(0x80 >> (x % 8));
    }
    fn fill(frame: &mut Frame, x0: usize, x1: usize, y0: usize, y1: usize) {
        for y in y0..y1 {
            for x in x0..x1 {
                black(frame, x, y);
            }
        }
    }

    /// A stand-in for the Sonos screen: art, text bars, and a volume bar.
    fn render(volume: usize, title: usize, art: usize) -> Frame {
        const BAYER: [[usize; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
        let mut frame: Frame = vec![0xff; 48_000].into_boxed_slice().try_into().unwrap();
        fill(&mut frame, 410, 770, 22, 36); // Transport label.
        for y in 76..436 {
            for x in 24..384 {
                let level = ((x - 24 + (y - 76) * (art + 1)) * (3 + art) / 4) % 360 * 16 / 360;
                if level <= BAYER[y % 4][x % 4] {
                    black(&mut frame, x, y);
                }
            }
        }
        for (line, y) in [(0, 100), (1, 140), (2, 180)] {
            fill(&mut frame, 410, 410 + 120 + (title * 53 + line * 71) % 230, y, y + 20);
        }
        fill(&mut frame, 410, 770, 423, 425); // Volume outline.
        fill(&mut frame, 410, 770, 433, 435);
        fill(&mut frame, 410, 412, 423, 435);
        fill(&mut frame, 768, 770, 423, 435);
        fill(&mut frame, 412, 412 + 355 * volume / 100, 425, 433);
        frame
    }

    fn partial(panel: &mut Panel, old: &Frame, new: &Frame, window: (u16, u16, u16, u16), variant: u8, name: &str) {
        let start = Instant::now();
        let result = panel.spike_partial(old, new, window, variant);
        println!("Spike: {name}: {:?} in {} ms", result, start.elapsed().as_millis());
    }

    pub fn run(panel: &mut Panel) {
        const VOLUME: (u16, u16, u16, u16) = (408, 776, 421, 437);
        const TITLE: (u16, u16, u16, u16) = (408, 776, 90, 230);
        const ART: (u16, u16, u16, u16) = (24, 384, 76, 436);
        let pause = |seconds| thread::sleep(Duration::from_secs(seconds));
        for variant in 0..4_u8 {
            println!("Spike: VARIANT {variant} (bit0 = old plane in 0x26, bit1 = no reset when awake)");
            let mut shown = render(20, 0, 0);
            let start = Instant::now();
            let mut full = shown.clone();
            println!("Spike: full draw: {:?} in {} ms", panel.spike_full(&mut full), start.elapsed().as_millis());
            pause(8);
            let mut volume = 20;
            for _ in 0..8 {
                volume += 10;
                let next = render(volume, 0, 0);
                partial(panel, &shown, &next, VOLUME, variant, &format!("volume {volume}"));
                shown = next;
                pause(3);
            }
            let next = render(volume, 1, 0);
            partial(panel, &shown, &next, TITLE, variant, "title text");
            shown = next;
            pause(5);
            let next = render(volume, 1, 1);
            partial(panel, &shown, &next, ART, variant, "art 360x360");
            shown = next;
            pause(6);
            for step in 0..20 {
                let next = render(if step % 2 == 0 { 30 } else { 70 }, 1, 1);
                partial(panel, &shown, &next, VOLUME, variant, &format!("burst {step}"));
                shown = next;
                pause(1);
            }
            println!("Spike: variant {variant} finished; next full draw shows ghosting cleanup");
            pause(10);
        }
        println!("Spike: all variants finished");
    }

    pub fn start(spi: esp_idf_svc::hal::spi::SPI2<'static>, pins: DisplayPins) -> anyhow::Result<()> {
        let mut panel = Panel::new(spi, pins)?;
        run(&mut panel);
        loop {
            thread::park();
        }
    }
}

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    std::thread::Builder::new()
        .name("partial-check".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            if let Err(error) = spike::start(peripherals.spi2, peripherals.pins.into()) {
                println!("Spike setup failed: {error}");
            }
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("Spike worker failed"))?;
    Ok(())
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    eprintln!("Build partial-check for --target riscv32imac-esp-espidf; see README.md.");
    std::process::exit(1);
}
