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
fn refresh_waits_and_only_caches_displayed_frames() {
    use sonos_desk_firmware::frame::Refresh;
    let mut refresh = Refresh::default();
    assert!(refresh.due(0));
    assert_eq!(refresh.etag(0), None);
    refresh.checked(0); // Failed fetch/decode: no new ETag or immediate retry.
    assert!(!refresh.due(179_999));
    assert!(refresh.due(180_000));
    refresh.rendered(Some("\"first\"".into()), 180_000);
    refresh.checked(180_000);
    assert_eq!(refresh.etag(360_000), Some("\"first\""));
    refresh.checked(360_000); // 304 or failed replacement retains displayed ETag.
    assert_eq!(refresh.etag(540_000), Some("\"first\""));
    assert!(!refresh.due(539_999));
    assert!(refresh.due(540_000));
    assert_eq!(refresh.etag(86_579_999), Some("\"first\""));
    assert_eq!(refresh.etag(86_580_000), None); // Force a full refresh after a day.
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
    show_partial(&mut wire, &old, &new, region, &mut Delay).unwrap();
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
    assert_eq!(wire.packets.last().map(|p| p.0), Some(0x20));
    assert_eq!(wire.packets[wire.packets.len() - 2].1, [0xff]); // Vendor partial mode.
    assert!(!wire.packets.iter().any(|p| p.0 == 0x10)); // Panel stays awake.

    // One pixel byte at (16..24, 3): RAM x byte 97, so pixel 776 = 0x308.
    let mut wire = Wire::default();
    let tiny = Region {
        x0: 16,
        x1: 24,
        y0: 3,
        y1: 4,
    };
    show_partial(&mut wire, &old, &new, tiny, &mut Delay).unwrap();
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
            show_partial(&mut wire, &old, &new, region, &mut Delay),
            Err("BUSY timeout")
        );
        assert_eq!(wire.packets.is_empty(), fail_wait == 1);
        assert!(!wire.packets.iter().any(|p| p.0 == 0x10));
    }
}
