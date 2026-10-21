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
