use tamer::debounce::{Edge, EdgeDetector};
use tamer::rotary::{EncoderDirection, QuadratureDecoder};

/// Physical inputs, normalized so a pressed button is `true`.
#[derive(Clone, Copy, Default)]
pub struct Levels {
    pub previous: bool,
    pub play_pause: bool,
    pub next: bool,
    pub mute: bool,
    pub a: bool,
    pub b: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InputEvent {
    Previous,
    PlayPause,
    Next,
    Mute,
    Volume(i8),
}

const DEBOUNCE_MS: u64 = 20;

pub struct Inputs {
    buttons: [EdgeDetector; 4],
    encoder: QuadratureDecoder,
    encoder_previous: (bool, bool),
    encoder_rest: (bool, bool),
    syncing: bool,
}

impl Inputs {
    pub fn new(levels: Levels) -> Self {
        Self {
            buttons: levels
                .buttons()
                .map(|pressed| EdgeDetector::new(pressed, DEBOUNCE_MS)),
            encoder: QuadratureDecoder::new(levels.a, levels.b, 4),
            encoder_previous: (levels.a, levels.b),
            encoder_rest: (levels.a, levels.b),
            syncing: false,
        }
    }

    pub fn sample(&mut self, levels: Levels, now_ms: u64) -> Vec<InputEvent> {
        let mut events = Vec::new();
        let actions = [
            InputEvent::Previous,
            InputEvent::PlayPause,
            InputEvent::Next,
            InputEvent::Mute,
        ];
        for ((button, pressed), action) in
            self.buttons.iter_mut().zip(levels.buttons()).zip(actions)
        {
            if button.update(pressed, now_ms) == Some(Edge::Rising) {
                events.push(action);
            }
        }

        let current = (levels.a, levels.b);
        if self.syncing {
            // After reconnect, finish any partial turn without producing an action.
            self.syncing = current != self.encoder_rest;
        } else {
            if current.0 != self.encoder_previous.0 && current.1 != self.encoder_previous.1 {
                self.encoder.reset(); // A two-contact jump breaks the partial cycle.
            }
            if let Some(direction) = self.encoder.update(levels.a, levels.b) {
                // Direction calibrated to the wired KY-040.
                let step = match direction {
                    EncoderDirection::Clockwise => 1,
                    EncoderDirection::CounterClockwise => -1,
                };
                events.push(InputEvent::Volume(step));
            }
        }
        self.encoder_previous = current;
        events
    }

    /// Drop held presses and partial motion when connectivity changes.
    pub fn disarm(&mut self, levels: Levels) {
        self.buttons = levels
            .buttons()
            .map(|pressed| EdgeDetector::new(pressed, DEBOUNCE_MS));
        self.encoder = QuadratureDecoder::new(self.encoder_rest.0, self.encoder_rest.1, 4);
        self.encoder_previous = (levels.a, levels.b);
        self.syncing = self.encoder_previous != self.encoder_rest;
    }
}

impl Levels {
    fn buttons(self) -> [bool; 4] {
        [self.previous, self.play_pause, self.next, self.mute]
    }
}
