//! Receive the packed bitmap HTTP contract and decide when the panel refreshes.
pub use crate::link::{DisplayLink, Signals};

/// Separate boxes for separate clusters of change (for example the art and the text),
/// so each can be driven in the way that suits it. Byte columns closer than three apart
/// share a box. More than four boxes collapse into one bounding box: every refresh costs
/// about half a second regardless of size.
pub fn changed_regions(old: &[u8; 48_000], new: &[u8; 48_000]) -> Vec<Region> {
    let mut rows: [Option<(u16, u16)>; 100] = [None; 100];
    for (y, (old_row, new_row)) in old.chunks(100).zip(new.chunks(100)).enumerate() {
        for (column, (a, b)) in old_row.iter().zip(new_row).enumerate() {
            if a != b {
                let y = y as u16;
                rows[column] =
                    Some(rows[column].map_or((y, y), |(low, high)| (low.min(y), high.max(y))));
            }
        }
    }
    let mut groups: Vec<(usize, usize, u16, u16)> = Vec::new();
    for (column, span) in rows.iter().enumerate() {
        let Some((low, high)) = *span else { continue };
        match groups.last_mut() {
            Some(group) if column - group.1 <= 3 => {
                group.1 = column;
                group.2 = group.2.min(low);
                group.3 = group.3.max(high);
            }
            _ => groups.push((column, column, low, high)),
        }
    }
    let region = |(first, last, low, high): (usize, usize, u16, u16)| Region {
        x0: first as u16 * 8,
        x1: (last as u16 + 1) * 8,
        y0: low,
        y1: high + 1,
    };
    if groups.len() > 4 {
        let (first, last) = (groups[0].0, groups[groups.len() - 1].1);
        let low = groups.iter().map(|g| g.2).min().unwrap_or(0);
        let high = groups.iter().map(|g| g.3).max().unwrap_or(0);
        return vec![region((first, last, low, high))];
    }
    groups.into_iter().map(region).collect()
}

/// Accept only the packed bitmap. An older service that ignores `format=bitmap` answers
/// with a PNG; decoding one beside two frame buffers can exhaust the heap and abort the
/// whole device, so it is refused with a recoverable error instead.
pub fn check_frame_content_type(content_type: Option<&str>) -> Result<(), &'static str> {
    let kind = content_type
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    match kind.as_deref() {
        Some("application/octet-stream") => Ok(()),
        Some("image/png") => {
            Err("Service sent a PNG; update Sonos Desk to a version with format=bitmap")
        }
        _ => Err("Frame response is not a bitmap"),
    }
}

#[derive(Debug)]
pub enum BitmapError<E> {
    Read(E),
    Size(&'static str),
}

/// Fill `frame` from a `format=bitmap` response body, which must be exactly one
/// packed frame: no PNG bytes and no decoder, so no memory beyond the frame itself.
pub fn fill_bitmap<E>(
    mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
    frame: &mut [u8; 48_000],
) -> Result<(), BitmapError<E>> {
    let mut filled = 0;
    while filled < frame.len() {
        match read(&mut frame[filled..]).map_err(BitmapError::Read)? {
            0 => return Err(BitmapError::Size("Truncated bitmap")),
            count => filled += count,
        }
    }
    if read(&mut [0]).map_err(BitmapError::Read)? != 0 {
        return Err(BitmapError::Size("Bitmap longer than one frame"));
    }
    Ok(())
}

pub use crate::panel::Region;

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
const CONTENT_CLEANUP_MS: u64 = 4_000;
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
    content_at: Option<u64>,
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
        // Art or text changed a few seconds ago: clean up soon, after the settle fetch.
        if self.shown
            && self
                .content_at
                .is_some_and(|at| now_ms.saturating_sub(at) >= CONTENT_CLEANUP_MS)
        {
            return Action::FullRefresh;
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

    /// A partial just changed the art or the text, which leaves the most visible residue.
    /// Presses and small changes (volume, labels) deliberately do not call this.
    pub fn content_changed(&mut self, now_ms: u64) {
        self.content_at = Some(now_ms);
    }

    pub fn partial_shown(&mut self, etag: Option<String>) {
        self.etag = etag;
        self.partials += 1;
    }

    pub fn full_shown(&mut self, etag: Option<String>, now_ms: u64) {
        self.etag = etag;
        self.shown = true;
        self.last_full = now_ms;
        self.partials = 0;
        self.content_at = None;
    }
}
