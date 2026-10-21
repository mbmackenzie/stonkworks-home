//! Counters that wake the display worker from the network and input threads.
use std::sync::atomic::{AtomicU32, Ordering};

/// Counters bumped from other threads; the C6 has only 32-bit atomics.
#[derive(Default)]
pub struct DisplayLink {
    changed: AtomicU32,
    activity: AtomicU32,
    force: AtomicU32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Signals {
    pub changed: u32,
    pub activity: u32,
    pub force: u32,
}

impl DisplayLink {
    /// Visible Sonos state changed (MQTT state or a completed command result).
    pub fn bump_changed(&self) {
        self.changed.fetch_add(1, Ordering::Release);
    }

    /// A physical input happened; full refreshes wait for quiet.
    pub fn bump_activity(&self) {
        self.activity.fetch_add(1, Ordering::Release);
    }

    /// The user asked for a full refresh now.
    pub fn bump_force(&self) {
        self.force.fetch_add(1, Ordering::Release);
    }

    pub fn snapshot(&self) -> Signals {
        Signals {
            changed: self.changed.load(Ordering::Acquire),
            activity: self.activity.load(Ordering::Acquire),
            force: self.force.load(Ordering::Acquire),
        }
    }
}
