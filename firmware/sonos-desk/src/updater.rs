use crate::{
    display::Panel,
    frame_http::{self, Body},
};
use sonos_desk_firmware::{
    config::Config,
    frame::{self, Action, Schedule, changed_region},
    link::DisplayLink,
    panel::Frame,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

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
        match schedule.update(link.snapshot(), now()) {
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
                        Body::Png(bytes) => {
                            frame::decode_png_into(&bytes, &mut next)
                                .map_err(|message| anyhow::anyhow!("{message}"))?;
                        }
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
                        } else if let Some(region) = changed_region(&shown, &next) {
                            println!(
                                "Display: partial x {}..{} y {}..{} etag {:?}",
                                region.x0, region.x1, region.y0, region.y1, etag
                            );
                            if let Err(error) = panel.draw_partial(&shown, &next, region) {
                                disable(error);
                            }
                            schedule.partial_shown(etag, now());
                        } else {
                            schedule.set_etag(etag);
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
