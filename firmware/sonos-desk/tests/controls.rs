use sonos_desk_firmware::config::Config;
use sonos_desk_firmware::inputs::{InputEvent, Inputs, Levels};

#[test]
fn configuration_validates_settings_without_exposing_secrets() {
    let example = include_str!("../config.example.toml");
    let config = Config::parse(example).expect("valid example");
    assert_eq!(config.mqtt.port, 8883);
    assert_eq!(config.mqtt.instance, "main");

    for invalid in [
        example.replace("YOUR_WIFI_NAME", &"x".repeat(33)),
        example.replace("CHANGE_ME", "short"),
        example.replace("8883", "0"),
        example.replace("desk-controller-01", "desk/#"),
        example.replace("mqtt.example.test", "mqtt://broker"),
        example.replace("RINCON_00000000000101400", "room/#"),
        example.replace(
            "http://sonos-desk.example.test:8008",
            "http://user:password@host",
        ),
        example.replace(
            "http://sonos-desk.example.test:8008",
            "http://host/path?room=bad",
        ),
        example.replace("http://sonos-desk.example.test:8008", "https://host:8008"),
        example.replace("CHANGE_ME", "do-not-log-this\n[invalid"),
    ] {
        let error = Config::parse(&invalid)
            .err()
            .expect("invalid settings rejected");
        assert!(!error.contains("do-not-log-this"));
    }
}

#[test]
fn buttons_require_stable_press_and_release() {
    // Each column tests the same debounce and hold behavior on a different button.
    for (button, expected) in [
        (0, InputEvent::Previous),
        (1, InputEvent::PlayPause),
        (2, InputEvent::Next),
        (3, InputEvent::Mute),
    ] {
        let levels = |pressed| {
            let mut state = Levels::default();
            match button {
                0 => state.previous = pressed,
                1 => state.play_pause = pressed,
                2 => state.next = pressed,
                _ => state.mute = pressed,
            }
            state
        };
        let mut inputs = Inputs::new(levels(true));
        assert!(inputs.sample(levels(true), 100).is_empty()); // Held at boot.
        assert!(inputs.sample(levels(false), 101).is_empty());
        assert!(inputs.sample(levels(false), 121).is_empty());
        for (time, pressed) in [(130, true), (135, false), (140, true), (159, true)] {
            assert!(inputs.sample(levels(pressed), time).is_empty());
        }
        assert_eq!(inputs.sample(levels(true), 160), vec![expected]);
        assert!(inputs.sample(levels(true), 1000).is_empty()); // No hold-repeat.
        inputs.disarm(levels(true));
        assert!(inputs.sample(levels(true), 1100).is_empty());
    }

    // Simultaneous buttons must not lose one another's events.
    let mut inputs = Inputs::new(Levels::default());
    let both = Levels {
        previous: true,
        next: true,
        ..Levels::default()
    };
    assert!(inputs.sample(both, 1).is_empty());
    assert_eq!(
        inputs.sample(both, 21),
        vec![InputEvent::Previous, InputEvent::Next]
    );
}

#[test]
fn encoder_counts_complete_detents() {
    let levels = |ab: u8| Levels {
        a: ab & 2 != 0,
        b: ab & 1 != 0,
        ..Levels::default()
    };
    for (trace, expected) in [
        // Calibrated to the connected KY-040: clockwise increases volume.
        (vec![1, 3, 2, 0], vec![InputEvent::Volume(-1)]),
        (vec![2, 3, 1, 0], vec![InputEvent::Volume(1)]),
        (vec![1, 0, 1, 3, 2, 0], vec![InputEvent::Volume(-1)]),
        (vec![3, 0], vec![]),       // Impossible two-bit jumps.
        (vec![1, 3, 1, 0], vec![]), // Turn partway, then back.
    ] {
        let mut inputs = Inputs::new(levels(0));
        let mut events = Vec::new();
        for (time, ab) in trace.into_iter().enumerate() {
            events.extend(inputs.sample(levels(ab), time as u64 + 1));
        }
        assert_eq!(events, expected);
    }

    let mut inputs = Inputs::new(levels(0));
    assert!(inputs.sample(levels(1), 1).is_empty());
    inputs.disarm(levels(1));
    for (time, ab) in [(3, 3), (4, 2), (5, 0)] {
        assert!(inputs.sample(levels(ab), time).is_empty());
    }
}

