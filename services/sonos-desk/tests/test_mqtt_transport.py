import json
from types import SimpleNamespace
from uuid import uuid4

from sonos_desk.mqtt.config import MqttConfig
from sonos_desk.mqtt.protocol import Publication, Topics
from sonos_desk.mqtt.transport import MqttTransport


class Client:
    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.sent, self.subscriptions = [], []
        self.sequence = 0

    def username_pw_set(self, *args):
        pass

    def will_set(self, topic, payload, **kwargs):
        self.will = topic, json.loads(payload), kwargs

    def max_queued_messages_set(self, count):
        self.max_queued = count

    def max_inflight_messages_set(self, count):
        self.max_inflight = count

    def subscribe(self, topic, options):
        self.subscriptions.append((topic, options))
        return 0, 77

    def publish(self, *args, **kw):
        self.sequence += 1
        self.sent.append((args, kw))
        return SimpleNamespace(rc=0, mid=self.sequence)


def rig():
    received, ready, offline = [], [], []
    config = MqttConfig("mqtt.example.test", 1883, "app", "pw", "main", 3, False, None)
    transport = MqttTransport(
        config,
        Topics(),
        str(uuid4()),
        received.append,
        lambda: ready.append(True),
        lambda: offline.append(True),
        client_factory=Client,
    )
    client = transport._new_client()
    return transport, client, received, ready, offline


def connect(client):
    client.on_connect(client, None, None, SimpleNamespace(is_failure=False), None)
    client.on_subscribe(client, None, 77, [SimpleNamespace(is_failure=False)], None)


def test_connection_options_and_wait_for_suback():
    from paho.mqtt.client import CallbackAPIVersion, MQTTv5

    transport, client, _, ready, _ = rig()
    assert client.kwargs["protocol"] == MQTTv5
    assert client.kwargs["callback_api_version"] == CallbackAPIVersion.VERSION2
    assert not client.kwargs["reconnect_on_failure"]
    assert client.will[1]["status"] == "offline" and client.will[2]["retain"]
    client.on_connect(client, None, None, SimpleNamespace(is_failure=False), None)
    assert not ready
    options = client.subscriptions[0][1]
    assert options.QoS == 1 and options.retainAsPublished and options.retainHandling == 2
    client.on_subscribe(client, None, 77, [SimpleNamespace(is_failure=False)], None)
    assert ready and transport.snapshot()["connected"]


def test_incoming_expiry_and_retain_preserved_and_old_callbacks_ignored():
    transport, client, received, ready, offline = rig()
    connect(client)
    message = SimpleNamespace(
        topic=Topics().room_command("uid-o"),
        payload=b"{}",
        retain=True,
        properties=SimpleNamespace(MessageExpiryInterval=7),
    )
    client.on_message(client, None, message)
    assert received[-1].retained and received[-1].remaining_expiry == 7
    replacement = transport._new_client()
    connect(replacement)
    before = len(ready)
    client.on_disconnect(client, None, None, None, None)
    client.on_subscribe(client, None, 77, [SimpleNamespace(is_failure=False)], None)
    client.on_message(client, None, message)
    assert len(ready) == before and not offline and len(received) == 1
    assert transport.snapshot()["connected"]


def test_queue_capacity_coalescing_inflight_and_disconnect_reset():
    transport, client, _, _, _ = rig()
    connect(client)
    for n in range(128):
        assert transport.publish(Publication(f"topic/{n}", b"old", True, 90))
    assert not transport.publish(Publication("result", b"{}", False, 30))
    assert transport.publish(Publication("topic/0", b"new", True, 90))
    transport._flush(client)
    assert len(client.sent) == 20
    assert client.sent[0][0][1] == b"new"
    client.on_disconnect(client, None, None, None, None)
    new = transport._new_client()
    connect(new)
    transport._flush(new)
    assert not new.sent


def test_subscription_and_auth_failure_not_online():
    transport, client, _, ready, _ = rig()
    client.on_connect(client, None, None, SimpleNamespace(is_failure=True), None)
    assert not ready and not transport.snapshot()["connected"]
    client.on_connect(client, None, None, SimpleNamespace(is_failure=False), None)
    client.on_subscribe(client, None, 77, [SimpleNamespace(is_failure=True)], None)
    assert not ready and transport.snapshot()["last_error"]


def test_backoff_is_bounded_and_jittered():
    transport, _, _, _, _ = rig()
    values = [transport._retry_delay(n) for n in range(10)]
    assert all(1 <= v <= 30 for v in values)
    assert values[0] == 1
    assert len({transport._retry_delay(5) for _ in range(10)}) > 1


def test_tls_verifies_hostname_and_trust(monkeypatch):
    import ssl

    contexts = []
    monkeypatch.setattr(Client, "tls_set_context", lambda self, context: contexts.append(context), raising=False)
    config = MqttConfig("mqtt.example.test", 8883, "app", "pw", "main", 3, True, None)
    transport = MqttTransport(
        config, Topics(), str(uuid4()), lambda m: None, lambda: None, lambda: None, client_factory=Client
    )
    transport._new_client()
    assert contexts[0].verify_mode == ssl.CERT_REQUIRED
    assert contexts[0].check_hostname


def test_expired_outgoing_results_are_dropped():
    transport, client, _, _, _ = rig()
    now = [0]
    transport.clock = lambda: now[0]
    connect(client)
    transport.publish(Publication("result", b"{}", False, 30))
    now[0] = 31
    transport._flush(client)
    assert not client.sent
