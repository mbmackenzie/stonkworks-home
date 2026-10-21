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

struct Button {
    raw: bool,
    stable: bool,
    changed_at: u64,
    armed: bool,
}

impl Button {
    fn new(pressed: bool, now_ms: u64) -> Self {
        Self {
            raw: pressed,
            stable: pressed,
            changed_at: now_ms,
            armed: !pressed,
        }
    }

    fn sample(&mut self, pressed: bool, now_ms: u64) -> bool {
        if pressed != self.raw {
            self.raw = pressed;
            self.changed_at = now_ms;
        }
        if self.raw == self.stable || now_ms.saturating_sub(self.changed_at) < DEBOUNCE_MS {
            return false;
        }
        self.stable = self.raw;
        if !self.stable {
            self.armed = true;
            return false;
        }
        let fire = self.armed;
        self.armed = false;
        fire
    }
}

pub struct Inputs {
    buttons: [Button; 4],
    encoder_previous: u8,
    encoder_rest: u8,
    encoder_steps: i8,
}

impl Inputs {
    pub fn new(levels: Levels, now_ms: u64) -> Self {
        Self {
            buttons: levels.buttons().map(|pressed| Button::new(pressed, now_ms)),
            encoder_previous: levels.encoder(),
            encoder_rest: levels.encoder(),
            encoder_steps: 0,
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
            if button.sample(pressed, now_ms) {
                events.push(action);
            }
        }

        let current = levels.encoder();
        if current != self.encoder_previous {
            // Only one contact changes at a time in a valid quadrature sequence.
            let step = match (self.encoder_previous, current) {
                (0, 1) | (1, 3) | (3, 2) | (2, 0) => 1,
                (1, 0) | (3, 1) | (2, 3) | (0, 2) => -1,
                _ => 0,
            };
            if step == 0 {
                self.encoder_steps = 0;
            } else {
                self.encoder_steps += step;
            }
            self.encoder_previous = current;
            if current == self.encoder_rest {
                if self.encoder_steps.abs() == 4 {
                    events.push(InputEvent::Volume(self.encoder_steps.signum()));
                }
                self.encoder_steps = 0;
            }
        }
        events
    }

    /// Drop held presses and partial motion when connectivity changes.
    pub fn disarm(&mut self, levels: Levels, now_ms: u64) {
        self.buttons = levels.buttons().map(|pressed| Button::new(pressed, now_ms));
        self.encoder_previous = levels.encoder();
        self.encoder_steps = 0;
    }
}

impl Levels {
    fn buttons(self) -> [bool; 4] {
        [self.previous, self.play_pause, self.next, self.mute]
    }

    fn encoder(self) -> u8 {
        (u8::from(self.a) << 1) | u8::from(self.b)
    }
}
