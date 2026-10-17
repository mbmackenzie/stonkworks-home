import json
from uuid import uuid4

import pytest
from pydantic import ValidationError
from sonos_desk.mqtt.protocol import CommandRequest, Topics, encode_payload
from sonos_desk.routes import Command


def request(**kw):
    return {"request_id": str(uuid4()), "service_session": str(uuid4()), "client_id": "test", "command": "pause", **kw}


@pytest.mark.parametrize(
    "change",
    [
        {"value": None},
        {"extra": 1},
        {"command": "volume", "value": True},
        {"command": "volume", "value": "4"},
        {"request_id": 42},
        {"client_id": "../x"},
    ],
)
def test_mqtt_command_envelope_is_strict(change):
    with pytest.raises(ValidationError):
        CommandRequest.model_validate(request(**change))


def test_valid_command_and_http_validation_survives_extraction():
    assert CommandRequest.model_validate(request(command="volume", value=0)).value == 0
    assert Command(room="office", command="mute", value=False).value is False
    with pytest.raises(ValidationError):
        Command(room="office", command="pause", value=None)


@pytest.mark.parametrize("bad", ["", "a/b", "+", "#", "a\x00b"])
def test_topic_ids_are_single_safe_segments(bad):
    topics = Topics("main")
    with pytest.raises(ValueError):
        topics.room_command(bad)
    with pytest.raises(ValueError):
        Topics(bad)
    with pytest.raises(ValueError):
        topics.results(bad)


def test_exact_topic_structure():
    topics = Topics("main")
    assert topics.availability == "stonkworks/v1/sonos-desk/main/availability"
    assert topics.inventory == "stonkworks/v1/sonos-desk/main/rooms"
    assert topics.command_room("stonkworks/v1/sonos-desk/main/rooms/uid/command") == "uid"
    with pytest.raises(ValueError):
        topics.command_room("stonkworks/v1/sonos-desk/other/rooms/uid/command")
    assert json.loads(encode_payload({"mute": False})) == {"mute": False}
    with pytest.raises(ValueError):
        encode_payload({"volume": float("nan")})


def test_command_schema_matches_runtime_validation():
    from pathlib import Path

    import jsonschema

    schema = json.loads((Path(__file__).resolve().parents[3] / "protocols/v1/sonos-desk.schema.json").read_text())
    validator = jsonschema.Draft202012Validator(
        {**schema, "$ref": "#/$defs/command"}, format_checker=jsonschema.FormatChecker()
    )
    validator.validate(request(command="mute", value=False))
    for change in ({"value": None}, {"command": "volume"}, {"command": "volume", "value": True}, {"extra": 1}):
        assert list(validator.iter_errors(request(**change)))
