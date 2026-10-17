"""Portable MQTT v1 envelopes and topic validation; no network I/O."""

import json
import re
from dataclasses import dataclass
from uuid import UUID

from pydantic import Field, field_validator

from sonos_desk.commands import CommandFields

ID_PATTERN = r"[A-Za-z0-9_-]{1,64}"


def identifier(value: str) -> str:
    if not re.fullmatch(ID_PATTERN, value):
        raise ValueError("Invalid instance or client ID")
    return value


def room_segment(value: str) -> str:
    if not value or len(value) > 256 or any(c in value for c in "/+#") or any(ord(c) < 32 for c in value):
        raise ValueError("Invalid room ID topic segment")
    return value


class CommandRequest(CommandFields):
    request_id: UUID
    client_id: str = Field(pattern="^" + ID_PATTERN + "$")
    service_session: UUID

    @field_validator("request_id", "service_session", mode="before")
    @classmethod
    def uuid_string(cls, value):
        if not isinstance(value, str):
            raise ValueError("UUID must be a string")  # noqa: TRY004 - Pydantic validators require ValueError
        return value


@dataclass(frozen=True)
class IncomingCommand:
    topic: str
    payload: bytes
    retained: bool
    remaining_expiry: int | None
    received_at: float


@dataclass(frozen=True)
class Publication:
    topic: str
    payload: bytes
    retained: bool
    expiry: int | None


class Topics:
    def __init__(self, instance: str = "main"):
        self.base = f"stonkworks/v1/sonos-desk/{identifier(instance)}"
        self.availability = self.base + "/availability"
        self.inventory = self.base + "/rooms"
        self.commands = self.base + "/rooms/+/command"

    def room_state(self, room_id: str) -> str:
        return f"{self.base}/rooms/{room_segment(room_id)}/state"

    def room_availability(self, room_id: str) -> str:
        return f"{self.base}/rooms/{room_segment(room_id)}/availability"

    def room_command(self, room_id: str) -> str:
        return f"{self.base}/rooms/{room_segment(room_id)}/command"

    def command_room(self, topic: str) -> str:
        prefix = self.base + "/rooms/"
        if not topic.startswith(prefix) or not topic.endswith("/command"):
            raise ValueError("Invalid command topic")
        return room_segment(topic[len(prefix) : -len("/command")])

    def results(self, client_id: str) -> str:
        return f"{self.base}/clients/{identifier(client_id)}/results"


def encode_payload(payload: dict[str, object]) -> bytes:
    return json.dumps(payload, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")


def decode_payload(payload: bytes):
    def invalid_constant(value):
        raise ValueError("Non-standard JSON constant")

    return json.loads(payload, parse_constant=invalid_constant)
