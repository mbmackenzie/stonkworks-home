"""MQTT 5 transport with bounded queues and application-owned reconnects."""

import logging
import math
import random
import ssl
import threading
import time
from collections import deque
from datetime import UTC, datetime

import paho.mqtt.client as mqtt
from paho.mqtt.packettypes import PacketTypes
from paho.mqtt.properties import Properties
from paho.mqtt.reasoncodes import ReasonCode
from paho.mqtt.subscribeoptions import SubscribeOptions

from sonos_desk.mqtt.protocol import IncomingCommand, Publication, encode_payload

log = logging.getLogger(__name__)


class MqttTransport:
    def __init__(
        self,
        config,
        topics,
        service_session,
        on_message,
        on_ready,
        on_disconnect,
        *,
        client_factory=mqtt.Client,
        clock=time.monotonic,
        utcnow=lambda: datetime.now(UTC),
        jitter=random.uniform,
    ):
        self.config, self.topics, self.session = config, topics, service_session
        self.on_message, self.on_ready, self.on_disconnect = on_message, on_ready, on_disconnect
        self._factory, self.clock, self.utcnow, self.jitter = client_factory, clock, utcnow, jitter
        self._lock = threading.Lock()
        self._queue = deque()
        self._inflight = {}
        self._generation = 0
        self._ready = False
        self._last_connected = None
        self._last_error = None
        self._stop = threading.Event()
        self._failed = threading.Event()
        self._offline_acked = threading.Event()
        self._thread = None

    def _failure(self, message):
        with self._lock:
            self._last_error = message
        self._failed.set()
        log.warning("%s", message)

    def _new_client(self):
        with self._lock:
            self._generation += 1
            generation = self._generation
            self._ready = False
            self._queue.clear()
            self._inflight.clear()
        self._failed.clear()
        self._offline_acked.clear()
        client = self._factory(
            callback_api_version=mqtt.CallbackAPIVersion.VERSION2,
            client_id=f"sonos-desk-{self.config.instance}",
            protocol=mqtt.MQTTv5,
            reconnect_on_failure=False,
        )
        client.connect_timeout = 3
        client.username_pw_set(self.config.username, self.config.password)
        client.max_queued_messages_set(128)
        client.max_inflight_messages_set(20)
        if self.config.tls:
            client.tls_set_context(ssl.create_default_context(cafile=self.config.ca_file))
        client.will_set(
            self.topics.availability,
            encode_payload({"status": "offline", "service_session": self.session}),
            qos=1,
            retain=True,
        )

        def current():
            return generation == self._generation

        def on_connect(c, userdata, flags, reason, properties):
            if not current():
                return
            if reason.is_failure:
                self._failure("MQTT connection refused; check authentication and broker access")
                return
            rc, _mid = c.subscribe(
                self.topics.commands, options=SubscribeOptions(qos=1, retainAsPublished=True, retainHandling=2)
            )
            if rc != mqtt.MQTT_ERR_SUCCESS:
                self._failure("MQTT command subscription failed")

        def on_subscribe(c, userdata, mid, reasons, properties):
            if not current():
                return
            if not reasons or any(r.is_failure for r in reasons):
                self._failure("MQTT command subscription denied")
                return
            with self._lock:
                self._ready = True
                self._last_connected = self.utcnow().isoformat().replace("+00:00", "Z")
                self._last_error = None
            self.on_ready()

        def on_disconnect(c, userdata, flags, reason, properties):
            if current():
                self._disconnected()
                self._failed.set()

        def on_message(c, userdata, message):
            if current() and self._ready:
                expiry = getattr(message.properties, "MessageExpiryInterval", None)
                self.on_message(
                    IncomingCommand(message.topic, bytes(message.payload), bool(message.retain), expiry, self.clock())
                )

        def on_publish(c, userdata, mid, reason, properties):
            if not current():
                return
            with self._lock:
                publication = self._inflight.pop(mid, None)
            if reason is not None and reason.is_failure:
                self._failure("MQTT publication denied")
                return
            if publication and publication.topic == self.topics.availability and b'"offline"' in publication.payload:
                self._offline_acked.set()

        client.on_connect, client.on_subscribe = on_connect, on_subscribe
        client.on_disconnect, client.on_message, client.on_publish = on_disconnect, on_message, on_publish
        return client

    def _disconnected(self):
        with self._lock:
            was_ready = self._ready
            self._ready = False
            self._queue.clear()
            self._inflight.clear()
        if was_ready:
            self.on_disconnect()

    def publish(self, publication: Publication) -> bool:
        with self._lock:
            if not self._ready or self._stop.is_set():
                return False
            if publication.retained:
                for n, (old, _at) in enumerate(self._queue):
                    if old.retained and old.topic == publication.topic:
                        self._queue[n] = (publication, self.clock())
                        return True
            if len(self._queue) >= 128:
                self._last_error = "MQTT outgoing queue full"
                return False
            self._queue.append((publication, self.clock()))
            return True

    def _flush(self, client):
        while True:
            with self._lock:
                if not self._ready or not self._queue or len(self._inflight) >= 20:
                    return
                publication, queued_at = self._queue.popleft()
            properties = None
            if publication.expiry is not None:
                remaining = publication.expiry - (self.clock() - queued_at)
                if remaining <= 0:
                    continue
                properties = Properties(PacketTypes.PUBLISH)
                properties.MessageExpiryInterval = math.ceil(remaining)
            info = client.publish(
                publication.topic, publication.payload, qos=1, retain=publication.retained, properties=properties
            )
            if info.rc != mqtt.MQTT_ERR_SUCCESS:
                self._failure("MQTT publication failed")
                return
            with self._lock:
                self._inflight[info.mid] = publication

    def _retry_delay(self, attempt):
        ceiling = min(30, 2 ** min(attempt, 5))
        return self.jitter(max(1, ceiling / 2), ceiling)

    def _run(self):
        attempt = 0
        while not self._stop.is_set():
            client = None
            try:
                client = self._new_client()
                properties = Properties(PacketTypes.CONNECT)
                properties.SessionExpiryInterval = 0
                client.connect(
                    self.config.host, self.config.port, keepalive=15, clean_start=True, properties=properties
                )
                while not self._stop.is_set() and not self._failed.is_set():
                    rc = client.loop(timeout=0.1)
                    if rc != mqtt.MQTT_ERR_SUCCESS:
                        break
                    if self._ready:
                        attempt = 0
                        self._flush(client)
            except (OSError, ValueError):
                self._failure("MQTT connection failed; check broker, credentials, and TLS trust")
            finally:
                self._disconnected()
                if client is not None:
                    # Normal disconnect suppresses the Will. Request it unless
                    # offline availability was positively acknowledged.
                    reason = ReasonCode(PacketTypes.DISCONNECT, identifier=0 if self._offline_acked.is_set() else 4)
                    try:
                        client.disconnect(reasoncode=reason)
                    finally:
                        sock = client.socket()
                        if sock is not None:
                            sock.close()
                    # The discarded client never resumes its outgoing messages.
            if not self._stop.is_set():
                self._stop.wait(self._retry_delay(attempt))
                attempt += 1

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="sonos-mqtt-network", daemon=True)
        self._thread.start()

    def close(self) -> None:
        self._offline_acked.clear()
        with self._lock:
            self._queue.clear()
        offline = Publication(
            self.topics.availability, encode_payload({"status": "offline", "service_session": self.session}), True, None
        )
        if self.publish(offline):
            self._offline_acked.wait(1)
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)
            if self._thread.is_alive():
                log.warning("MQTT network shutdown exceeded grace period")

    def snapshot(self) -> dict[str, object]:
        with self._lock:
            return {"connected": self._ready, "last_connected_at": self._last_connected, "last_error": self._last_error}
