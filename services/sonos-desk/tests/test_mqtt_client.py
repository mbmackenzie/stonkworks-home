import importlib.util
from pathlib import Path

import pytest


def client_module():
    path = Path(__file__).parents[1] / "examples/mqtt_client.py"
    spec = importlib.util.spec_from_file_location("mqtt_example", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    import sys

    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize(
    "command,value,expected", [("volume", "0", 0), ("volume_relative", "-1", -1), ("mute", "false", False)]
)
def test_cli_parses_json_value_types(command, value, expected):
    args = client_module().parse_args(["--room-id", "uid-o", "--command", command, "--value", value])
    assert args.value == expected and type(args.value) is type(expected)


@pytest.mark.parametrize(
    "args",
    [
        [],
        ["--room-id", "uid-o", "--command", "pause", "--value", "null"],
        ["--room-id", "uid-o", "--command", "volume", "--value", "true"],
    ],
)
def test_cli_requires_explicit_valid_action(args):
    with pytest.raises(SystemExit):
        client_module().parse_args(args)


def test_client_timeout_does_not_republish(monkeypatch):
    module = client_module()
    sent = []
    monkeypatch.setattr(module, "run_command", lambda *args, **kwargs: sent.append(args) or None)
    monkeypatch.setenv("SONOS_MQTT_HOST", "mqtt.example.test")
    monkeypatch.setenv("SONOS_MQTT_USERNAME", "app")
    monkeypatch.setenv("SONOS_MQTT_PASSWORD_FILE", "/secret")
    monkeypatch.setattr(module, "read_secret", lambda path: "pw")
    assert module.main(["--room-id", "uid-o", "--command", "pause"]) == 4
    assert len(sent) == 1


@pytest.mark.parametrize("disconnect", [True, False])
def test_run_command_publishes_once_after_disconnect_or_timeout(disconnect):
    from types import SimpleNamespace
    from uuid import uuid4

    from sonos_desk.mqtt.config import MqttConfig

    module, sent = client_module(), []

    class Client:
        def __init__(self, **kwargs):
            assert not kwargs["reconnect_on_failure"]
            self.on_connect = lambda *args: None
            self.on_subscribe = lambda *args: None
            self.on_message = lambda *args: None
            self.on_disconnect = lambda *args: None

        def username_pw_set(self, *args):
            pass

        def connect(self, *args, **kwargs):
            pass

        def subscribe(self, topics):
            self.on_subscribe(self, None, 1, [SimpleNamespace(is_failure=False)], None)

        def loop_start(self):
            self.on_connect(self, None, None, SimpleNamespace(is_failure=False), None)
            self.on_message(
                self,
                None,
                SimpleNamespace(
                    topic="stonkworks/v1/sonos-desk/main/availability",
                    payload=module.encode_payload({"status": "online", "service_session": str(uuid4())}),
                ),
            )

        def publish(self, *args, **kwargs):
            sent.append((args, kwargs))
            if disconnect:
                self.on_disconnect()
            return SimpleNamespace(rc=0)

        def disconnect(self):
            pass

        def loop_stop(self):
            pass

    config = MqttConfig("mqtt.example.test", 1883, "app", "pw", "main", 3, False, None)
    assert module.run_command(config, "uid-o", {"command": "next"}, "cli", timeout=0.02, client_factory=Client) is None
    assert len(sent) == 1 and not sent[0][1]["retain"]
    assert sent[0][1]["properties"].MessageExpiryInterval == 10
