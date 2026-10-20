"""Send one explicitly requested MQTT command, without action retries."""

import argparse
import json
import os
import sys
import threading
import time
from dataclasses import dataclass, replace
from uuid import uuid4

import paho.mqtt.client as mqtt
from paho.mqtt.packettypes import PacketTypes
from paho.mqtt.properties import Properties
from pydantic import ValidationError
from sonos_desk.commands import CommandFields
from sonos_desk.mqtt.config import load_mqtt_config
from sonos_desk.mqtt.protocol import CommandRequest, Topics, decode_payload, encode_payload, identifier, room_segment


@dataclass
class Observation:
    session: str | None = None
    subscribed: bool = False
    failed: bool = False
    result: dict | None = None


def read_secret(path):
    return path.read_text(encoding="utf-8")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--room-id", required=True)
    parser.add_argument("--command", required=True)
    parser.add_argument("--value", default=argparse.SUPPRESS, type=json.loads)
    parser.add_argument("--instance")
    parser.add_argument("--client-id", default="cli-" + uuid4().hex[:12])
    args = parser.parse_args(argv)
    try:
        room_segment(args.room_id)
        identifier(args.client_id)
        if args.instance is not None:
            identifier(args.instance)
        fields = {"command": args.command}
        if hasattr(args, "value"):
            fields["value"] = args.value
        CommandFields.model_validate(fields)
    except (ValueError, ValidationError):
        parser.error("Invalid room/client/instance ID or command/value")
    return args


def run_command(config, room_id, fields, client_id, *, timeout=15, client_factory=mqtt.Client):
    """Return one matching result or None; reconnect and action retry are disabled."""
    topics, request_id = Topics(config.instance), str(uuid4())
    condition = threading.Condition()
    observed = Observation()
    client = client_factory(
        callback_api_version=mqtt.CallbackAPIVersion.VERSION2,
        client_id=client_id,
        protocol=mqtt.MQTTv5,
        reconnect_on_failure=False,
    )
    client.username_pw_set(config.username, config.password)
    if config.tls:
        import ssl

        client.tls_set_context(ssl.create_default_context(cafile=config.ca_file))

    def on_connect(c, userdata, flags, reason, properties):
        if reason.is_failure:
            with condition:
                observed.failed = True
                condition.notify_all()
        else:
            c.subscribe([(topics.availability, 1), (topics.results(client_id), 1)])

    def on_subscribe(c, userdata, mid, reasons, properties):
        with condition:
            observed.subscribed = bool(reasons) and not any(r.is_failure for r in reasons)
            observed.failed = not observed.subscribed
            condition.notify_all()

    def on_message(c, userdata, message):
        try:
            payload = decode_payload(bytes(message.payload))
            if not isinstance(payload, dict):
                return
            with condition:
                if message.topic == topics.availability:
                    observed.session = payload.get("service_session") if payload.get("status") == "online" else None
                elif payload.get("request_id") == request_id and payload.get("client_id") == client_id:
                    observed.result = payload
                condition.notify_all()
        except (ValueError, UnicodeError):
            return

    def on_disconnect(*args):
        with condition:
            observed.failed = True
            condition.notify_all()

    client.on_connect, client.on_subscribe = on_connect, on_subscribe
    client.on_message, client.on_disconnect = on_message, on_disconnect
    deadline = time.monotonic() + timeout
    try:
        properties = Properties(PacketTypes.CONNECT)
        properties.SessionExpiryInterval = 0
        client.connect(config.host, config.port, keepalive=15, clean_start=True, properties=properties)
        client.loop_start()
        with condition:
            condition.wait_for(
                lambda: observed.failed or (observed.session and observed.subscribed),
                timeout=max(0, deadline - time.monotonic()),
            )
            if observed.failed or not observed.session or not observed.subscribed:
                return None
            request = CommandRequest.model_validate(
                {"request_id": request_id, "client_id": client_id, "service_session": observed.session, **fields}
            )
        expiry = Properties(PacketTypes.PUBLISH)
        expiry.MessageExpiryInterval = 10
        info = client.publish(
            topics.room_command(room_id),
            encode_payload(request.model_dump(mode="json", exclude_unset=True)),
            qos=1,
            retain=False,
            properties=expiry,
        )
        if info.rc != mqtt.MQTT_ERR_SUCCESS:
            return None
        with condition:
            condition.wait_for(
                lambda: observed.result is not None or observed.failed,
                timeout=max(0, deadline - time.monotonic()),
            )
            result = observed.result
            return result if isinstance(result, dict) else None
    except (OSError, ValueError):
        return None
    finally:
        client.disconnect()
        client.loop_stop()


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        config = load_mqtt_config(os.environ, read_secret)
        if config is None:
            raise ValueError("MQTT host required")
        if args.instance:
            config = replace(config, instance=args.instance)
    except ValueError:
        print("Configure broker host, account, password file, and TLS settings.", file=sys.stderr)
        return 5
    fields = {"command": args.command}
    if hasattr(args, "value"):
        fields["value"] = args.value
    result = run_command(config, args.room_id, fields, args.client_id)
    if result is None:
        print("No result received. Outcome may be uncertain; inspect state before another action.", file=sys.stderr)
        return 4
    print(json.dumps(result))
    return {"completed": 0, "rejected": 2, "uncertain": 3}.get(result.get("status"), 4)


if __name__ == "__main__":
    raise SystemExit(main())