use sonos_desk_firmware::controller::{Controller, ControllerConfig, Effect, Event, Time};
use sonos_desk_firmware::protocol::{Assembly, Command, Observation};
use uuid::Uuid;

const SESSION: &str = "bb678ecb-e986-4c29-b0dd-7c4bdd79a1ae";
const UTC: i64 = 1_792_195_200_000; // 2026-10-17T00:00:00Z

fn step(controller: &mut Controller, event: Event, ms: u64) -> Vec<Effect> {
    controller.handle(
        event,
        Time {
            ms,
            utc_ms: Some(UTC + ms as i64),
        },
        &mut || {
            use std::sync::atomic::{AtomicU64, Ordering};
            static ID: AtomicU64 = AtomicU64::new(1);
            Uuid::from_u128(ID.fetch_add(1, Ordering::Relaxed) as u128)
        },
    )
}

fn message(
    controller: &mut Controller,
    topic: &str,
    json: serde_json::Value,
    ms: u64,
) -> Vec<Effect> {
    let observation = Observation::parse(
        topic,
        &serde_json::to_vec(&json).unwrap(),
        &controller.config,
    )
    .unwrap();
    step(controller, Event::Observation(observation), ms)
}

fn snapshots(controller: &mut Controller, ms: u64, stamp: &str, mute: bool) {
    for (topic, json) in [
        (
            "availability",
            serde_json::json!({"service_session":SESSION,"status":"online"}),
        ),
        (
            "rooms",
            serde_json::json!({"service_session":SESSION,"observed_at":stamp,"discovery":"ok","rooms":[{"room_id":"room-1","room":"Study","ip":"192.0.2.1","coordinator":"Study","coordinator_id":"room-1"}]}),
        ),
        (
            "rooms/room-1/availability",
            serde_json::json!({"service_session":SESSION,"observed_at":stamp,"status":"online"}),
        ),
        (
            "rooms/room-1/state",
            serde_json::json!({"service_session":SESSION,"observed_at":stamp,"state":{"room_id":"room-1","room":"Study","coordinator":"Study","coordinator_id":"room-1","title":"","artist":"","album":"","artwork_url":"","playback_state":"PAUSED_PLAYBACK","duration":"0:00:00","volume":20,"mute":mute}}),
        ),
    ] {
        message(controller, topic, json, ms);
    }
}

fn controller() -> Controller {
    Controller::new(ControllerConfig {
        instance: "test".into(),
        client_id: "desk".into(),
        room_id: "room-1".into(),
    })
}

fn ready() -> Controller {
    let mut c = controller();
    snapshots(&mut c, 0, "2026-10-17T00:00:00Z", false);
    step(&mut c, Event::Subscribed, 0);
    c
}

