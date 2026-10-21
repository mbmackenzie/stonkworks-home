#![cfg(feature = "display-check")]

use embedded_hal::delay::DelayNs;
use sonos_desk_firmware::panel::show_pattern;
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
