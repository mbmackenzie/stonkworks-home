"""Bounded MQTT command acceptance, dispatch, and duplicate protection."""

import logging
import threading
from collections import deque
from dataclasses import dataclass
from uuid import UUID

from pydantic import ValidationError

from sonos_desk.mqtt.protocol import CommandRequest, Publication, decode_payload, encode_payload
from sonos_desk.sonos import BackendError, CommandExpired

log = logging.getLogger(__name__)


@dataclass
class Record:
    request: CommandRequest
    room_id: str
    deadline: float
    result: dict | None = None
    finished_at: float | None = None

    def matches(self, request, room):
        return self.room_id == room and self.request.model_dump() == request.model_dump()


class CommandProcessor:
    def __init__(self, backend, topics, service_session, publish, clock, utcnow, on_command):
        self.backend, self.topics, self.session = backend, topics, service_session
        self.publish, self.clock, self.utcnow, self.on_command = publish, clock, utcnow, on_command
        self._lock = threading.Lock()
        self._accepting = False
        self._room_ids = frozenset()
        self._records = {}
        self._queue = deque()

    def set_accepting(self, accepting: bool) -> None:
        with self._lock:
            self._accepting = accepting

    def set_room_ids(self, room_ids: frozenset[str]) -> None:
        with self._lock:
            self._room_ids = room_ids

    def _result(self, request, room, status, detail, state=None):
        result = {
            "service_session": self.session,
            "client_id": request["client_id"],
            "request_id": request["request_id"],
            "room_id": room,
            "completed_at": self.utcnow().isoformat().replace("+00:00", "Z"),
            "status": status,
            "detail": detail,
        }
        if state is not None:
            result["state"] = state
        return result

    def _send(self, result):
        self.publish(Publication(self.topics.results(result["client_id"]), encode_payload(result), False, 30))

    def accept(self, message) -> None:
        # Identify a safe response route before full validation; never echo bad JSON.
        try:
            if len(message.payload) > 4096:
                raise ValueError("Oversized payload")
            room = self.topics.command_room(message.topic)
            raw = decode_payload(message.payload)
            if not isinstance(raw, dict) or not isinstance(raw.get("request_id"), str):
                raise TypeError("Invalid envelope")
            raw["request_id"] = str(UUID(raw["request_id"]))
            self.topics.results(raw.get("client_id", ""))
        except (ValueError, TypeError, UnicodeError):
            log.warning("Dropped invalid MQTT command envelope")
            return
        try:
            request = CommandRequest.model_validate(raw)
        except ValidationError:
            self._send(self._result(raw, room, "rejected", "Invalid command envelope"))
            return
        with self._lock:
            now = self.clock()
            self._records = {
                key: rec for key, rec in self._records.items() if rec.finished_at is None or now - rec.finished_at < 60
            }
            reason = None
            if message.retained:
                reason = "Retained commands are forbidden"
            elif type(message.remaining_expiry) is not int or not 1 <= message.remaining_expiry <= 10:
                reason = "Commands require remaining message expiry from 1 to 10 seconds"
            elif str(request.service_session) != self.session:
                reason = "Service session changed; observe state before another action"
            elif not self._accepting:
                reason = "MQTT command interface unavailable"
            if reason:
                self._send(self._result(raw, room, "rejected", reason))
                return
            key = (request.client_id, str(request.request_id))
            record = self._records.get(key)
            if record:
                if not record.matches(request, room):
                    self._send(self._result(raw, room, "rejected", "Conflicting request ID reuse"))
                elif record.result is not None:
                    self._send(record.result)
                return
            if room not in self._room_ids:
                reason = "Unknown stable room ID"
            elif now >= message.received_at + message.remaining_expiry:
                reason = "Command expired before acceptance"
            elif len(self._queue) >= 16 or len(self._records) >= 1024:
                reason = "MQTT command capacity exceeded"
            if reason:
                self._send(self._result(raw, room, "rejected", reason))
                return
            self._records[key] = Record(request, room, message.received_at + message.remaining_expiry)
            self._queue.append(key)

    def _finish(self, record, status, detail, state=None):
        with self._lock:
            record.result = self._result(record.request.model_dump(mode="json"), record.room_id, status, detail, state)
            record.finished_at = self.clock()
            self._send(record.result)

    def run_one(self) -> bool:
        with self._lock:
            if not self._queue:
                return False
            record = self._records[self._queue.popleft()]
            accepting = self._accepting
        if not accepting or self.clock() >= record.deadline:
            self._finish(record, "rejected", "Command expired or interface unavailable")
            return True
        try:
            state = self.backend.command(
                record.room_id,
                record.request.command,
                record.request.value,
                deadline=record.deadline,
                room_id_only=True,
            )
        except CommandExpired:
            self._finish(record, "rejected", "Command expired before speaker dispatch")
        except BackendError as exc:
            status = "uncertain" if exc.status == 503 else "rejected"
            self._finish(record, status, str(exc))
        else:
            self._finish(record, "completed", "Command completed", state)
        finally:
            self.on_command(record.room_id)
        return True

    def reject_pending(self) -> None:
        self.set_accepting(False)
        while self.run_one():
            pass
