"""Disposable MQTT integration infrastructure, enabled only by explicit opt-in."""

import json
import os
import secrets
import socket
import subprocess
import threading
import time
from collections import deque
from contextlib import contextmanager
from uuid import uuid4

import paho.mqtt.client as mqtt
import pytest
from paho.mqtt.packettypes import PacketTypes
from paho.mqtt.properties import Properties
from sonos_desk.mqtt.config import MqttConfig
from sonos_desk.mqtt.runtime import MqttAdapter
from sonos_desk.mqtt.transport import MqttTransport
from sonos_desk.sonos import SonosBackend
from test_sonos import Speaker


def pytest_collection_modifyitems(items):
    if os.getenv("SONOS_MQTT_INTEGRATION") != "1":
        for item in items:
            if "mqtt_integration" in item.keywords:
                item.add_marker(
                    pytest.mark.skip(reason="Set SONOS_MQTT_INTEGRATION=1 for disposable Docker broker tests")
                )


def until(predicate, timeout=10):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline:
            raise TimeoutError("Timed out waiting for MQTT test condition")
        threading.Event().wait(0.02)


@pytest.fixture
def wait_until():
    return until


def docker(*args):
    return subprocess.run(["docker", *args], check=True, capture_output=True, text=True, timeout=30).stdout.strip()


class Broker:
    image = "eclipse-mosquitto:2.0.22"

    def __init__(self, directory):
        self.name = "sonos-mqtt-test-" + uuid4().hex[:12]
        self.password = secrets.token_urlsafe(24)
        self.directory = directory
        self.endpoint = None

    def create(self):
        d = self.directory
        (d / "data").mkdir()
        (d / "passwords").write_text("app:" + self.password + "\n")
        (d / "passwords").chmod(0o600)
        # -U hashes a temporary plaintext file without a password in argv.
        docker(
            "run",
            "--rm",
            "--user",
            f"{os.getuid()}:{os.getgid()}",
            "--mount",
            f"type=bind,src={d},dst=/work",
            "--entrypoint",
            "mosquitto_passwd",
            self.image,
            "-U",
            "/work/passwords",
        )
        (d / "acl").write_text("user app\ntopic readwrite stonkworks/#\n")
        (d / "mosquitto.conf").write_text(
            "listener 1883\nallow_anonymous false\npassword_file /config/passwords\nacl_file /config/acl\npersistence true\npersistence_location /data/\nautosave_interval 1\nlog_dest stdout\n"
            + ("user root\n" if os.getuid() == 0 else "")
        )
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            host_port = reservation.getsockname()[1]
        docker(
            "run",
            "-d",
            "--name",
            self.name,
            "--user",
            f"{os.getuid()}:{os.getgid()}",
            "--read-only",
            "-p",
            f"127.0.0.1:{host_port}:1883",
            "--mount",
            f"type=bind,src={d},dst=/config,readonly",
            "--mount",
            f"type=bind,src={d / 'data'},dst=/data",
            self.image,
            "mosquitto",
            "-c",
            "/config/mosquitto.conf",
        )
        info = json.loads(docker("inspect", self.name))[0]
        port = int(info["NetworkSettings"]["Ports"]["1883/tcp"][0]["HostPort"])
        self.endpoint = ("127.0.0.1", port)
        self.wait()

    def wait(self):
        assert self.endpoint is not None
        endpoint = self.endpoint

        def listening():
            try:
                with socket.create_connection(endpoint, timeout=0.1):
                    return True
            except OSError:
                return False

        until(listening)

    def stop(self):
        docker("stop", "--time", "2", self.name)

    def start(self):
        docker("start", self.name)
        info = json.loads(docker("inspect", self.name))[0]
        actual_port = int(info["NetworkSettings"]["Ports"]["1883/tcp"][0]["HostPort"])
        assert self.endpoint is not None
        assert actual_port == self.endpoint[1], (self.endpoint[1], actual_port, info["State"]["Status"])
        self.wait()

    def close(self):
        subprocess.run(["docker", "rm", "-f", self.name], capture_output=True, timeout=15, check=False)


