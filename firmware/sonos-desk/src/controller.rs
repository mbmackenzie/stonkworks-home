use crate::inputs::InputEvent;
use crate::protocol::{Command, Observation, ResultStatus, Status, Value};
use uuid::Uuid;

pub struct ControllerConfig {
    pub instance: String,
    pub client_id: String,
    pub room_id: String,
}
#[derive(Clone, Copy)]
pub struct Time {
    pub ms: u64,
    pub utc_ms: Option<i64>,
}
pub enum Event {
    Input(InputEvent),
    Tick,
    Disconnected,
    Subscribed,
    Observation(Observation),
    PublishFailed(Uuid),
}
pub enum Effect {
    Publish(Command),
    Ready(bool),
    Log(&'static str),
}

#[derive(Clone, Copy)]
struct Stamp {
    session: Uuid,
    observed: i64,
    received_ms: u64,
    age_ms: u64,
}
impl Stamp {
    fn new(session: Uuid, observed: i64, now: Time) -> Option<Self> {
        let utc = now.utc_ms?;
        if observed > utc.saturating_add(5000) {
            return None;
        }
        Some(Self {
            session,
            observed,
            received_ms: now.ms,
            age_ms: utc.saturating_sub(observed).max(0) as u64,
        })
    }
    fn fresh(self, session: Uuid, now: Time) -> bool {
        self.session == session
            && now.utc_ms.is_some()
            && self
                .age_ms
                .saturating_add(now.ms.saturating_sub(self.received_ms))
                <= 45_000
    }
}
struct Observed<T> {
    stamp: Stamp,
    value: T,
}
struct Pending {
    id: Uuid,
    deadline_ms: u64,
}
struct MuteGuard {
    id: Uuid,
    observed: i64,
}

pub struct Controller {
    pub config: ControllerConfig,
    subscribed: bool,
    session: Option<Uuid>,
    inventory: Option<Observed<bool>>,
    room: Option<Observed<bool>>,
    state: Option<Observed<bool>>,
    ready: bool,
    pending: [Option<Pending>; 4],
    volume: Option<(u64, i16)>,
    mute: Option<MuteGuard>,
}

impl Controller {
    pub fn new(config: ControllerConfig) -> Self {
        Self {
            config,
            subscribed: false,
            session: None,
            inventory: None,
            room: None,
            state: None,
            ready: false,
            pending: [const { None }; 4],
            volume: None,
            mute: None,
        }
    }

    pub fn handle(
        &mut self,
        event: Event,
        now: Time,
        next_id: &mut dyn FnMut() -> Uuid,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        match &event {
            Event::Disconnected | Event::PublishFailed(_) => {
                self.subscribed = false;
                self.session = None;
                self.inventory = None;
                self.room = None;
                self.state = None;
            }
            Event::Subscribed => self.subscribed = true,
            _ => {}
        }
        for pending in &mut self.pending {
            if pending.as_ref().is_some_and(|p| now.ms >= p.deadline_ms) {
                *pending = None;
                effects.push(Effect::Log("Command: outcome uncertain; no retry"));
            }
        }
        let input = match event {
            Event::Observation(observation) => {
                self.observe(observation, now, &mut effects);
                None
            }
            Event::Input(input) => Some(input),
            _ => None,
        };
        self.update_ready(now, &mut effects);
        if self.ready {
            if let Some((deadline, delta)) = self.volume
                && now.ms >= deadline
            {
                self.volume = None;
                if delta != 0 {
                    self.send(
                        "volume_relative",
                        Some(Value::Volume(delta)),
                        now,
                        next_id,
                        &mut effects,
                    );
                }
            }
            if let Some(input) = input {
                match input {
                    InputEvent::Volume(delta) => {
                        let batch = self.volume.get_or_insert((now.ms + 100, 0));
                        batch.1 = (batch.1 + i16::from(delta)).clamp(-100, 100);
                    }
                    InputEvent::Mute => {
                        if self.mute.is_none() {
                            let target = !self.state.as_ref().unwrap().value;
                            self.send(
                                "mute",
                                Some(Value::Mute(target)),
                                now,
                                next_id,
                                &mut effects,
                            );
                        }
                    }
                    input => self.send(
                        match input {
                            InputEvent::Previous => "previous",
                            InputEvent::PlayPause => "play_pause",
                            _ => "next",
                        },
                        None,
                        now,
                        next_id,
                        &mut effects,
                    ),
                }
            }
        }
        effects
    }

