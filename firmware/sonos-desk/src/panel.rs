//! Waveshare 3.97-inch monochrome sequence; see third-party/Waveshare.txt.
//! SPI/DC/BUSY handling comes from ssd1677; panel settings follow EPD_3in97.c.
use embedded_hal::delay::DelayNs;
use ssd1677::DisplayInterface;

pub fn show_pattern<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    delay: &mut D,
) -> Result<(), I::Error> {
    show_frame(io, &pattern(), delay)
}

pub type Frame = Box<[u8; 48_000]>;

/// Pixel box in frame orientation; x values are multiples of 8, ends are exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x0: u16,
    pub x1: u16,
    pub y0: u16,
    pub y1: u16,
}

/// One row of panel RAM, in the order sent. Calibrated to the user's mounting:
/// 180° means reverse byte and bit order, so stream row `r` is frame row `479 - r`.
fn ram_row(frame: &[u8; 48_000], row: usize) -> [u8; 100] {
    let source = &frame[(479 - row) * 100..][..100];
    std::array::from_fn(|column| source[99 - column].reverse_bits())
}

/// Initialize the controller and fill both RAM planes without refreshing.
pub fn load_planes<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    frame: &[u8; 48_000],
    delay: &mut D,
) -> Result<(), I::Error> {
    io.busy_wait(delay)?;
    io.send_command(0x12)?; // Software reset, after the board adapter's hardware reset.
    io.busy_wait(delay)?;
    for (command, data) in [
        (0x18, &[0x80][..]), // Internal temperature sensor.
        (0x0c, &[0xae, 0xc7, 0xc3, 0xc0, 0x80]),
        (0x01, &[0xdf, 0x01, 0x02]), // 480 rows; vendor gate scan direction.
        (0x3c, &[0x01]),
        (0x11, &[0x01]),                   // X increment, Y decrement.
        (0x44, &[0x00, 0x00, 0x1f, 0x03]), // X 0..799, pixel addressing.
        (0x45, &[0xdf, 0x01, 0x00, 0x00]), // Y 479..0.
        (0x4e, &[0x00, 0x00]),
        (0x4f, &[0x00, 0x00]), // Vendor's initial Y counter.
    ] {
        io.send_command(command)?;
        io.send_data(data)?;
    }
    io.busy_wait(delay)?;
    for command in [0x24, 0x26] {
        io.send_command(command)?;
        for row in 0..480 {
            io.send_data(&ram_row(frame, row))?;
        }
    }
    Ok(())
}

pub fn show_frame<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    frame: &[u8; 48_000],
    delay: &mut D,
) -> Result<(), I::Error> {
    load_planes(io, frame, delay)?;
    io.send_command(0x22)?;
    io.send_data(&[0xf7])?; // Full refresh using the panel's OTP waveform.
    io.send_command(0x20)?;
    // Allow BUSY to assert before the library polls it.
    delay.delay_ms(10);
    io.busy_wait(delay)?;
    io.send_command(0x10)?;
    io.send_data(&[0x01])?;
    delay.delay_ms(100);
    Ok(())
}

fn set_window<I: DisplayInterface>(io: &mut I, region: Region) -> Result<(), I::Error> {
    // RAM x is mirrored (800 - x) and RAM y equals frame y.
    let (xs, xe) = ((800 - region.x1) / 8, (800 - region.x0) / 8);
    let (xs_px, xe_px) = (xs * 8, (xe - 1) * 8);
    let (y0, y1) = (region.y0, region.y1 - 1);
    for (command, data) in [
        (0x11, &[0x03][..]), // X and Y increment, as after the vendor's reset.
        (
            0x44,
            &[
                xs_px as u8,
                (xs_px >> 8) as u8,
                xe_px as u8,
                (xe_px >> 8) as u8,
            ],
        ),
        (
            0x45,
            &[y0 as u8, (y0 >> 8) as u8, y1 as u8, (y1 >> 8) as u8],
        ),
        (0x4e, &[xs_px as u8, (xs_px >> 8) as u8]),
        (0x4f, &[y0 as u8, (y0 >> 8) as u8]),
    ] {
        io.send_command(command)?;
        io.send_data(data)?;
    }
    Ok(())
}

/// Send `region` of `frame` to one RAM plane, window already set.
fn write_window<I: DisplayInterface>(
    io: &mut I,
    command: u8,
    frame: &[u8; 48_000],
    region: Region,
) -> Result<(), I::Error> {
    let (xs, xe) = (
        usize::from((800 - region.x1) / 8),
        usize::from((800 - region.x0) / 8),
    );
    io.send_command(command)?;
    for y in region.y0..region.y1 {
        // Stream row 479 - y holds frame row y; send only the window's bytes.
        io.send_data(&ram_row(frame, 479 - usize::from(y))[xs..xe])?;
    }
    Ok(())
}

/// Partial refresh of `region` using Waveshare's `EPD_3in97_Display_Partial` sequence.
/// The board adapter must hardware-reset the panel first; the panel stays awake after.
///
/// This controller swaps its two RAM planes after every refresh. Writing only the
/// changed window leaves everything outside it with "old" in the plane that now counts
/// as new, so the next partial would draw the previous window back (hardware experiment,
/// 2026-10-21). Writing the new window into both planes afterwards keeps them equal.
pub fn show_partial<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    old: &[u8; 48_000],
    new: &[u8; 48_000],
    region: Region,
    delay: &mut D,
) -> Result<(), I::Error> {
    io.busy_wait(delay)?;
    for (command, data) in [(0x18, 0x80), (0x3c, 0x80)] {
        io.send_command(command)?;
        io.send_data(&[data])?;
    }
    for (command, frame) in [(0x26, old), (0x24, new)] {
        set_window(io, region)?; // Each write moves the address counters.
        write_window(io, command, frame, region)?;
    }
    io.send_command(0x22)?;
    io.send_data(&[0xff])?; // Vendor partial mode.
    io.send_command(0x20)?;
    delay.delay_ms(10);
    io.busy_wait(delay)?;
    for command in [0x26, 0x24] {
        set_window(io, region)?;
        write_window(io, command, new, region)?;
    }
    Ok(())
}

pub fn pattern() -> Frame {
    let mut frame = vec![0xff; 48_000];
    for y in 0_usize..480 {
        for x in 0_usize..800 {
            let border = !(8..792).contains(&x) || !(8..472).contains(&y);
            let solid = (24..88).contains(&x) && (24..88).contains(&y);
            let hollow = (712..776).contains(&x)
                && (24..88).contains(&y)
                && (!(720..768).contains(&x) || !(32..80).contains(&y));
            let bars = (24..88).contains(&x)
                && x % 24 < 16
                && (400 + ((x - 24) / 24) * 16..456).contains(&y);
            let checker =
                (712..776).contains(&x) && (392..456).contains(&y) && (x / 8 + y / 8) % 2 == 0;
            let arrow = ((200..600).contains(&x) && (230..250).contains(&y))
                || ((560..640).contains(&x) && y.abs_diff(240) <= (640 - x) / 2);
            if border || solid || hollow || bars || checker || arrow {
                frame[y * 100 + x / 8] &= !(0x80 >> (x % 8));
            }
        }
    }
    frame
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!())
}
