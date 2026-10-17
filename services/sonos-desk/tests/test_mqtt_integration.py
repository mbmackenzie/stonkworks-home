"""Opt-in disposable-broker checks; never targets household services."""

import json
import socket
import threading
from uuid import uuid4

import pytest
import requests
from fastapi.testclient import TestClient

pytestmark = pytest.mark.mqtt_integration


def command(service, **changes):
    return {
        "request_id": str(uuid4()),
        "client_id": "test",
        "service_session": service.adapter.session,
        "command": "next",
        **changes,
    }


def test_publish_state_and_execute_one_command(service, mqtt_probe):
    with mqtt_probe(service.base + "/#") as probe:
        service.start()
        state = probe.wait_for_message(service.base + "/rooms/uid-o/state")
        assert json.loads(state.payload)["state"]["room"] == "Office"
        from pathlib import Path

        import jsonschema

        schema = json.loads((Path(__file__).resolve().parents[3] / "protocols/v1/sonos-desk.schema.json").read_text())

        def validate(message, family):
            jsonschema.Draft202012Validator(
                {**schema, "$ref": "#/$defs/" + family}, format_checker=jsonschema.FormatChecker()
            ).validate(json.loads(message.payload))

        validate(state, "state")
        for suffix, family in (
            ("/availability", "availability"),
            ("/rooms", "inventory"),
            ("/rooms/uid-o/availability", "room_availability"),
        ):
            validate(probe.wait_for_message(service.base + suffix), family)
        with mqtt_probe(service.base + "/rooms/uid-o/state") as late:
            assert late.wait_for_message(service.base + "/rooms/uid-o/state").retain
        payload = command(service)
        probe.publish(service.command_topic, payload, expiry=10)
        result = probe.wait_for_message(service.results_topic)
        assert json.loads(result.payload)["status"] == "completed"
        validate(result, "result")
        probe.publish(service.command_topic, payload, expiry=10)
        duplicate = probe.wait_for_message(service.results_topic)
        assert duplicate.payload == result.payload and service.speaker.skips == 1
        with mqtt_probe(service.results_topic) as late, pytest.raises(TimeoutError):
            late.wait_for_message(service.results_topic, timeout=0.4)


def test_stored_retained_commands_suppressed_and_live_retained_rejected(service, mqtt_probe):
    with mqtt_probe(service.base + "/#") as probe:
        probe.publish(service.command_topic, command(service), expiry=10, retain=True)
        service.start()
        probe.wait_for_message(service.base + "/rooms/uid-o/state")
        assert service.speaker.skips == 0
        probe.publish(service.command_topic, command(service), expiry=10, retain=True)
        assert json.loads(probe.wait_for_message(service.results_topic).payload)["status"] == "rejected"
        assert service.speaker.skips == 0
        probe.publish(service.command_topic, b"", retain=True)


def test_delivered_command_expires_while_waiting_for_backend_lock(service, mqtt_probe, monkeypatch):
    with mqtt_probe(service.base + "/#") as probe:
        service.start()
        probe.wait_for_message(service.base + "/rooms/uid-o/state")
        entered = threading.Event()
        original = service.backend.command

        def waiting(*args, **kwargs):
            entered.set()
            return original(*args, **kwargs)

        monkeypatch.setattr(service.backend, "command", waiting)
        with service.backend._lock:
            probe.publish(service.command_topic, command(service), expiry=1)
            assert entered.wait(3)
            threading.Event().wait(1.2)
        assert json.loads(probe.wait_for_message(service.results_topic).payload)["status"] == "rejected"
        assert service.speaker.skips == 0


def test_uncertain_write_and_process_restart(service, mqtt_probe, monkeypatch):
    with mqtt_probe(service.base + "/#") as probe:
        service.start()
        probe.wait_for_message(service.base + "/rooms/uid-o/state")

        def uncertain():
            service.speaker.skips += 1
            raise requests.Timeout()

        monkeypatch.setattr(service.speaker, "next", uncertain)
        old = command(service)
        probe.publish(service.command_topic, old, expiry=10)
        assert json.loads(probe.wait_for_message(service.results_topic).payload)["status"] == "uncertain"
        probe.publish(service.command_topic, old, expiry=10)
        assert json.loads(probe.wait_for_message(service.results_topic).payload)["status"] == "uncertain"
        assert service.speaker.skips == 1
        service.restart_adapter()
        probe.wait_for_message(
            service.base + "/availability",
            lambda m: (
                json.loads(m.payload).get("service_session") == service.adapter.session
                and json.loads(m.payload)["status"] == "online"
            ),
        )
        probe.publish(service.command_topic, old, expiry=10)
        assert json.loads(probe.wait_for_message(service.results_topic).payload)["status"] == "rejected"
        assert service.speaker.skips == 1


def test_last_will_on_abrupt_socket_loss(service, mqtt_probe):
    with mqtt_probe(service.base + "/#") as probe:
        service.start()
        probe.wait_for_message(service.base + "/rooms/uid-o/state")
        service.clients[-1].socket().shutdown(socket.SHUT_RDWR)
        probe.wait_for_message(service.base + "/availability", lambda m: json.loads(m.payload)["status"] == "offline")
        probe.wait_for_message(service.base + "/availability", lambda m: json.loads(m.payload)["status"] == "online")


