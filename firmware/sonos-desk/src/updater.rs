use crate::{display::Panel, frame_http};
use sonos_desk_firmware::{
    config::Config,
    frame::{self, Action, Schedule, Signals},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub fn run(mut panel: Panel, config: Config, ready: Arc<AtomicU32>) {
    let start = Instant::now();
    let mut schedule = Schedule::default();
    loop {
        let now = start.elapsed().as_millis() as u64;
        if ready.load(Ordering::Acquire) != 0
            && schedule.update(Signals::default(), now) == Action::Fetch
        {
            let result = (|| -> anyhow::Result<()> {
                let response = frame_http::fetch_connected(&config, schedule.etag())?;
                let Some(bytes) = response.png else {
                    println!("Display: frame unchanged; skipping refresh");
                    return Ok(());
                };
                let frame =
                    frame::decode_png(&bytes).map_err(|message| anyhow::anyhow!("{message}"))?;
                drop(bytes);
                if ready.load(Ordering::Acquire) == 0 {
                    anyhow::bail!("Connection unavailable; keeping previous image");
                }
                if let Err(error) = panel.draw(&frame) {
                    println!("Display: panel failed: {error}; refresh disabled until reset");
                    // Hold PWR low and leave controls running after a panel/SPI fault.
                    loop {
                        thread::park();
                    }
                }
                schedule.full_shown(response.etag, start.elapsed().as_millis() as u64);
                Ok(())
            })();
            if let Err(error) = &result {
                println!("Display: update failed: {error}; keeping previous image");
            }
            schedule.fetched(start.elapsed().as_millis() as u64, result.is_ok());
        }
        thread::sleep(Duration::from_millis(250));
    }
}
