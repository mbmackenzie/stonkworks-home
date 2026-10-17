"""Environment configuration with file-based credentials."""

import math
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from pathlib import Path

from sonos_desk.mqtt.protocol import identifier


@dataclass(frozen=True)
class MqttConfig:
    host: str
    port: int
    username: str
    password: str = field(repr=False)
    instance: str
    poll_interval: float
    tls: bool
    ca_file: str | None


def load_mqtt_config(environ: Mapping[str, str], read_secret: Callable[[Path], str]) -> MqttConfig | None:
    host = environ.get("SONOS_MQTT_HOST", "").strip()
    if not host:
        return None
    try:
        tls_value = environ.get("SONOS_MQTT_TLS", "false").lower()
        if tls_value not in {"true", "false"}:
            raise ValueError("Invalid TLS boolean")
        tls = tls_value == "true"
        port = int(environ.get("SONOS_MQTT_PORT", "8883" if tls else "1883"))
        interval = float(environ.get("SONOS_MQTT_POLL_INTERVAL", "3"))
        username = environ.get("SONOS_MQTT_USERNAME", "").strip()
        secret_file = environ.get("SONOS_MQTT_PASSWORD_FILE", "")
        instance = identifier(environ.get("SONOS_MQTT_INSTANCE", "main"))
        ca_file = environ.get("SONOS_MQTT_CA_FILE") or None
        if not 1 <= port <= 65535 or not math.isfinite(interval) or interval <= 0:
            raise ValueError("Invalid port or poll interval")
        if not username or not secret_file or (ca_file and not tls):
            raise ValueError("Missing credentials or inconsistent TLS configuration")
        if ca_file and not Path(ca_file).is_file():
            raise ValueError("CA file unavailable")
        password = read_secret(Path(secret_file))
        if password.endswith("\r\n"):
            password = password[:-2]
        elif password.endswith("\n"):
            password = password[:-1]
        if not password or "\x00" in password:
            raise ValueError("Empty or invalid password")
        return MqttConfig(host, port, username, password, instance, interval, tls, ca_file)
    except (ValueError, OSError, UnicodeError):
        raise ValueError("Invalid MQTT configuration; check broker, credentials file, and TLS settings") from None
