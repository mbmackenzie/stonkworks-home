import json
from datetime import UTC, datetime
from uuid import uuid4

from fastapi.testclient import TestClient
from sonos_desk.app import create_app
from sonos_desk.mqtt.config import MqttConfig
from sonos_desk.mqtt.protocol import IncomingCommand, encode_payload
from sonos_desk.mqtt.runtime import MqttAdapter
from sonos_desk.sonos import SonosBackend
from test_sonos import Speaker


class Transport:
    def __init__(self, config, topics, session, on_message, on_ready, on_disconnect, **kwargs):
        self.ready, self.disconnected = on_ready, on_disconnect
        self.connected, self.closed = False, False
        self.sent = []

    def start(self):
        pass

    def publish(self, p):
        self.sent.append(p)
        return self.connected

    def snapshot(self):
        return {"connected": self.connected, "last_connected_at": None, "last_error": None}

    def close(self):
        self.closed = True
        self.connected = False


def config():
    return MqttConfig("mqtt.example.test", 1883, "app", "pw", "main", 3, False, None)


def test_disabled_status_is_cached_and_http_health_unchanged(monkeypatch):
    backend = SonosBackend(discover=lambda **kw: [])
    with TestClient(create_app(backend)) as client:

        def forbidden():
            raise AssertionError("status contacted speakers")

        monkeypatch.setattr(backend, "rooms", forbidden)
        response = client.get("/mqtt/status")
        assert response.headers["cache-control"] == "no-store"
        assert response.json() == {
            "enabled": False,
            "connected": False,
            "service_session": None,
            "last_connected_at": None,
            "last_poll_completed_at": None,
            "rooms_known": 0,
            "rooms_unavailable": 0,
            "last_error": None,
        }
        assert client.get("/health").json() == {"status": "ok", "rooms": 0, "discovery": "ok"}


def test_command_priority_and_reconnect_session():
    speaker = Speaker("Office", "uid-o")
    backend = SonosBackend(discover=lambda **kw: [speaker], clock=lambda: 0)
    adapter = MqttAdapter(
        backend,
        config(),
        transport_factory=Transport,
        clock=lambda: 0,
        utcnow=lambda: datetime(2026, 10, 16, tzinfo=UTC),
    )
    assert isinstance(adapter.transport, Transport)
    adapter.transport.connected = True
    adapter.transport.ready()
    assert adapter._step()  # inventory first
    msg = IncomingCommand(
        adapter.topics.room_command("uid-o"),
        encode_payload(
            {"client_id": "test", "request_id": str(uuid4()), "service_session": adapter.session, "command": "next"}
        ),
        False,
        10,
        0,
    )
    adapter.commands.accept(msg)
    assert adapter._step()
    assert speaker.skips == 1
    assert not any(p.topic.endswith("/state") for p in adapter.transport.sent)
    old_session = adapter.session
    adapter.transport.disconnected()
    adapter.transport.ready()
    assert adapter.session == old_session
    adapter.commands.accept(msg)
    assert speaker.skips == 1
    assert json.loads(adapter.transport.sent[-1].payload)["status"] == "completed"


def test_enabled_lifecycle_does_not_wait_for_broker(monkeypatch):
    import sonos_desk.app as module
    from sonos_desk.render import Renderer

    backend = SonosBackend(discover=lambda **kw: [])
    adapter = MqttAdapter(backend, config(), transport_factory=Transport)
    monkeypatch.setattr(module, "MqttAdapter", lambda *args: adapter)
    renderer = Renderer()
    with TestClient(create_app(backend, renderer, mqtt_config=config())) as client:
        assert client.get("/mqtt/status").json()["enabled"]
        assert not client.get("/mqtt/status").json()["connected"]
        assert client.get("/rooms").json() == []
        assert client.get("/").status_code == 200
        assert not renderer.client.is_closed
    assert isinstance(adapter.transport, Transport)
    assert adapter.transport.closed and renderer.client.is_closed
    assert adapter._thread is not None
    assert not adapter._thread.is_alive()


def test_shutdown_rejects_pending_without_mutation():
    speaker = Speaker("Office", "uid-o")
    backend = SonosBackend(discover=lambda **kw: [speaker])
    adapter = MqttAdapter(backend, config(), transport_factory=Transport)
    assert isinstance(adapter.transport, Transport)
    adapter.transport.connected = True
    adapter.transport.ready()
    adapter._step()
    import time

    adapter.commands.accept(
        IncomingCommand(
            adapter.topics.room_command("uid-o"),
            encode_payload(
                {"request_id": str(uuid4()), "client_id": "test", "service_session": adapter.session, "command": "next"}
            ),
            False,
            10,
            time.monotonic(),
        )
    )
    adapter.close()
    assert speaker.skips == 0
    results = [json.loads(p.payload) for p in adapter.transport.sent if p.topic.endswith("/results")]
    assert results[-1]["status"] == "rejected"


def test_startup_failure_closes_adapter_before_renderer(monkeypatch):
    import pytest
    import sonos_desk.app as module
    from sonos_desk.render import Renderer

    events = []
    backend = SonosBackend(discover=lambda **kw: [])
    adapter = MqttAdapter(backend, config(), transport_factory=Transport)

    def broken_start():
        raise RuntimeError("startup failed")

    monkeypatch.setattr(adapter, "start", broken_start)
    original_close = adapter.close

    def close_adapter():
        events.append("adapter")
        original_close()

    monkeypatch.setattr(adapter, "close", close_adapter)
    monkeypatch.setattr(module, "MqttAdapter", lambda *args: adapter)
    renderer = Renderer()
    original_renderer_close = renderer.close

    def close_renderer():
        events.append("renderer")
        original_renderer_close()

    monkeypatch.setattr(renderer, "close", close_renderer)
    with (
        pytest.raises(RuntimeError, match="startup failed"),
        TestClient(create_app(backend, renderer, mqtt_config=config())),
    ):
        pass
    assert events == ["adapter", "renderer"]