fn sent(effects: Vec<Effect>) -> Vec<Command> {
    effects
        .into_iter()
        .filter_map(|effect| {
            if let Effect::Publish(command) = effect {
                Some(command)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn readiness_requires_matching_fresh_snapshots() {
    let mut assembly = Assembly::default();
    assert!(
        assembly
            .push(Some("availability"), b"{", 2, 0)
            .unwrap()
            .is_none()
    );
    assert_eq!(assembly.push(None, b"}", 2, 1).unwrap().unwrap().1, b"{}");
    assert!(
        assembly
            .push(Some("availability"), b"", 32 * 1024 + 1, 0)
            .is_err()
    );
    assert!(assembly.push(None, b"x", 4, 1).is_err());

    // Removing freshness/session gates would send a command in these offline cases.
    for stamp in ["2026-10-16T23:59:14Z", "2026-10-17T00:00:06Z"] {
        let mut c = controller();
        snapshots(&mut c, 0, stamp, false);
        step(&mut c, Event::Subscribed, 0);
        assert!(sent(step(&mut c, Event::Input(InputEvent::Next), 0)).is_empty());
    }
    let mut c = controller();
    snapshots(&mut c, 0, "2026-10-17T00:00:05Z", false);
    assert!(sent(step(&mut c, Event::Input(InputEvent::Next), 0)).is_empty());
    step(&mut c, Event::Subscribed, 0);
    let commands = sent(step(&mut c, Event::Input(InputEvent::Next), 1));
    assert_eq!(commands.len(), 1);
    let json: serde_json::Value = serde_json::from_slice(&commands[0].payload().unwrap()).unwrap();
    assert_eq!(json["command"], "next");
    assert!(json.get("value").is_none());
    assert_eq!(
        commands[0].topic,
        "stonkworks/v1/sonos-desk/test/rooms/room-1/command"
    );
    assert_eq!(commands[0].expiry_seconds(1001), Some(4));
    assert_eq!(commands[0].expiry_seconds(5001), None);
    assert!(sent(step(&mut c, Event::Input(InputEvent::Next), 45_001)).is_empty());

    let mut c = ready();
    message(
        &mut c,
        "rooms",
        serde_json::json!({"service_session":SESSION,"observed_at":"2026-10-17T00:00:01Z","discovery":"ok","rooms":[]}),
        1000,
    );
    assert!(sent(step(&mut c, Event::Input(InputEvent::Next), 1001)).is_empty());
    snapshots(&mut c, 2000, "2026-10-17T00:00:02Z", false);
    message(
        &mut c,
        "availability",
        serde_json::json!({"service_session":"00000000-0000-4000-8000-000000000002","status":"online"}),
        2000,
    );
    assert!(sent(step(&mut c, Event::Input(InputEvent::Next), 2001)).is_empty());
}

#[test]
fn volume_batches_without_backlog() {
    let mut c = ready();
    for ms in [0, 30, 99] {
        assert!(sent(step(&mut c, Event::Input(InputEvent::Volume(1)), ms)).is_empty());
    }
    let command = sent(step(&mut c, Event::Tick, 100)).pop().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&command.payload().unwrap()).unwrap();
    assert_eq!(json["command"], "volume_relative");
    assert_eq!(json["value"], 3);
    for (ms, delta) in [(110, 1), (120, -1)] {
        step(&mut c, Event::Input(InputEvent::Volume(delta)), ms);
    }
    assert!(sent(step(&mut c, Event::Tick, 210)).is_empty());
    for ms in [220, 230, 240] {
        assert_eq!(
            sent(step(&mut c, Event::Input(InputEvent::Next), ms)).len(),
            1
        );
    }
    step(&mut c, Event::Input(InputEvent::Volume(1)), 250);
    assert!(sent(step(&mut c, Event::Tick, 350)).is_empty()); // Four occupied slots; drop batch.
    step(&mut c, Event::Tick, 16_000);
    assert!(sent(step(&mut c, Event::Tick, 16_001)).is_empty());
    for ms in 16_010..16_110 {
        step(&mut c, Event::Input(InputEvent::Volume(2)), ms);
    }
    let command = sent(step(&mut c, Event::Tick, 16_110)).pop().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&command.payload().unwrap()).unwrap()["value"],
        100
    );
}

#[test]
fn mute_uses_confirmed_state() {
    let mut c = ready();
    let command = sent(step(&mut c, Event::Input(InputEvent::Mute), 10))
        .pop()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&command.payload().unwrap()).unwrap()["value"],
        true
    );
    assert!(sent(step(&mut c, Event::Input(InputEvent::Mute), 20)).is_empty());
    snapshots(&mut c, 30, "2026-10-17T00:00:00Z", false); // Old cached state cannot unlock.
    assert!(sent(step(&mut c, Event::Input(InputEvent::Mute), 40)).is_empty());
    snapshots(&mut c, 2000, "2026-10-17T00:00:02Z", true);
    message(
        &mut c,
        "clients/desk/results",
        serde_json::json!({"service_session":SESSION,"client_id":"desk","request_id":command.request_id,"room_id":"room-1","completed_at":"2026-10-17T00:00:01Z","status":"completed","detail":"","state":{"room_id":"room-1","volume":20,"mute":false}}),
        2000,
    );
    let next = sent(step(&mut c, Event::Input(InputEvent::Mute), 2010))
        .pop()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&next.payload().unwrap()).unwrap()["value"],
        false
    );
    // Duplicate result cannot clear the newer mute guard.
    message(
        &mut c,
        "clients/desk/results",
        serde_json::json!({"service_session":SESSION,"client_id":"desk","request_id":command.request_id,"room_id":"room-1","completed_at":"2026-10-17T00:00:01Z","status":"rejected","detail":""}),
        2020,
    );
    assert!(sent(step(&mut c, Event::Input(InputEvent::Mute), 2030)).is_empty());
    // A's first delayed result must not release B's newer mute guard.
    let mut c = ready();
    let a = sent(step(&mut c, Event::Input(InputEvent::Mute), 10))
        .pop()
        .unwrap();
    snapshots(&mut c, 2000, "2026-10-17T00:00:02Z", true);
    assert_eq!(
        sent(step(&mut c, Event::Input(InputEvent::Mute), 2010)).len(),
        1
    );
    message(
        &mut c,
        "clients/desk/results",
        serde_json::json!({"service_session":SESSION,"client_id":"desk","request_id":a.request_id,"room_id":"room-1","completed_at":"2026-10-17T00:00:02.005Z","status":"completed","detail":"","state":{"room_id":"room-1","volume":20,"mute":true}}),
        2020,
    );
    assert!(sent(step(&mut c, Event::Input(InputEvent::Mute), 2030)).is_empty());
}

