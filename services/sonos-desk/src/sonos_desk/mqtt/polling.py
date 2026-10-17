"""Fair room polling and replaceable retained MQTT snapshots."""

import threading
from collections import deque

from sonos_desk.mqtt.protocol import Publication, encode_payload, room_segment
from sonos_desk.sonos import BackendError


class StatePoller:
    def __init__(self, backend, topics, service_session, publish, clock, utcnow, poll_interval, on_inventory):
        self.backend, self.topics, self.session = backend, topics, service_session
        self.publish, self.clock, self.utcnow = publish, clock, utcnow
        self.poll_interval, self.on_inventory = poll_interval, on_inventory
        self._lock = threading.RLock()
        self._due = {}
        self._order = deque()
        self._inventory_due = float("-inf")
        self._inventory = []
        self._inventory_observed = None
        self._sent = {}
        self._online = {}
        self._last_poll = None
        self._last_error = None

    def _time(self):
        return self.utcnow().isoformat().replace("+00:00", "Z")

    def _emit(self, topic, payload, content, interval):
        previous = self._sent.get(topic)
        now = self.clock()
        if previous is None or previous[0] != content or now - previous[1] >= interval:
            envelope = {"service_session": self.session, **payload}
            if self.publish(Publication(topic, encode_payload(envelope), True, 90)):
                self._sent[topic] = (content, now)
            else:
                self._last_error = "Snapshot publication unavailable"

    def force_refresh(self, room_id: str | None = None) -> None:
        with self._lock:
            if room_id is None:
                self._sent.clear()
                self._inventory_due = float("-inf")
                for room in self._due:
                    self._due[room] = float("-inf")
            elif room_id in self._due:
                self._due[room_id] = float("-inf")
                self._order.remove(room_id)
                self._order.appendleft(room_id)
                self._sent.pop(self.topics.room_state(room_id), None)

    def _refresh_inventory(self):
        ids = self.backend.room_ids()
        ids = [room for room in ids if self._safe_room(room)]
        error = None
        try:
            rooms = self.backend.rooms()
        except BackendError:
            rooms = None
            error = "Speaker inventory unavailable"
        discovery = self.backend.health()["discovery"] if error is None else "degraded"
        with self._lock:
            self._inventory_due = self.clock() + 30
            if rooms is not None:
                self._inventory = [r for r in rooms if r["room_id"] in ids]
                self._inventory_observed = self._time()
            self._last_error = error
            for room in self._due.keys() - set(ids):
                for topic in (self.topics.room_state(room), self.topics.room_availability(room)):
                    self.publish(Publication(topic, b"", True, None))
                    self._sent.pop(topic, None)
                self._order.remove(room)
                self._online.pop(room, None)
            self._due = {room: self._due.get(room, float("-inf")) for room in ids}
            for room in ids:
                if room not in self._order:
                    self._order.append(room)
            self.on_inventory(frozenset(ids))
            # If there has never been a successful metadata inventory, publish
            # an empty degraded snapshot, never manufacture room metadata.
            payload = {
                "observed_at": self._inventory_observed or self._time(),
                "discovery": discovery,
                "rooms": self._inventory,
            }
            self._emit(self.topics.inventory, payload, (discovery, self._inventory), 30)

    @staticmethod
    def _safe_room(room):
        try:
            room_segment(room)
        except ValueError:
            return False
        return True

    def poll_one(self) -> bool:
        with self._lock:
            inventory_due = self.clock() >= self._inventory_due
            room = None
            if not inventory_due:
                for _ in range(len(self._order)):
                    candidate = self._order.popleft()
                    self._order.append(candidate)
                    if self.clock() >= self._due[candidate]:
                        room = candidate
                        break
        if inventory_due:
            self._refresh_inventory()
            return True
        if room is None:
            return False
        try:
            state = self.backend.state(room, fresh=True, room_id_only=True)
        except BackendError:
            state = None
        with self._lock:
            observed_at = self._time()
            self._last_poll = observed_at
            self._due[room] = self.clock() + self.poll_interval
            self._online[room] = state is not None
            if state is not None:
                self._emit(self.topics.room_state(room), {"observed_at": observed_at, "state": state}, state, 15)
            self._emit(
                self.topics.room_availability(room),
                {"observed_at": observed_at, "status": "online" if state else "offline"},
                state is not None,
                15,
            )
        return True

    def snapshot(self) -> dict[str, object]:
        with self._lock:
            return {
                "rooms_known": len(self._due),
                "rooms_unavailable": sum(not self._online.get(room, False) for room in self._due),
                "last_poll_completed_at": self._last_poll,
                "last_error": self._last_error,
            }
