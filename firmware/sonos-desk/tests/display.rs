#![cfg(feature = "display-check")]

use embedded_hal::delay::DelayNs;
use sonos_desk_firmware::panel::{Region, pattern, show_frame, show_partial, show_pattern};
use ssd1677::DisplayInterface;

#[derive(Default)]
struct Wire {
    packets: Vec<(u8, Vec<u8>)>,
    waits: usize,
    fail_wait: Option<usize>,
}
impl DisplayInterface for Wire {
    type Error = &'static str;
    fn send_command(&mut self, command: u8) -> Result<(), Self::Error> {
        self.packets.push((command, Vec::new()));
        Ok(())
    }
    fn send_data(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.packets.last_mut().unwrap().1.extend_from_slice(data);
        Ok(())
    }
    fn reset<D: DelayNs>(&mut self, _: &mut D) {
        panic!("board adapter owns hardware reset");
    }
    fn busy_wait<D: DelayNs>(&mut self, _: &mut D) -> Result<(), Self::Error> {
        self.waits += 1;
        if self.fail_wait == Some(self.waits) {
            Err("BUSY timeout")
        } else {
            Ok(())
        }
    }
}
struct Delay;
impl DelayNs for Delay {
    fn delay_ns(&mut self, _: u32) {}
}

#[test]
fn display_uses_vendor_sequence_and_stops_on_timeout() {
    let mut wire = Wire::default();
    show_pattern(&mut wire, &mut Delay).unwrap();
    let commands: Vec<u8> = wire.packets.iter().map(|p| p.0).collect();
    assert_eq!(
        commands,
        [
            0x12, 0x18, 0x0c, 0x01, 0x3c, 0x11, 0x44, 0x45, 0x4e, 0x4f, 0x24, 0x26, 0x22, 0x20,
            0x10
        ]
    );
    assert_eq!(wire.packets[2].1, [0xae, 0xc7, 0xc3, 0xc0, 0x80]);
    assert_eq!(wire.packets[3].1, [0xdf, 0x01, 0x02]); // 480 gates, vendor scan direction.
    assert_eq!(wire.packets[6].1, [0, 0, 0x1f, 3]); // X 0..799 in pixels, not bytes.
    assert_eq!(wire.packets[7].1, [0xdf, 1, 0, 0]); // Y 479..0.
    assert_eq!(wire.packets[10].1.len(), 48_000);
    assert_eq!(wire.packets[10].1, wire.packets[11].1); // Initialize both RAM planes.
    assert_eq!(wire.packets[10].1[439 * 100 + 94], 0); // Solid corner rotated into RAM bottom-right.
    assert_eq!(wire.packets[10].1[379 * 100 + 79], 0xff); // White background, MSB first.
    assert_eq!(wire.packets[10].1[239 * 100 + 20], 0); // Arrowhead moves to RAM left.
    assert_eq!(wire.packets[10].1[240 * 100 + 79], 0xff); // Previous arrowhead position becomes white.
    assert_eq!(wire.packets[12].1, [0xf7]); // Full refresh; no custom waveform.
    assert_eq!(wire.packets.last().unwrap().1, [1]); // Sleep after refresh.

    for fail_wait in [1, 2, 3, 4] {
        let mut wire = Wire {
            fail_wait: Some(fail_wait),
            ..Default::default()
        };
        assert_eq!(show_pattern(&mut wire, &mut Delay), Err("BUSY timeout"));
        assert!(!wire.packets.iter().any(|(command, _)| *command == 0x10));
    }
}

// Guard the decoder against large contiguous allocations on the C6.
#[cfg(feature = "frame-check")]
mod allocation {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };
    thread_local! { static LARGEST: Cell<Option<usize>> = const { Cell::new(None) }; }
    struct Observed;
    #[global_allocator]
    static ALLOCATOR: Observed = Observed;
    fn record(size: usize) {
        let _ = LARGEST.try_with(|value| {
            if let Some(previous) = value.get() {
                value.set(Some(previous.max(size)));
            }
        });
    }
    unsafe impl GlobalAlloc for Observed {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            record(size);
            unsafe { System.realloc(ptr, layout, size) }
        }
    }
    pub fn measure<T>(run: impl FnOnce() -> T) -> (T, usize) {
        LARGEST.set(Some(0));
        let result = run();
        (result, LARGEST.replace(None).unwrap())
    }
}

