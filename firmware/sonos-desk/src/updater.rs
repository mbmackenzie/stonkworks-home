use crate::{display::Panel, frame_http};
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
    let (free, low) = unsafe {
        (
            esp_idf_svc::sys::esp_get_free_heap_size(),
            esp_idf_svc::sys::esp_get_minimum_free_heap_size(),
        )
    };
    println!("Display: heap {label}: {free} free, {low} lowest");
}

pub fn run(mut panel: Panel, config: Config, ready: Arc<AtomicU32>, link: Arc<DisplayLink>) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut schedule = Schedule::default();
    let mut shown: Option<Frame> = None;
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
                if let Some(frame) = &shown {
                    if let Err(error) = panel.draw(frame) {
                        disable(error);
                    }
                    let etag = schedule.etag().map(str::to_owned);
                    schedule.full_shown(etag, now());
                    heap("after full refresh");
                }
            }
            Action::Fetch => {
                let result = (|| -> anyhow::Result<Option<(Frame, Option<String>)>> {
                    let response = frame_http::fetch_connected(&config, schedule.etag())?;
                    let Some(bytes) = response.png else {
                        println!("Display: frame unchanged; skipping refresh");
                        return Ok(None);
                    };
                    let frame = frame::decode_png(&bytes)
                        .map_err(|message| anyhow::anyhow!("{message}"))?;
                    drop(bytes);
                    if ready.load(Ordering::Acquire) == 0 {
                        anyhow::bail!("Connection unavailable; keeping previous image");
                    }
                    Ok(Some((frame, response.etag)))
                })();
                match &result {
                    Ok(Some((new, etag))) => match &shown {
                        None => {
                            if let Err(error) = panel.draw(new) {
                                disable(error);
                            }
                            schedule.full_shown(etag.clone(), now());
                        }
                        Some(old) => match changed_region(old, new) {
                            None => schedule.set_etag(etag.clone()),
                            Some(region) => {
                                if let Err(error) = panel.draw_partial(old, new, region) {
                                    disable(error);
                                }
                                schedule.partial_shown(etag.clone(), now());
                            }
                        },
                    },
                    Ok(None) => {}
                    Err(error) => {
                        println!("Display: update failed: {error}; keeping previous image")
                    }
                }
                let ok = result.is_ok();
                // Replace the cached frame only after the panel call succeeded.
                if let Ok(Some((new, _))) = result {
                    shown = Some(new);
                    heap("after update");
                }
                schedule.fetched(now(), ok);
            }
        }
    }
}
