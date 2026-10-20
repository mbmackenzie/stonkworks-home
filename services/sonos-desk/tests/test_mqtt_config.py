from pathlib import Path

import pytest
from sonos_desk.mqtt.config import load_mqtt_config


def env(**kw):
    return {
        "SONOS_MQTT_HOST": "mqtt.example.test",
        "SONOS_MQTT_USERNAME": "app",
        "SONOS_MQTT_PASSWORD_FILE": "/secret",
        **kw,
    }


def test_disabled_and_secret_file_configuration():
    assert load_mqtt_config({}, lambda p: "unused") is None
    config = load_mqtt_config(env(), lambda p: "secret\n\n")
    assert config is not None
    assert config.port == 1883 and config.password == "secret\n"
    assert "secret" not in repr(config)
    assert config.poll_interval == 3 and config.instance == "main"
    tls = load_mqtt_config(env(SONOS_MQTT_TLS="true"), lambda p: "pw")
    assert tls is not None
    assert tls.tls and tls.port == 8883 and tls.ca_file is None


@pytest.mark.parametrize(
    "change",
    [
        {"SONOS_MQTT_PORT": "0"},
        {"SONOS_MQTT_PORT": "65536"},
        {"SONOS_MQTT_USERNAME": ""},
        {"SONOS_MQTT_PASSWORD_FILE": ""},
        {"SONOS_MQTT_POLL_INTERVAL": "nan"},
        {"SONOS_MQTT_POLL_INTERVAL": "inf"},
        {"SONOS_MQTT_POLL_INTERVAL": "0"},
        {"SONOS_MQTT_INSTANCE": "a/b"},
        {"SONOS_MQTT_TLS": "maybe"},
        {"SONOS_MQTT_CA_FILE": "/ca"},
    ],
)
def test_invalid_configuration(change):
    with pytest.raises(ValueError):
        load_mqtt_config(env(**change), lambda p: "pw")


def test_secret_failure_is_sanitized():
    def fail(path: Path):
        raise OSError("SENTINEL_SECRET")

    with pytest.raises(ValueError) as exc:
        load_mqtt_config(env(), fail)
    assert "SENTINEL_SECRET" not in str(exc.value)
    with pytest.raises(ValueError):
        load_mqtt_config(env(), lambda p: "\n")