#[cfg(feature = "frame-check")]
#[test]
fn frame_decoder_accepts_only_complete_mono_frames() {
    use sonos_desk_firmware::frame::decode_png;
    let encode = |width, height, samples: &[u8]| {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(samples)
                .unwrap();
        }
        bytes
    };
    let mut pixels = vec![255; 800 * 480];
    pixels[0] = 0;
    pixels[800 * 240 + 16] = 0;
    pixels[800 * 480 - 1] = 0;
    let bytes = encode(800, 480, &pixels);
    let (frame, largest) = allocation::measure(|| decode_png(&bytes));
    let frame = frame.unwrap();
    assert!(
        largest <= 64 * 1024,
        "decoder requested {largest} contiguous bytes"
    );
    assert_eq!(frame[0], 0x7f);
    assert_eq!(frame[240 * 100 + 2], 0x7f);
    assert_eq!(frame[47_999], 0xfe);
    assert_eq!(frame[100], 0xff);
    assert!(decode_png(&bytes[..bytes.len() - 12]).is_err()); // Missing IEND.
    let mut corrupt = bytes.clone();
    corrupt[29] ^= 1; // IHDR CRC.
    assert!(decode_png(&corrupt).is_err());
    assert!(decode_png(&encode(8, 1, &[255; 8])).is_err());
    pixels[1] = 85;
    assert!(decode_png(&encode(800, 480, &pixels)).is_err()); // gray4 is not mono.
    assert!(decode_png(&vec![0; 128 * 1024 + 1]).is_err());
}

#[cfg(feature = "frame-check")]
#[test]
fn changed_region_bounds_differences() {
    use sonos_desk_firmware::frame::{Region, changed_region};
    let old = [0xff; 48_000];
    let mut new = old;
    assert_eq!(changed_region(&old, &new), None);
    new[3 * 100 + 2] = 0x7f; // Pixel (17, 3) lies in byte column 2.
    assert_eq!(
        changed_region(&old, &new),
        Some(Region {
            x0: 16,
            x1: 24,
            y0: 3,
            y1: 4
        })
    );
    new[0] = 0x00;
    new[47_999] = 0x00;
    assert_eq!(
        changed_region(&old, &new),
        Some(Region {
            x0: 0,
            x1: 800,
            y0: 0,
            y1: 480
        })
    );
}

fn ram_stream(frame: &[u8; 48_000]) -> Vec<u8> {
    let mut wire = Wire::default();
    show_frame(&mut wire, frame, &mut Delay).unwrap();
    wire.packets.into_iter().find(|p| p.0 == 0x24).unwrap().1
}

#[test]
fn full_refresh_does_not_modify_the_frame() {
    let frame = pattern();
    let copy = frame.clone();
    show_frame(&mut Wire::default(), &frame, &mut Delay).unwrap();
    assert_eq!(frame, copy);
}

