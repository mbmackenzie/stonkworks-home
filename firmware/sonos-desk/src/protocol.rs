use serde::{Deserialize, Deserializer, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::controller::ControllerConfig;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Online,
    Offline,
}

#[derive(Deserialize)]
pub struct Availability {
    pub service_session: Uuid,
    pub status: Status,
}

#[derive(Deserialize)]
pub struct Snapshot<T> {
    pub service_session: Uuid,
    #[serde(deserialize_with = "timestamp")]
    pub observed_at: i64,
    #[serde(flatten)]
    pub data: T,
}

#[derive(Deserialize)]
pub struct Inventory {
    pub rooms: Vec<Room>,
}
#[derive(Deserialize)]
pub struct Room {
    pub room_id: String,
}
#[derive(Deserialize)]
pub struct State {
    pub state: RoomState,
}
#[derive(Deserialize)]
pub struct RoomStatus {
    pub status: Status,
}
#[derive(Deserialize)]
pub struct RoomState {
    pub room_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub artwork_url: String,
    #[serde(default)]
    pub playback_state: String,
    pub volume: u8,
    pub mute: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultStatus {
    Completed,
    Rejected,
    Uncertain,
}
#[derive(Deserialize)]
pub struct ResultMessage {
    pub service_session: Uuid,
    pub client_id: String,
    pub request_id: Uuid,
    pub room_id: String,
    #[serde(deserialize_with = "timestamp")]
    pub completed_at: i64,
    pub status: ResultStatus,
    pub state: Option<RoomState>,
}

pub enum Observation {
    Service(Availability),
    Inventory(Snapshot<Inventory>),
    State(Snapshot<State>),
    Room(Snapshot<RoomStatus>),
    Result(ResultMessage),
}

impl Observation {
    /// Topic is relative to the configured instance; unknown fields are ignored.
    pub fn parse(
        topic: &str,
        bytes: &[u8],
        config: &ControllerConfig,
    ) -> Result<Self, &'static str> {
        let invalid = || "Invalid MQTT update";
        if bytes.len() > 32 * 1024 {
            return Err(invalid());
        }
        let observation = if topic == "availability" {
            Self::Service(serde_json::from_slice(bytes).map_err(|_| invalid())?)
        } else if topic == "rooms" {
            Self::Inventory(serde_json::from_slice(bytes).map_err(|_| invalid())?)
        } else if topic == format!("rooms/{}/state", config.room_id) {
            let snapshot: Snapshot<State> = serde_json::from_slice(bytes).map_err(|_| invalid())?;
            if snapshot.data.state.room_id != config.room_id || snapshot.data.state.volume > 100 {
                return Err(invalid());
            }
            Self::State(snapshot)
        } else if topic == format!("rooms/{}/availability", config.room_id) {
            Self::Room(serde_json::from_slice(bytes).map_err(|_| invalid())?)
        } else if topic == format!("clients/{}/results", config.client_id) {
            let result: ResultMessage = serde_json::from_slice(bytes).map_err(|_| invalid())?;
            if matches!(result.status, ResultStatus::Completed) && result.state.is_none()
                || result
                    .state
                    .as_ref()
                    .is_some_and(|s| s.room_id != config.room_id || s.volume > 100)
            {
                return Err(invalid());
            }
            Self::Result(result)
        } else {
            return Err("Unexpected MQTT topic");
        };
        Ok(observation)
    }
}

fn timestamp<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let text = String::deserialize(d)?;
    OffsetDateTime::parse(&text, &Rfc3339)
        .map(|time| (time.unix_timestamp_nanos() / 1_000_000) as i64)
        .map_err(|_| serde::de::Error::custom("Invalid timestamp"))
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Value {
    Volume(i16),
    Mute(bool),
}

#[derive(Serialize)]
pub struct Command {
    pub request_id: Uuid,
    pub client_id: String,
    pub service_session: Uuid,
    pub command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip)]
    pub topic: String,
    #[serde(skip)]
    pub deadline_ms: u64,
}

impl Command {
    pub fn payload(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
    pub fn expiry_seconds(&self, now_ms: u64) -> Option<u32> {
        // Round down; never renew the original deadline or publish expiry zero (unlimited).
        let seconds = self.deadline_ms.saturating_sub(now_ms) / 1000;
        (seconds > 0).then_some(seconds.min(5) as u32)
    }
}

/// One bounded MQTT message; continuation chunks must be contiguous.
#[derive(Default)]
pub struct Assembly {
    partial: Option<(String, Vec<u8>, usize)>,
}
impl Assembly {
    pub fn push(
        &mut self,
        topic: Option<&str>,
        bytes: &[u8],
        total: usize,
        offset: usize,
    ) -> Result<Option<(String, Vec<u8>)>, &'static str> {
        if total > 32 * 1024 || offset > total || bytes.len() > total - offset {
            self.partial = None;
            return Err("MQTT payload exceeds limit");
        }
        if offset == 0 {
            if self.partial.is_some() {
                return Err("Interrupted MQTT payload");
            }
            let topic = topic
                .filter(|t| t.len() <= 256)
                .ok_or("Invalid MQTT topic")?;
            self.partial = Some((topic.into(), Vec::with_capacity(total), total));
        }
        let (saved_topic, data, size) = self.partial.as_mut().ok_or("Missing MQTT first chunk")?;
        if *size != total || data.len() != offset || topic.is_some_and(|t| t != saved_topic) {
            return Err("Invalid MQTT chunk order");
        }
        data.extend_from_slice(bytes);
        Ok(if data.len() == total {
            self.partial.take().map(|(topic, data, _)| (topic, data))
        } else {
            None
        })
    }
}
