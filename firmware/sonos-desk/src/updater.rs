use crate::{
    display::Panel,
    frame_http::{self, Body},
};
use sonos_desk_firmware::{
    config::Config,
    frame::{Action, Schedule, changed_regions},
    link::DisplayLink,
    panel::blank_frame,
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
    // panel, `next` receives each download. A fresh 48 KB per update would fragment
    // the small heap.
    let mut shown = blank_frame();
    let mut next = blank_frame();
    let mut have_shown = false;
    let mut last_force = 0;
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
        // A held Mute asks for the thorough refresh; idle cleanup uses the fast waveform.
        let forced = signals.force != last_force;
        last_force = signals.force;
        match schedule.update(signals, now()) {
            Action::Wait => {}
            Action::FullRefresh => {
                // Redraw the cached frame to clear ghosting; no network involved.
                if have_shown {
                    if let Err(error) = panel.draw_with(&shown, !forced) {
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
                                let content = regions.iter().any(|region| region.is_content());
                                for region in regions {
                                    println!(
                                        "Display: partial x {}..{} y {}..{}",
                                        region.x0, region.x1, region.y0, region.y1
                                    );
                                    if let Err(error) = panel.draw_partial(&shown, &next, region) {
                                        disable(error);
                                    }
                                }
                                schedule.partial_shown(etag);
                                if content {
                                    schedule.content_changed(now());
                                }
                            }
                        }
                        // The panel call succeeded, so the downloaded frame is what it shows.
                        std::mem::swap(&mut shown, &mut next);
                        have_shown = true;
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
