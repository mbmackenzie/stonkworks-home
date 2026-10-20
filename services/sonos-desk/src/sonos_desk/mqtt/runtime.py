"""Optional MQTT adapter lifecycle and command-first room scheduling."""

import logging
import threading
import time
from datetime import UTC, datetime
from uuid import uuid4

from sonos_desk.mqtt.commands import CommandProcessor
from sonos_desk.mqtt.polling import StatePoller
from sonos_desk.mqtt.protocol import Publication, Topics, encode_payload
from sonos_desk.mqtt.transport import MqttTransport

log = logging.getLogger(__name__)


def disabled_status():
    return {
        "enabled": False,
        "connected": False,
        "service_session": None,
        "last_connected_at": None,
        "last_poll_completed_at": None,
        "rooms_known": 0,
        "rooms_unavailable": 0,
        "last_error": None,
    }


class MqttAdapter:
    def __init__(
        self,
        backend,
        config,
        *,
        transport_factory=MqttTransport,
        clock=time.monotonic,
        utcnow=lambda: datetime.now(UTC),
    ):
        self.session = str(uuid4())
        self.topics = Topics(config.instance)
        self._stop = threading.Event()
        self._wake = threading.Event()
        self._thread = None
        self._worker_error = None
        self.transport = transport_factory(
            config, self.topics, self.session, self._receive, self._ready, self._disconnected
        )
        self.commands = CommandProcessor(
            backend, self.topics, self.session, self.transport.publish, clock, utcnow, self._command_finished
        )
        self.poller = StatePoller(
            backend,
            self.topics,
            self.session,
            self.transport.publish,
            clock,
            utcnow,
            config.poll_interval,
            self.commands.set_room_ids,
        )

    def _receive(self, message):
        self.commands.accept(message)
        self._wake.set()

    def _ready(self):
        if self._stop.is_set() or self._worker_error:
            return
        self.poller.force_refresh()
        self.commands.set_accepting(True)
        self.transport.publish(
            Publication(
                self.topics.availability,
                encode_payload({"status": "online", "service_session": self.session}),
                True,
                None,
            )
        )
        self._wake.set()

    def _disconnected(self):
        self.commands.set_accepting(False)
        self._wake.set()

    def _command_finished(self, room):
        self.poller.force_refresh(room)

    def _step(self):
        if self.commands.run_one():
            return True
        if self.transport.snapshot()["connected"]:
            return self.poller.poll_one()
        return False

    def _run(self):
        try:
            while not self._stop.is_set():
                self._wake.clear()
                if not self._step():
                    self._wake.wait(0.1)
        except Exception:  # noqa: BLE001 - worker boundary must withdraw availability on unexpected failures
            # Unexpected application failures must not leave an apparently
            # healthy command endpoint accepting work without a worker.
            self._worker_error = "MQTT integration worker failed"
            log.error("MQTT integration worker failed")
            self.commands.set_accepting(False)
            self.transport.publish(
                Publication(
                    self.topics.availability,
                    encode_payload({"status": "offline", "service_session": self.session}),
                    True,
                    None,
                )
            )
        finally:
            self.commands.reject_pending()

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="sonos-mqtt-worker", daemon=True)
        self._thread.start()
        self.transport.start()

    def close(self) -> None:
        self.commands.set_accepting(False)
        self._stop.set()
        self._wake.set()
        if self._thread:
            self._thread.join(timeout=6)
            if self._thread.is_alive():
                self._worker_error = "MQTT worker shutdown exceeded grace period; action outcome may be uncertain"
                log.warning("%s", self._worker_error)
        else:
            self.commands.reject_pending()
        self.transport.close()

    def status(self) -> dict[str, object]:
        polling, transport = self.poller.snapshot(), self.transport.snapshot()
        return {
            "enabled": True,
            "service_session": self.session,
            **polling,
            **transport,
            "connected": bool(transport["connected"] and not self._worker_error),
            "last_error": self._worker_error or transport["last_error"] or polling["last_error"],
        }