#[test]
fn partial_window_matches_full_ram_image() {
    let old = pattern();
    let mut new = old.clone();
    new[421 * 100 + 60..421 * 100 + 70].fill(0x00);
    let (old_ram, new_ram) = (ram_stream(&old), ram_stream(&new));
    let region = Region {
        x0: 408,
        x1: 776,
        y0: 421,
        y1: 437,
    };
    let mut wire = Wire::default();
    show_partial(&mut wire, &old, &new, region, 1, &mut Delay).unwrap();
    // RAM x is mirrored (800 - x) and RAM y equals frame y; the full stream is sent from RAM y 479 down.
    let window = |ram: &[u8]| -> Vec<u8> {
        (421..437)
            .flat_map(|y| (3..49).map(move |xb| (y, xb)))
            .map(|(y, xb)| ram[(479 - y) * 100 + xb])
            .collect()
    };
    let data = |command| {
        wire.packets
            .iter()
            .find(|p| p.0 == command)
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(data(0x26), window(&old_ram));
    assert_eq!(data(0x24), window(&new_ram));
    let trigger = wire.packets.iter().position(|p| p.0 == 0x20).unwrap();
    assert_eq!(wire.packets[trigger - 1].1, [0xff]); // Vendor partial mode.
    // The controller swaps its RAM planes after a refresh, so both must then hold the
    // new window or the next partial would draw the old one back.
    for command in [0x26, 0x24] {
        let synced = wire.packets[trigger..]
            .iter()
            .find(|p| p.0 == command)
            .unwrap();
        assert_eq!(synced.1, window(&new_ram));
    }
    assert!(!wire.packets.iter().any(|p| p.0 == 0x10)); // Panel stays awake.

    // One pixel byte at (16..24, 3): RAM x byte 97, so pixel 776 = 0x308.
    let mut wire = Wire::default();
    let tiny = Region {
        x0: 16,
        x1: 24,
        y0: 3,
        y1: 4,
    };
    show_partial(&mut wire, &old, &new, tiny, 1, &mut Delay).unwrap();
    let data = |command| {
        wire.packets
            .iter()
            .find(|p| p.0 == command)
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(data(0x44), [0x08, 0x03, 0x08, 0x03]);
    assert_eq!(data(0x45), [3, 0, 3, 0]);
    assert_eq!(data(0x4e), [0x08, 0x03]);
    assert_eq!(data(0x4f), [3, 0]);
}

#[test]
fn partial_stops_on_busy_timeout() {
    let (old, new) = (pattern(), pattern());
    let region = Region {
        x0: 0,
        x1: 8,
        y0: 0,
        y1: 1,
    };
    for fail_wait in [1, 2] {
        let mut wire = Wire {
            fail_wait: Some(fail_wait),
            ..Default::default()
        };
        assert_eq!(
            show_partial(&mut wire, &old, &new, region, 1, &mut Delay),
            Err("BUSY timeout")
        );
        assert_eq!(wire.packets.is_empty(), fail_wait == 1);
        assert!(!wire.packets.iter().any(|p| p.0 == 0x10));
    }
}

#[cfg(feature = "frame-check")]
mod schedule {
    use sonos_desk_firmware::frame::{Action, DisplayLink, Schedule, Signals};

    const E: fn() -> Option<String> = || Some("\"e\"".into());

    /// A schedule that has already shown a full frame at `now`.
    fn shown(now: u64) -> Schedule {
        let mut s = Schedule::default();
        assert_eq!(s.update(Signals::default(), now), Action::Fetch);
        s.fetched(now, true);
        s.full_shown(E(), now);
        s
    }

    /// Answer every Fetch with a 304, returning the first other action.
    fn drive(s: &mut Schedule, signals: Signals, now: u64) -> Action {
        loop {
            match s.update(signals, now) {
                Action::Fetch => s.fetched(now, true),
                other => return other,
            }
        }
    }

    fn signals(changed: u32, activity: u32, force: u32) -> Signals {
        Signals {
            changed,
            activity,
            force,
        }
    }

    #[test]
    fn schedule_coalesces_dial_bursts() {
        let mut s = shown(0);
        let mut fetches = 0;
        for k in 0..20_u32 {
            if s.update(signals(k + 1, 0, 0), 1_000 + u64::from(k) * 5) == Action::Fetch {
                fetches += 1;
            }
        }
        assert_eq!(fetches, 0);
        assert_eq!(s.update(signals(20, 0, 0), 1_150), Action::Fetch);
        s.fetched(1_150, true);
        assert_eq!(s.update(signals(20, 0, 0), 1_151), Action::Wait);
    }

    #[test]
    fn schedule_settles_once_even_when_pixels_are_unchanged() {
        let mut s = shown(0);
        assert_eq!(s.update(signals(1, 0, 0), 1_000), Action::Wait);
        assert_eq!(s.update(signals(1, 0, 0), 1_150), Action::Fetch);
        s.fetched(1_150, true); // 304 or identical: no partial_shown.
        assert_eq!(s.update(signals(1, 0, 0), 3_649), Action::Wait);
        assert_eq!(s.update(signals(1, 0, 0), 3_650), Action::Fetch);
        s.fetched(3_650, true);
        assert_eq!(s.update(signals(1, 0, 0), 6_200), Action::Wait); // No second settle.
    }

    #[test]
    fn schedule_does_not_hammer_a_failing_startup_fetch() {
        let mut s = Schedule::default();
        assert_eq!(s.update(Signals::default(), 0), Action::Fetch);
        s.fetched(0, false);
        for now in (100..10_000).step_by(100) {
            assert_eq!(s.update(Signals::default(), now), Action::Wait);
        }
        assert_eq!(s.update(Signals::default(), 10_000), Action::Fetch);
    }

    #[test]
    fn schedule_cleans_up_only_when_idle() {
        let mut s = shown(0);
        assert_eq!(drive(&mut s, signals(1, 0, 0), 1_000), Action::Wait);
        assert_eq!(drive(&mut s, signals(1, 0, 0), 1_150), Action::Wait);
        s.partial_shown(E(), 1_150);
        assert_eq!(drive(&mut s, signals(1, 1, 0), 30_000), Action::Wait); // Input resets idle.
        assert_eq!(drive(&mut s, signals(1, 1, 0), 89_999), Action::Wait);
        assert_eq!(drive(&mut s, signals(1, 1, 0), 90_000), Action::FullRefresh);
        s.full_shown(E(), 90_000);
        assert_eq!(drive(&mut s, signals(1, 1, 0), 200_000), Action::Wait);
        // No partials since the last full refresh: nothing to clean up.
        assert_eq!(drive(&mut s, signals(1, 1, 0), 10_000_000), Action::Wait);
    }

    #[test]
    fn schedule_backstop_survives_partials_and_304() {
        const DAY: u64 = 86_400_000;
        let mut s = shown(0);
        s.partial_shown(E(), DAY / 2); // Partials never advance the full-refresh clock.
        assert_eq!(drive(&mut s, signals(0, 1, 0), DAY), Action::Wait); // Active: deferred.
        assert_eq!(
            drive(&mut s, signals(0, 1, 0), DAY + 60_000),
            Action::FullRefresh
        );
        s.full_shown(E(), DAY + 60_000);
        // Every fetch is a 304 from here on; the backstop still redraws the cached frame.
        assert_eq!(
            drive(&mut s, signals(0, 1, 0), 2 * DAY + 60_000 - 1),
            Action::Wait
        );
        assert_eq!(
            drive(&mut s, signals(0, 1, 0), 2 * DAY + 60_000),
            Action::FullRefresh
        );
    }

    #[test]
    fn schedule_forced_refresh_is_immediate() {
        let link = DisplayLink::default();
        let mut s = shown(0);
        link.bump_activity();
        assert_eq!(s.update(link.snapshot(), 100), Action::Wait);
        link.bump_force();
        assert_eq!(s.update(link.snapshot(), 101), Action::FullRefresh);
        assert_eq!(s.update(link.snapshot(), 102), Action::Wait);
    }

    #[test]
    fn schedule_keeps_etag_and_polls_on_failure() {
        let mut s = shown(0);
        assert_eq!(s.etag(), Some("\"e\""));
        s.fetched(0, false); // A failed replacement keeps the displayed ETag.
        assert_eq!(s.etag(), Some("\"e\""));
        s.set_etag(Some("\"new\"".into())); // Identical pixels under a new ETag.
        assert_eq!(s.etag(), Some("\"new\""));
        assert_eq!(s.update(Signals::default(), 179_999), Action::Wait);
        assert_eq!(s.update(Signals::default(), 180_000), Action::Fetch);
    }
}

#[cfg(feature = "frame-check")]
#[test]
fn frame_decoder_overwrites_a_reused_buffer() {
    use sonos_desk_firmware::frame::{decode_png, decode_png_into};
    let mut pixels = vec![255; 800 * 480];
    pixels[800 * 100 + 40] = 0;
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 800, 480);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
    }
    let mut reused = [0x00; 48_000]; // A previous frame's leftovers.
    decode_png_into(&bytes, &mut reused).unwrap();
    assert_eq!(&reused, &*decode_png(&bytes).unwrap());
    assert!(decode_png_into(&bytes[..bytes.len() - 12], &mut reused).is_err());
}