#[test]
fn uncertain_actions_are_never_replayed() {
    let mut c = ready();
    let action = sent(step(&mut c, Event::Input(InputEvent::Mute), 10))
        .pop()
        .unwrap();
    let late = message(
        &mut c,
        "clients/desk/results",
        serde_json::json!({"service_session":SESSION,"client_id":"desk","request_id":action.request_id,"room_id":"room-1","completed_at":"2026-10-17T00:00:16Z","status":"completed","detail":"","state":{"room_id":"room-1","volume":20,"mute":true}}),
        16_000,
    );
    assert!(
        !late
            .iter()
            .any(|e| matches!(e, Effect::Log("Command: completed")))
    );
    assert!(sent(step(&mut c, Event::Input(InputEvent::Mute), 16_001)).is_empty());

    for disconnect in [false, true] {
        let mut c = ready();
        let original = sent(step(&mut c, Event::Input(InputEvent::PlayPause), 10))
            .pop()
            .unwrap();
        step(&mut c, Event::Input(InputEvent::Volume(1)), 11);
        if disconnect {
            step(&mut c, Event::Disconnected, 20);
            assert!(sent(step(&mut c, Event::Input(InputEvent::Next), 21)).is_empty());
            snapshots(&mut c, 1000, "2026-10-17T00:00:01Z", false);
            step(&mut c, Event::Subscribed, 1000);
        } else {
            step(&mut c, Event::PublishFailed(original.request_id), 20);
            snapshots(&mut c, 1000, "2026-10-17T00:00:01Z", false);
            step(&mut c, Event::Subscribed, 1000);
        }
        assert!(sent(step(&mut c, Event::Tick, 16_000)).is_empty());
        let next = sent(step(&mut c, Event::Input(InputEvent::Next), 16_001))
            .pop()
            .unwrap();
        assert_ne!(next.request_id, original.request_id);
    }
}
