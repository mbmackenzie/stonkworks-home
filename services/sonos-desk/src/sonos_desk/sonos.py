"""Local Sonos discovery and serialized control; no subscriptions or cloud API."""

import logging
import os
import threading
import time
from urllib.parse import urljoin

import requests
import soco
import soco.config
from soco.exceptions import NotSupportedException, SoCoException, SoCoUPnPException

soco.config.REQUEST_TIMEOUT = 3.0
log = logging.getLogger(__name__)
NETWORK_ERRORS = (requests.RequestException, SoCoException, OSError)


class BackendError(Exception):
    def __init__(self, status, message):
        self.status = status
        super().__init__(message)


class SonosBackend:
    def __init__(self, discover=None, clock=time.monotonic):
        self._discover = discover or soco.discover
        self._clock = clock
        self._lock = threading.RLock()
        self._speakers = []
        self._inventory_at = float("-inf")
        self._discovery_failed = False
        self._states = {}

    def _refresh(self, force=False):
        if not force and self._clock() - self._inventory_at < 60:
            return
        try:
            self._speakers = list(self._discover(timeout=3, interface_addr=os.getenv("SONOS_INTERFACE") or None) or [])
            self._discovery_failed = False
        except NETWORK_ERRORS:
            # Keep the last inventory during a temporary multicast outage.
            self._discovery_failed = True
            log.warning("Sonos discovery failed", exc_info=True)
        self._inventory_at = self._clock()

    def _resolve(self, room):
        self._refresh()
        for attempt in range(2):
            uid_matches = [s for s in self._speakers if s.uid == room]
            matches = uid_matches or [s for s in self._speakers if s.player_name.casefold() == room.casefold()]
            if len(matches) == 1:
                return matches[0]
            if len(matches) > 1:
                raise BackendError(409, "Room name is ambiguous; use its room_id")
            if not attempt:
                self._refresh(force=True)
        if self._discovery_failed:
            raise BackendError(503, "Sonos discovery unavailable")
        raise BackendError(404, "Room not found; see /rooms")

    @staticmethod
    def _error(exc, command=False):
        if isinstance(exc, NotSupportedException) or (
            isinstance(exc, SoCoUPnPException) and str(exc.error_code) in {"701", "711", "712"}
        ):
            return BackendError(409, "Action unavailable for the current source or queue")
        message = "Speaker unavailable"
        if command:
            message += "; command may have taken effect. Read state before retrying"
        return BackendError(503, message)

    def rooms(self):
        with self._lock:
            self._refresh()
            try:
                result = []
                for speaker in self._speakers:
                    coordinator = speaker.group.coordinator
                    result.append(
                        {
                            "room": speaker.player_name,
                            "room_id": speaker.uid,
                            "ip": speaker.ip_address,
                            "coordinator": coordinator.player_name,
                            "coordinator_id": coordinator.uid,
                        }
                    )
                return sorted(result, key=lambda r: (r["room"].casefold(), r["room_id"]))
            except NETWORK_ERRORS as exc:
                raise self._error(exc) from exc

    def health(self):
        # Health checks must never wait on speakers or initiate SSDP discovery.
        return {
            "status": "ok",
            "rooms": len(self._speakers),
            "discovery": "degraded" if self._discovery_failed else "ok",
        }

    def state(self, room, fresh=False):
        with self._lock:
            try:
                speaker = self._resolve(room)
                cached = self._states.get(speaker.uid)
                if not fresh and cached and self._clock() - cached[0] < 1:
                    return dict(cached[1])
                coordinator = speaker.group.coordinator
                track = coordinator.get_current_track_info()
                transport = coordinator.get_current_transport_info()
                try:
                    artwork_url = (
                        urljoin(f"http://{coordinator.ip_address}:1400/", track.get("album_art", ""))
                        if track.get("album_art")
                        else ""
                    )
                except ValueError:
                    artwork_url = ""
                state = {
                    "room": speaker.player_name,
                    "room_id": speaker.uid,
                    "coordinator": coordinator.player_name,
                    "coordinator_id": coordinator.uid,
                    "title": track.get("title", ""),
                    "artist": track.get("artist", ""),
                    "album": track.get("album", ""),
                    "artwork_url": artwork_url,
                    "playback_state": transport["current_transport_state"],
                    "volume": speaker.volume,
                    "mute": speaker.mute,
                    "duration": track.get("duration", ""),
                }
                # At most one entry per discovered speaker.
                self._states = {uid: val for uid, val in self._states.items() if uid in {s.uid for s in self._speakers}}
                self._states[speaker.uid] = (self._clock(), state)
                return dict(state)
            except NETWORK_ERRORS as exc:
                raise self._error(exc) from exc

    def command(self, room, command, value=None):
        with self._lock:
            try:
                speaker = self._resolve(room)
                speaker.zone_group_state.clear_cache()
                coordinator = speaker.group.coordinator
                if command == "volume":
                    speaker.volume = value
                elif command == "volume_relative":
                    speaker.volume = max(0, min(100, speaker.volume + value))
                elif command == "mute":
                    speaker.mute = value
                elif command == "play_pause":
                    playing = coordinator.get_current_transport_info()["current_transport_state"] == "PLAYING"
                    (coordinator.pause if playing else coordinator.play)()
                elif command in {"play", "pause", "next", "previous"}:
                    getattr(coordinator, command)()
                else:
                    raise BackendError(422, "Unknown command")
                # Invalidate every room: a transport command affects the group.
                self._states.clear()
                try:
                    return self.state(speaker.uid, fresh=True)
                except BackendError as exc:
                    raise BackendError(
                        503, "Command sent but state refresh failed; read state before retrying"
                    ) from exc
            except NETWORK_ERRORS as exc:
                raise self._error(exc, command=True) from exc
            finally:
                # An uncertain write may still have changed the speaker.
                self._states.clear()
