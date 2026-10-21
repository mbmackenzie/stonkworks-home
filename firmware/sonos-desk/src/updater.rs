use crate::{
    display::Panel,
    frame_http::{self, Body},
};
use sonos_desk_firmware::{
    config::Config,
    frame::{Action, Schedule, changed_regions},
    link::DisplayLink,
    panel::{Frame, Style},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Experiment: each forced refresh (hold Mute) restarts from a clean screen and switches
/// to the next style, so they can be compared by eye. Text boxes and art are styled apart.
const STYLES: [(&str, Style, Style); 3] = [
    (
        "A: two direct passes everywhere",
        Style::Direct(2),
        Style::Direct(2),
    ),
    (
        "B: erase-then-write text, one pass art",
        Style::EraseThenWrite,
        Style::Direct(1),
    ),
    (
        "C: three direct passes text, one pass art",
        Style::Direct(3),
        Style::Direct(1),
    ),
];

fn heap(label: &str) {
    // SAFETY: Plain ESP-IDF getters with no preconditions.
    let (free, low, block) = unsafe {
        (
            esp_idf_svc::sys::esp_get_free_heap_size(),
            esp_idf_svc::sys::esp_get_minimum_free_heap_size(),
            esp_idf_svc::sys::heap_caps_get_largest_free_block(
                esp_idf_svc::sys::MALLOC_CAP_DEFAULT,
            ),
        )
    };
    println!("Display: heap {label}: {free} free, {low} lowest, {block} largest block");
}

pub fn run(mut panel: Panel, config: Config, ready: Arc<AtomicU32>, link: Arc<DisplayLink>) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut schedule = Schedule::default();
    // Two long-lived frames, allocated before the heap fragments: `shown` is on the
    // panel, `next` receives each decode. A fresh 48 KB per update starved the PNG
    // decoder of its contiguous 32 KiB window.
    let mut shown: Frame = blank();
    let mut next: Frame = blank();
    let mut have_shown = false;
    let (mut style, mut last_force) = (0, 0);
    heap("at start");
    // Hold PWR low and leave controls running after a panel/SPI fault.
    let disable = |error: anyhow::Error| -> ! {
        println!("Display: panel failed: {error}; refresh disabled until reset");
        loop {
            thread::park();
        }
    };
    loop {
        thread::sleep(Duration::from_millis(100));
        if ready.load(Ordering::Acquire) == 0 {
            continue;
        }
        let signals = link.snapshot();
        if signals.force != last_force {
            last_force = signals.force;
            style = (style + 1) % STYLES.len();
            println!("Display: style {}", STYLES[style].0);
        }
        match schedule.update(signals, now()) {
            Action::Wait => {}
            Action::FullRefresh => {
                // Redraw the cached frame to clear ghosting; no network involved.
                if have_shown {
                    if let Err(error) = panel.draw(&shown) {
                        disable(error);
                    }
                    let etag = schedule.etag().map(str::to_owned);
                    schedule.full_shown(etag, now());
                    heap("after full refresh");
                }
            }
            Action::Fetch => {
                let result = (|| -> anyhow::Result<Option<Option<String>>> {
                    let response =
                        frame_http::fetch_connected(&config, schedule.etag(), &mut next)?;
                    match response.body {
                        Body::Unchanged => {
                            println!("Display: frame unchanged; skipping refresh");
                            return Ok(None);
                        }
                        Body::Bitmap => {}
                    }
                    if ready.load(Ordering::Acquire) == 0 {
                        anyhow::bail!("Connection unavailable; keeping previous image");
                    }
                    Ok(Some(response.etag))
                })();
                let ok = result.is_ok();
                match result {
                    Ok(Some(etag)) => {
                        if !have_shown {
                            if let Err(error) = panel.draw(&next) {
                                disable(error);
                            }
                            schedule.full_shown(etag, now());
                        } else {
                            let regions = changed_regions(&shown, &next);
                            if regions.is_empty() {
                                schedule.set_etag(etag);
                            } else {
                                for region in regions {
                                    let (_, text, art) = STYLES[style];
                                    let style = if region.looks_like_art() { art } else { text };
                                    println!(
                                        "Display: partial x {}..{} y {}..{} {style:?}",
                                        region.x0, region.x1, region.y0, region.y1
                                    );
                                    if let Err(error) =
                                        panel.draw_partial(&shown, &next, region, style)
                                    {
                                        disable(error);
                                    }
                                }
                                schedule.partial_shown(etag, now());
                            }
                        }
                        // The panel call succeeded, so the decoded frame is what it shows.
                        if !have_shown || next != shown {
                            std::mem::swap(&mut shown, &mut next);
                            have_shown = true;
                        }
                        heap("after update");
                    }
                    Ok(None) => {}
                    Err(error) => {
                        println!("Display: update failed: {error}; keeping previous image")
                    }
                }
                schedule.fetched(now(), ok);
            }
        }
    }
}

fn blank() -> Frame {
    vec![0xff; 48_000]
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!())
}
