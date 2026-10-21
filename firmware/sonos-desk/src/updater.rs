use crate::{display::Panel, frame_http};
use sonos_desk_firmware::{
    config::Config,
    frame::{self, Refresh},
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
    let mut refresh = Refresh::default();
    loop {
        let now = start.elapsed().as_millis() as u64;
        if ready.load(Ordering::Acquire) != 0 && refresh.due(now) {
            let result = (|| -> anyhow::Result<()> {
                let response = frame_http::fetch_connected(&config, refresh.etag(now))?;
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
                refresh.rendered(response.etag, start.elapsed().as_millis() as u64);
                Ok(())
            })();
            if let Err(error) = result {
                println!("Display: update failed: {error}; keeping previous image");
            }
            refresh.checked(start.elapsed().as_millis() as u64);
        }
        thread::sleep(Duration::from_millis(250));
    }
}