@pytest.fixture(scope="session")
def mqtt_broker(tmp_path_factory):
    broker = Broker(tmp_path_factory.mktemp("sonos-mqtt-broker"))
    try:
        broker.create()
        yield broker
    finally:
        broker.close()


class Probe:
    def __init__(self, broker, topic):
        self.broker, self.topic = broker, topic
        self.condition, self.messages = threading.Condition(), deque(maxlen=2048)
        self.ready = threading.Event()
        self.client = mqtt.Client(
            mqtt.CallbackAPIVersion.VERSION2, client_id="probe-" + uuid4().hex, protocol=mqtt.MQTTv5
        )
        self.client.username_pw_set("app", broker.password)
        self.client.on_connect = self._connected
        self.client.on_subscribe = lambda *args: self.ready.set()
        self.client.on_message = self._received

    def _connected(self, client, userdata, flags, reason, properties):
        if not reason.is_failure:
            client.subscribe(self.topic, qos=1)

    def _received(self, client, userdata, message):
        with self.condition:
            self.messages.append(message)
            self.condition.notify_all()

    def __enter__(self):
        self.client.connect(*self.broker.endpoint, clean_start=True)
        self.client.loop_start()
        if not self.ready.wait(5):
            self.__exit__(None, None, None)
            raise TimeoutError("Probe subscription failed")
        return self

    def __exit__(self, *args):
        self.client.disconnect()
        self.client.loop_stop()

    def publish(self, topic, payload, *, expiry=None, retain=False):
        properties = None
        if expiry is not None:
            properties = Properties(PacketTypes.PUBLISH)
            properties.MessageExpiryInterval = expiry
        data = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        info = self.client.publish(topic, data, qos=1, retain=retain, properties=properties)
        info.wait_for_publish(timeout=3)
        assert info.is_published()

    def wait_for_message(self, topic, predicate=lambda m: True, timeout=10):
        deadline = time.monotonic() + timeout
        with self.condition:
            while True:
                for message in self.messages:
                    if message.topic == topic and predicate(message):
                        self.messages.remove(message)
                        return message
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("Timed out waiting for topic " + topic)
                self.condition.wait(remaining)

    def clear(self):
        with self.condition:
            self.messages.clear()


@pytest.fixture
def mqtt_probe(mqtt_broker):
    @contextmanager
    def probe(topic):
        with Probe(mqtt_broker, topic) as p:
            yield p

    return probe


class ArtworklessSpeaker(Speaker):
    def get_current_track_info(self):
        return {**super().get_current_track_info(), "album_art": ""}


class Service:
    def __init__(self, broker):
        self.speaker = ArtworklessSpeaker("Office", "uid-o")
        self.backend = SonosBackend(discover=lambda **kw: [self.speaker])
        self.clients = []
        self.config = MqttConfig(
            broker.endpoint[0], broker.endpoint[1], "app", broker.password, "test-" + uuid4().hex[:12], 0.2, False, None
        )
        self.adapter = self.new_adapter()
        self.base = self.adapter.topics.base
        self.command_topic = self.adapter.topics.room_command("uid-o")
        self.results_topic = self.adapter.topics.results("test")

    def new_adapter(self):
        def factory(**kwargs):
            client = mqtt.Client(**kwargs)
            self.clients.append(client)
            return client

        def transport(*args):
            return MqttTransport(*args, client_factory=factory)

        return MqttAdapter(self.backend, self.config, transport_factory=transport)

    def start(self):
        self.adapter.start()

    def restart_adapter(self):
        self.adapter.close()
        self.adapter = self.new_adapter()
        self.adapter.start()


@pytest.fixture
def service(mqtt_broker):
    service = Service(mqtt_broker)
    try:
        yield service
    finally:
        service.adapter.close()
