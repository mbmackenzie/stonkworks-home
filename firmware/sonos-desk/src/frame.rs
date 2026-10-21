//! Decode the existing mono HTTP contract into one packed panel buffer.
use crate::panel::Frame;
use std::io::{BufReader, Cursor};

pub const MAX_PNG_BYTES: usize = 128 * 1024;

pub fn decode_png(bytes: &[u8]) -> Result<Frame, &'static str> {
    if bytes.len() > MAX_PNG_BYTES {
        return Err("PNG exceeds 128 KiB");
    }
    let mut decoder = png::Decoder::new_with_limits(
        BufReader::with_capacity(1024, Cursor::new(bytes)),
        png::Limits { bytes: 64 * 1024 },
    );
    decoder.set_decode_limits(
        png::DecodeLimits::default()
            .with_max_width(800)
            .with_max_height(480)
            .with_max_pixels(800 * 480)
            .with_max_alloc_bytes(64 * 1024)
            .with_max_chunk_len(MAX_PNG_BYTES as u32)
            .with_max_unknown_chunk_bytes(0)
            .with_max_frames(1),
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    // Check dimensions before read_info allocates its row/decompression buffers.
    let info = decoder
        .read_header_info()
        .map_err(|_| "Invalid PNG header")?;
    if info.width != 800
        || info.height != 480
        || info.bit_depth != png::BitDepth::Eight
        || info.color_type != png::ColorType::Grayscale
        || info.interlaced
    {
        return Err("Expected non-interlaced 800x480 grayscale8 PNG");
    }
    let mut reader = decoder.read_info().map_err(|_| "Invalid PNG")?;
    if reader.info().animation_control.is_some() {
        return Err("Animated PNG is unsupported");
    }
    let mut frame = vec![0xff; 48_000];
    for y in 0..480 {
        let row = reader
            .next_row()
            .map_err(|_| "Invalid PNG pixels")?
            .ok_or("Incomplete PNG")?;
        if row.data().len() != 800 {
            return Err("Invalid PNG row length");
        }
        for (x, &pixel) in row.data().iter().enumerate() {
            match pixel {
                0 => frame[y * 100 + x / 8] &= !(0x80 >> (x % 8)),
                255 => {}
                _ => return Err("Expected mono pixels: 0 or 255"),
            }
        }
    }
    if reader.next_row().map_err(|_| "Invalid PNG end")?.is_some() {
        return Err("Unexpected PNG rows");
    }
    reader
        .finish()
        .map_err(|_| "Incomplete or invalid PNG end")?;
    frame
        .into_boxed_slice()
        .try_into()
        .map_err(|_| "Invalid framebuffer size")
}
