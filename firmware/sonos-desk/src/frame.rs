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

pub use crate::link::{DisplayLink, Signals};
pub use crate::panel::Region;

/// Smallest byte-aligned box covering every differing byte, or `None` if identical.
pub fn changed_region(old: &[u8; 48_000], new: &[u8; 48_000]) -> Option<Region> {
    let mut bounds: Option<(u16, u16, u16, u16)> = None;
    for (y, (old_row, new_row)) in old.chunks(100).zip(new.chunks(100)).enumerate() {
        let mut differing = old_row
            .iter()
            .zip(new_row)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(column, _)| column as u16);
        if let Some(first) = differing.next() {
            let last = differing.next_back().unwrap_or(first);
            let (x0, x1, y0, _) = bounds.unwrap_or((first, last, y as u16, 0));
            bounds = Some((x0.min(first), x1.max(last), y0, y as u16));
        }
    }
    bounds.map(|(x0, x1, y0, y1)| Region {
        x0: x0 * 8,
        x1: (x1 + 1) * 8,
        y0,
        y1: y1 + 1,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    Fetch,
    FullRefresh,
}

const DEBOUNCE_MS: u64 = 150;
const SETTLE_MS: u64 = 2_500;
const POLL_MS: u64 = 180_000;
const STARTUP_RETRY_MS: u64 = 10_000;
const IDLE_MS: u64 = 60_000;
const BACKSTOP_MS: u64 = 86_400_000;

/// Decide when to fetch a frame and when to clean ghosting with a full refresh.
/// Only the ETag of an image actually shown on the panel is kept.
#[derive(Default)]
pub struct Schedule {
    seen: Signals,
    fetch_at: Option<u64>,
    settle_at: Option<u64>,
    next_check: u64,
    shown: bool,
    last_activity: u64,
    last_full: u64,
    partials: u32,
    etag: Option<String>,
}

impl Schedule {
    pub fn update(&mut self, signals: Signals, now_ms: u64) -> Action {
        if signals.changed != self.seen.changed {
            self.fetch_at.get_or_insert(now_ms + DEBOUNCE_MS);
            self.last_activity = now_ms;
        }
        if signals.activity != self.seen.activity {
            self.last_activity = now_ms;
        }
        let force = signals.force != self.seen.force;
        self.seen = signals;
        if force && self.shown {
            return Action::FullRefresh;
        }
        if self.fetch_at.is_some_and(|at| now_ms >= at) {
            self.fetch_at = None;
            self.settle_at = Some(now_ms + SETTLE_MS); // The server's state may still be moving.
            return Action::Fetch;
        }
        if self.settle_at.is_some_and(|at| now_ms >= at) {
            self.settle_at = None;
            return Action::Fetch;
        }
        if now_ms >= self.next_check {
            return Action::Fetch;
        }
        let idle = now_ms.saturating_sub(self.last_activity) >= IDLE_MS;
        if self.shown
            && idle
            && (self.partials > 0 || now_ms.saturating_sub(self.last_full) >= BACKSTOP_MS)
        {
            return Action::FullRefresh;
        }
        Action::Wait
    }

    /// ETag of the image on the panel; it does not expire (the backstop redraws instead).
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// A fetch attempt ended: success, 304, or (`ok` false) an HTTP or decode failure.
    pub fn fetched(&mut self, now_ms: u64, ok: bool) {
        let wait = if ok || self.shown {
            POLL_MS
        } else {
            STARTUP_RETRY_MS
        };
        self.next_check = now_ms.saturating_add(wait);
    }

    /// The server sent identical pixels under a new ETag; remember it without drawing.
    pub fn set_etag(&mut self, etag: Option<String>) {
        self.etag = etag;
    }

    pub fn partial_shown(&mut self, etag: Option<String>, _now_ms: u64) {
        self.etag = etag;
        self.partials += 1;
    }

    pub fn full_shown(&mut self, etag: Option<String>, now_ms: u64) {
        self.etag = etag;
        self.shown = true;
        self.last_full = now_ms;
        self.partials = 0;
    }
}
