import json
from datetime import UTC, datetime
from uuid import uuid4

import pytest
import requests
from sonos_desk.mqtt.commands import CommandProcessor
from sonos_desk.mqtt.protocol import IncomingCommand, Topics, encode_payload
from sonos_desk.sonos import SonosBackend
from test_sonos import Speaker


@pytest.fixture
def rig():
    now, sent, refreshed = [0.0], [], []
    speaker = Speaker("Office", "uid-o")
    backend = SonosBackend(discover=lambda **kw: [speaker], clock=lambda: now[0])
    session, topics = str(uuid4()), Topics()
    processor = CommandProcessor(
        backend,
        topics,
        session,
        lambda p: sent.append(p) or True,
        lambda: now[0],
        lambda: datetime(2026, 10, 16, tzinfo=UTC),
        refreshed.append,
    )
    processor.set_accepting(True)
    processor.set_room_ids(frozenset({"uid-o"}))

    def message(**kw):
        payload = {"request_id": str(uuid4()), "client_id": "test", "service_session": session, "command": "next", **kw}
        return IncomingCommand(topics.room_command("uid-o"), encode_payload(payload), False, 10, now[0])

    return processor, speaker, now, sent, message


def results(sent):
    return [json.loads(p.payload) for p in sent]


def test_duplicate_next_executes_once(rig):
    processor, speaker, now, sent, message = rig
    msg = message()
    processor.accept(msg)
    processor.accept(msg)
    assert processor.run_one()
    assert not processor.run_one()
    now[0] = 11
    processor.accept(msg)
    assert speaker.skips == 1
    assert results(sent)[0] == results(sent)[1]
    assert sent[0].expiry == 30 and not sent[0].retained


def test_conflicting_request_id_is_rejected(rig):
    processor, speaker, _, sent, message = rig
    msg = message()
    processor.accept(msg)
    changed = json.loads(msg.payload) | {"command": "previous"}
    processor.accept(IncomingCommand(msg.topic, encode_payload(changed), False, 10, 0))
    assert results(sent)[-1]["status"] == "rejected"
    processor.run_one()
    assert speaker.skips == 1


@pytest.mark.parametrize(
    "change", [{"service_session": str(uuid4())}, {"value": None}, {"command": "volume", "value": True}]
)
def test_invalid_envelope_never_mutates(rig, change):
    processor, speaker, _, sent, message = rig
    processor.accept(message(**change))
    assert not processor.run_one()
    assert speaker.skips == 0
    assert results(sent)[-1]["status"] == "rejected"


@pytest.mark.parametrize("expiry,retained", [(None, False), (0, False), (11, False), (10, True)])
def test_transport_safety(rig, expiry, retained):
    processor, speaker, _, sent, message = rig
    m = message()
    processor.accept(IncomingCommand(m.topic, m.payload, retained, expiry, 0))
    assert not processor.run_one()
    assert speaker.skips == 0
    assert results(sent)[-1]["status"] == "rejected"


def test_names_and_unknown_ids_do_not_fall_back_to_http_lookup(rig):
    processor, speaker, _, _, message = rig
    m = message()
    for room in ("Office", "unknown"):
        processor.accept(IncomingCommand(Topics().room_command(room), m.payload, False, 10, 0))
    assert not processor.run_one()
    assert speaker.skips == 0


def test_invalid_unroutable_inputs_are_dropped(rig):
    processor, speaker, _, sent, message = rig
    for payload in (b"x" * 4097, b"\xff", b"{", b"[]", b'{"client_id":"../escape"}', b'{"value":NaN}'):
        processor.accept(IncomingCommand(message().topic, payload, False, 10, 0))
    assert not sent and speaker.skips == 0


def test_queued_expiry_and_queue_capacity(rig):
    processor, speaker, now, sent, message = rig
    for _ in range(17):
        processor.accept(message())
    assert len(sent) == 1 and results(sent)[0]["status"] == "rejected"
    now[0] = 10
    while processor.run_one():
        pass
    assert speaker.skips == 0
    assert len(sent) == 17


def test_protected_cache_not_evicted_and_releases_at_60_seconds(rig):
    processor, speaker, now, _sent, message = rig
    first = message()
    processor.accept(first)
    processor.run_one()
    for _ in range(1023):
        processor.accept(message())
        processor.run_one()
    processor.accept(message())
    assert not processor.run_one()
    assert speaker.skips == 1024
    processor.accept(first)
    assert speaker.skips == 1024
    now[0] = 60
    processor.accept(message())
    assert processor.run_one()
    assert speaker.skips == 1025


def test_uncertain_write_is_not_retried(rig, monkeypatch):
    processor, speaker, _, sent, message = rig

    def uncertain():
        speaker.skips += 1
        raise requests.Timeout()

    monkeypatch.setattr(speaker, "next", uncertain)
    m = message()
    processor.accept(m)
    processor.run_one()
    processor.accept(m)
    assert speaker.skips == 1
    assert [r["status"] for r in results(sent)] == ["uncertain", "uncertain"]


def test_publication_failure_and_reconnect_never_repeat_action(rig):
    processor, speaker, _, sent, message = rig
    processor.publish = lambda publication: False
    m = message()
    processor.accept(m)
    processor.run_one()
    processor.set_accepting(False)
    processor.set_accepting(True)
    processor.publish = lambda p: sent.append(p) or True
    processor.accept(m)
    assert speaker.skips == 1
    assert results(sent)[-1]["status"] == "completed"


def test_accept_never_contacts_speakers_and_pending_record_stays_protected(rig, monkeypatch):
    processor, speaker, now, sent, message = rig

    def network_forbidden(*args, **kwargs):
        raise AssertionError("callback performed I/O")

    monkeypatch.setattr(processor.backend, "command", network_forbidden)
    monkeypatch.setattr(processor.backend, "state", network_forbidden)
    monkeypatch.setattr(processor.backend, "rooms", network_forbidden)
    m = message()
    processor.accept(m)
    now[0] = 65
    processor.accept(m)
    assert processor.run_one()
    assert not processor.run_one()
    assert speaker.skips == 0
    assert results(sent)[-1]["status"] == "rejected"


@pytest.mark.parametrize("status", [404, 409])
def test_known_backend_rejection(rig, monkeypatch, status):
    from sonos_desk.sonos import BackendError

    processor, _, _, sent, message = rig

    def unavailable(*args, **kw):
        raise BackendError(status, "Unavailable")

    monkeypatch.setattr(processor.backend, "command", unavailable)
    processor.accept(message())
    processor.run_one()
    assert results(sent)[-1]["status"] == "rejected"
