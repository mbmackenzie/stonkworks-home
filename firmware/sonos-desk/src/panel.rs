//! Waveshare 3.97-inch monochrome sequence; see third-party/Waveshare.txt.
//! SPI/DC/BUSY handling comes from ssd1677; panel settings follow EPD_3in97.c.
use embedded_hal::delay::DelayNs;
use ssd1677::DisplayInterface;

pub fn show_pattern<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    delay: &mut D,
) -> Result<(), I::Error> {
    show_frame(io, &mut pattern(), delay)
}

pub type Frame = Box<[u8; 48_000]>;

pub fn show_frame<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    frame: &mut [u8; 48_000],
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
    // Calibrated to the user's mounting: 180° means reverse byte and bit order.
    frame.reverse();
    for byte in frame.iter_mut() {
        *byte = byte.reverse_bits();
    }
    for command in [0x24, 0x26] {
        io.send_command(command)?;
        for row in frame.chunks(100) {
            io.send_data(row)?;
        }
    }
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

/// Spike only (throwaway): Waveshare `EPD_3in97_Display_Partial` sequence.
/// `window` is `(x0, x1, y0, y1)` in frame pixels, x multiples of 8, exclusive ends.
/// `variant` bit 0 also writes `old` to RAM 0x26. The caller owns the hardware reset.
pub fn show_partial_experiment<I: DisplayInterface, D: DelayNs>(
    io: &mut I,
    old: &[u8; 48_000],
    new: &[u8; 48_000],
    window: (u16, u16, u16, u16),
    variant: u8,
    delay: &mut D,
) -> Result<(), I::Error> {
    let (x0, x1, y0, y1) = window;
    // The full draw mirrors x (byte order and bits) and keeps y, so RAM x is 800 - frame x.
    let (xs, xe) = ((800 - x1) / 8, (800 - x0) / 8);
    let ram = |frame: &[u8; 48_000]| {
        let mut bytes = Vec::with_capacity(usize::from((xe - xs) * (y1 - y0)));
        for y in y0..y1 {
            for column in (x0 / 8..x1 / 8).rev() {
                bytes.push(frame[usize::from(y) * 100 + usize::from(column)].reverse_bits());
            }
        }
        bytes
    };
    let set_window = |io: &mut I| -> Result<(), I::Error> {
        let (xs_px, xe_px) = (xs * 8, (xe - 1) * 8);
        for (command, data) in [
            (0x11, &[0x03][..]), // X and Y increment, as after the vendor's reset.
            (0x44, &[xs_px as u8, (xs_px >> 8) as u8, xe_px as u8, (xe_px >> 8) as u8][..]),
            (0x45, &[y0 as u8, (y0 >> 8) as u8, (y1 - 1) as u8, ((y1 - 1) >> 8) as u8]),
            (0x4e, &[xs_px as u8, (xs_px >> 8) as u8]),
            (0x4f, &[y0 as u8, (y0 >> 8) as u8]),
        ] {
            io.send_command(command)?;
            io.send_data(data)?;
        }
        Ok(())
    };
    io.busy_wait(delay)?;
    for (command, data) in [(0x18, 0x80), (0x3c, 0x80)] {
        io.send_command(command)?;
        io.send_data(&[data])?;
    }
    if variant & 1 != 0 {
        set_window(io)?;
        io.send_command(0x26)?;
        for row in ram(old).chunks(100) {
            io.send_data(row)?;
        }
    }
    set_window(io)?;
    io.send_command(0x24)?;
    for row in ram(new).chunks(100) {
        io.send_data(row)?;
    }
    io.send_command(0x22)?;
    io.send_data(&[0xff])?; // Vendor partial display mode.
    io.send_command(0x20)?;
    delay.delay_ms(10);
    io.busy_wait(delay)
}