#[cfg(feature = "frame-check")]
#[test]
fn bitmap_body_must_be_exactly_one_frame() {
    use sonos_desk_firmware::frame::fill_bitmap;
    let source: Vec<u8> = (0..48_000_u32).map(|i| (i % 251) as u8).collect();
    let chunked = |data: &[u8], size: usize| {
        let mut rest = data.to_vec();
        move |buffer: &mut [u8]| -> Result<usize, &'static str> {
            let count = rest.len().min(size).min(buffer.len());
            buffer[..count].copy_from_slice(&rest[..count]);
            rest.drain(..count);
            Ok(count)
        }
    };
    let mut frame = [0_u8; 48_000];
    assert!(fill_bitmap(chunked(&source, 1_000), &mut frame).is_ok());
    assert_eq!(frame.as_slice(), source.as_slice());
    assert!(fill_bitmap(chunked(&source[..47_999], 1_000), &mut frame).is_err()); // Truncated.
    let mut long = source.clone();
    long.push(0);
    assert!(fill_bitmap(chunked(&long, 1_000), &mut frame).is_err()); // Trailing byte.
    let failing = |_: &mut [u8]| -> Result<usize, &'static str> { Err("link down") };
    assert!(fill_bitmap(failing, &mut frame).is_err());
}

#[test]
fn partial_repeats_the_drive_before_syncing_the_planes() {
    let old = pattern();
    let mut new = old.clone();
    new[100 * 10..100 * 10 + 5].fill(0x00);
    let region = Region {
        x0: 0,
        x1: 80,
        y0: 10,
        y1: 11,
    };
    let mut wire = Wire::default();
    show_partial(&mut wire, &old, &new, region, 2, &mut Delay).unwrap();
    let commands: Vec<u8> = wire.packets.iter().map(|p| p.0).collect();
    let triggers: Vec<usize> = (0..commands.len())
        .filter(|&i| commands[i] == 0x20)
        .collect();
    assert_eq!(triggers.len(), 2);
    // Each pass rewrites old into 0x26 and new into 0x24, so the same pixels are driven again.
    for (pass, &trigger) in triggers.iter().enumerate() {
        let start = if pass == 0 { 0 } else { triggers[0] + 1 };
        let planes: Vec<u8> = commands[start..trigger]
            .iter()
            .copied()
            .filter(|c| *c == 0x26 || *c == 0x24)
            .collect();
        assert_eq!(planes, [0x26, 0x24]);
    }
    // Only after the last pass are both planes set to new.
    let after: Vec<u8> = commands[triggers[1]..]
        .iter()
        .copied()
        .filter(|c| *c == 0x26 || *c == 0x24)
        .collect();
    assert_eq!(after, [0x26, 0x24]);
    let waits_fail = Wire {
        fail_wait: Some(3),
        ..Default::default()
    };
    let mut wire = waits_fail;
    assert_eq!(
        show_partial(&mut wire, &old, &new, region, 2, &mut Delay),
        Err("BUSY timeout")
    );
    assert_eq!(wire.packets.iter().filter(|p| p.0 == 0x20).count(), 2); // Stops after pass two's wait.
}