def test_broker_restart_keeps_http_and_rebuilds_snapshots(service, mqtt_probe, mqtt_broker, monkeypatch):
    import sonos_desk.app as module

    monkeypatch.setattr(module, "MqttAdapter", lambda *args: service.adapter)
    with (
        TestClient(module.create_app(service.backend, mqtt_config=service.config)) as http,
        mqtt_probe(service.base + "/#") as probe,
    ):
        probe.wait_for_message(service.base + "/rooms/uid-o/state")
        mqtt_broker.stop()
        assert http.get("/health").status_code == 200
        assert http.get("/rooms").json()[0]["room_id"] == "uid-o"
        assert http.get("/state?room=uid-o").status_code == 200
        assert http.get("/frame?room=uid-o").status_code == 200
        service.speaker.title = "After broker restart"
        probe.clear()
        mqtt_broker.start()
        state = probe.wait_for_message(
            service.base + "/rooms/uid-o/state",
            lambda m: json.loads(m.payload)["state"]["title"] == "After broker restart",
            timeout=15,
        )
        assert json.loads(state.payload)["service_session"] == service.adapter.session
        assert http.get("/mqtt/status").json()["connected"]


def test_broker_expiry_removes_retained_snapshots(mqtt_probe):
    topic = "stonkworks/test/expiry/" + uuid4().hex
    with mqtt_probe(topic) as probe:
        probe.publish(topic, {"test": True}, expiry=1, retain=True)
        probe.wait_for_message(topic)
    threading.Event().wait(1.2)
    with mqtt_probe(topic) as late, pytest.raises(TimeoutError):
        late.wait_for_message(topic, timeout=0.4)


def test_bad_credentials_stay_offline(service, wait_until):
    from dataclasses import replace

    from sonos_desk.mqtt.runtime import MqttAdapter

    adapter = MqttAdapter(service.backend, replace(service.config, password="incorrect-password"))
    try:
        adapter.start()
        wait_until(lambda: adapter.status()["last_error"] is not None)
        assert not adapter.status()["connected"]
        assert "incorrect-password" not in str(adapter.status())
    finally:
        adapter.close()


def test_example_client_controls_simulated_speaker_once(service, mqtt_probe, tmp_path):
    import os
    import subprocess
    import sys
    from pathlib import Path

    with mqtt_probe(service.base + "/#") as probe:
        service.start()
        probe.wait_for_message(service.base + "/rooms/uid-o/state")
        secret = tmp_path / "client-password"
        secret.write_text(service.config.password)
        secret.chmod(0o600)
        env = {
            **os.environ,
            "SONOS_MQTT_HOST": service.config.host,
            "SONOS_MQTT_PORT": str(service.config.port),
            "SONOS_MQTT_USERNAME": "app",
            "SONOS_MQTT_PASSWORD_FILE": str(secret),
        }
        example = Path(__file__).parents[1] / "examples/mqtt_client.py"
        result = subprocess.run(
            [
                sys.executable,
                str(example),
                "--instance",
                service.config.instance,
                "--room-id",
                "uid-o",
                "--command",
                "volume_relative",
                "--value",
                "1",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["status"] == "completed"
        assert service.speaker.volume == 41


def test_anonymous_connection_rejected(mqtt_broker):
    import paho.mqtt.client as mqtt

    finished, outcomes = threading.Event(), []
    client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, protocol=mqtt.MQTTv5, reconnect_on_failure=False)

    def connected(c, userdata, flags, reason, properties):
        outcomes.append(reason.is_failure)
        finished.set()

    client.on_connect = connected
    try:
        client.connect(*mqtt_broker.endpoint)
        client.loop_start()
        assert finished.wait(3) and outcomes == [True]
    finally:
        client.disconnect()
        client.loop_stop()


def test_broker_expires_offline_session_message(mqtt_broker, mqtt_probe):
    import paho.mqtt.client as mqtt
    from paho.mqtt.packettypes import PacketTypes
    from paho.mqtt.properties import Properties

    topic = "stonkworks/test/offline/" + uuid4().hex
    received, subscribed = [], threading.Event()
    client_id = "offline-" + uuid4().hex
    client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, client_id=client_id, protocol=mqtt.MQTTv5)
    client.username_pw_set("app", mqtt_broker.password)
    client.on_subscribe = lambda *args: subscribed.set()
    props = Properties(PacketTypes.CONNECT)
    props.SessionExpiryInterval = 60
    client.connect(*mqtt_broker.endpoint, clean_start=True, properties=props)
    client.loop_start()
    client.subscribe(topic, qos=1)
    assert subscribed.wait(3)
    client.disconnect()
    client.loop_stop()
    with mqtt_probe(topic) as publisher:
        publisher.publish(topic, {"command": "next"}, expiry=1)
    # Broker expiry uses whole seconds; allow the following clock tick.
    threading.Event().wait(2.1)
    resumed = threading.Event()
    client.on_connect = lambda *args: resumed.set()
    client.on_message = lambda c, u, m: received.append(m)
    props.SessionExpiryInterval = 0
    try:
        client.connect(*mqtt_broker.endpoint, clean_start=False, properties=props)
        client.loop_start()
        assert resumed.wait(3)
        threading.Event().wait(0.4)
        assert not received
    finally:
        client.disconnect()
        client.loop_stop()