    fn update_ready(&mut self, now: Time, effects: &mut Vec<Effect>) {
        let ready = self.subscribed
            && self.session.is_some_and(|session| {
                self.inventory
                    .as_ref()
                    .is_some_and(|x| x.value && x.stamp.fresh(session, now))
                    && self
                        .room
                        .as_ref()
                        .is_some_and(|x| x.value && x.stamp.fresh(session, now))
                    && self
                        .state
                        .as_ref()
                        .is_some_and(|x| x.stamp.fresh(session, now))
            });
        if !ready {
            self.volume = None;
            if self.pending.iter().any(Option::is_some) {
                effects.push(Effect::Log("Command: outcome uncertain; no retry"));
            }
            self.pending.fill_with(|| None);
            self.mute = None;
        }
        if ready != self.ready {
            self.ready = ready;
            effects.push(Effect::Ready(ready));
        }
    }

    fn send(
        &mut self,
        command: &'static str,
        value: Option<Value>,
        now: Time,
        next_id: &mut dyn FnMut() -> Uuid,
        effects: &mut Vec<Effect>,
    ) {
        let Some(slot) = self.pending.iter_mut().find(|slot| slot.is_none()) else {
            effects.push(Effect::Log("Command: busy; input dropped"));
            return;
        };
        let id = next_id();
        *slot = Some(Pending {
            id,
            deadline_ms: now.ms + 15_000,
        });
        if command == "mute" {
            self.mute = Some(MuteGuard {
                id,
                observed: self.state.as_ref().unwrap().stamp.observed,
            });
        }
        effects.push(Effect::Publish(Command {
            request_id: id,
            client_id: self.config.client_id.clone(),
            service_session: self.session.unwrap(),
            command,
            value,
            topic: format!(
                "stonkworks/v1/sonos-desk/{}/rooms/{}/command",
                self.config.instance, self.config.room_id
            ),
            deadline_ms: now.ms + 5000,
        }));
    }

    fn observe(&mut self, observation: Observation, now: Time, effects: &mut Vec<Effect>) {
        match observation {
            Observation::Service(service) => {
                let session =
                    matches!(service.status, Status::Online).then_some(service.service_session);
                if self.session.is_some() && self.session != session {
                    self.inventory = None;
                    self.room = None;
                    self.state = None;
                    self.volume = None;
                    self.pending.fill_with(|| None);
                    self.mute = None;
                }
                self.session = session;
            }
            Observation::Inventory(snapshot) => {
                if let Some(stamp) = Stamp::new(snapshot.service_session, snapshot.observed_at, now)
                {
                    let present = snapshot
                        .data
                        .rooms
                        .iter()
                        .any(|room| room.room_id == self.config.room_id);
                    replace(&mut self.inventory, stamp, present);
                }
            }
            Observation::Room(snapshot) => {
                if let Some(stamp) = Stamp::new(snapshot.service_session, snapshot.observed_at, now)
                {
                    replace(
                        &mut self.room,
                        stamp,
                        matches!(snapshot.data.status, Status::Online),
                    );
                }
            }
            Observation::State(snapshot) => {
                if let Some(stamp) = Stamp::new(snapshot.service_session, snapshot.observed_at, now)
                {
                    self.confirm(stamp, snapshot.data.state.mute, now);
                }
            }
            Observation::Result(result) => {
                if Some(result.service_session) != self.session
                    || result.client_id != self.config.client_id
                    || result.room_id != self.config.room_id
                {
                    return;
                }
                let Some(slot) = self
                    .pending
                    .iter_mut()
                    .find(|slot| slot.as_ref().is_some_and(|p| p.id == result.request_id))
                else {
                    return;
                };
                *slot = None;
                match result.status {
                    ResultStatus::Completed => {
                        if let (Some(state), Some(stamp)) = (
                            result.state,
                            Stamp::new(result.service_session, result.completed_at, now),
                        ) {
                            // A result updates state, but only its own request may
                            // release a mute guard. It may belong to an older action.
                            replace(&mut self.state, stamp, state.mute);
                        }
                        effects.push(Effect::Log("Command: completed"));
                    }
                    ResultStatus::Rejected => {
                        effects.push(Effect::Log("Command: rejected; no retry"))
                    }
                    ResultStatus::Uncertain => {
                        effects.push(Effect::Log("Command: outcome uncertain; no retry"))
                    }
                }
                if !matches!(result.status, ResultStatus::Uncertain)
                    && self
                        .mute
                        .as_ref()
                        .is_some_and(|guard| guard.id == result.request_id)
                {
                    self.mute = None;
                }
            }
        }
    }

    fn confirm(&mut self, stamp: Stamp, mute: bool, now: Time) {
        if replace(&mut self.state, stamp, mute)
            && self.session.is_some_and(|s| stamp.fresh(s, now))
            && self
                .mute
                .as_ref()
                .is_some_and(|guard| stamp.observed > guard.observed)
        {
            self.mute = None;
        }
    }
}

fn replace<T>(slot: &mut Option<Observed<T>>, stamp: Stamp, value: T) -> bool {
    if slot.as_ref().is_some_and(|old| {
        old.stamp.session == stamp.session && old.stamp.observed >= stamp.observed
    }) {
        return false;
    }
    *slot = Some(Observed { stamp, value });
    true
}
