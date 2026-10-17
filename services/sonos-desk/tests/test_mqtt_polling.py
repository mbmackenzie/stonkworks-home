import json
from datetime import UTC, datetime, timedelta
from uuid import uuid4

import pytest
import requests
from sonos_desk.mqtt.polling import StatePoller
from sonos_desk.mqtt.protocol import Topics
from sonos_desk.sonos import SonosBackend
from test_sonos import Speaker


@pytest.fixture
def rig():
    now, sent, inventory = [0.0], [], []
    speakers = [Speaker("Office", "uid-o"), Speaker("Kitchen", "uid-k")]
    backend = SonosBackend(discover=lambda **kw: speakers.copy(), clock=lambda: now[0])
    poller = StatePoller(
        backend,
        Topics(),
        str(uuid4()),
        lambda p: sent.append(p) or True,
        lambda: now[0],
        lambda: datetime(2026, 10, 16, tzinfo=UTC) + timedelta(seconds=now[0]),
        3,
        inventory.append,
    )
    return poller, speakers, now, sent, inventory


def observations(sent, suffix):
    return [json.loads(p.payload) for p in sent if p.topic.endswith(suffix) and p.payload]


def drain(poller):
    for _ in range(20):
        if not poller.poll_one():
            return
    raise AssertionError("polling backlog did not drain")


def test_all_rooms_publish_by_id(rig):
    poller, _, _, sent, inventory = rig
    drain(poller)
    states = observations(sent, "/state")
    assert {r["state"]["room_id"] for r in states} == {"uid-o", "uid-k"}
    assert inventory[-1] == frozenset({"uid-o", "uid-k"})
    assert all(p.retained and p.expiry == 90 for p in sent)
    assert poller.snapshot()["rooms_known"] == 2


def test_unchanged_state_refreshes_after_15_seconds_and_inventory_after_30(rig):
    poller, speakers, now, sent, _ = rig
    drain(poller)
    for n in (3, 6, 9, 12):
        now[0] = n
        drain(poller)
    assert len(observations(sent, "/state")) == 2
    now[0] = 15
    drain(poller)
    assert len(observations(sent, "/state")) == 4
    assert len(observations(sent, "/rooms")) == 1
    now[0] = 30
    drain(poller)
    assert len(observations(sent, "/rooms")) == 2
    speakers[0].title = "New song"
    now[0] = 33
    drain(poller)
    assert observations(sent, "/state")[-1]["state"]["title"] == "New song"


def test_one_broken_room_does_not_block_healthy_room(rig, monkeypatch):
    poller, speakers, now, sent, _ = rig
    drain(poller)

    def unavailable():
        raise requests.Timeout()

    monkeypatch.setattr(speakers[0], "get_current_track_info", unavailable)
    speakers[1].title = "Changed"
    now[0] = 3
    drain(poller)
    assert observations(sent, "/state")[-1]["state"]["room_id"] == "uid-k"
    assert observations(sent, "/state")[-1]["state"]["title"] == "Changed"
    assert observations(sent, "/uid-o/availability")[-1]["status"] == "offline"
    assert poller.snapshot()["rooms_unavailable"] == 1
    assert len([s for s in observations(sent, "/state") if s["state"]["room_id"] == "uid-o"]) == 1


def test_inventory_failure_retains_ids_and_independent_reads(rig, monkeypatch):
    from sonos_desk.sonos import BackendError

    poller, speakers, now, sent, inventory = rig
    drain(poller)

    def broken_inventory():
        raise BackendError(503, "Group unavailable")

    monkeypatch.setattr(poller.backend, "rooms", broken_inventory)
    speakers[1].title = "Still available"
    now[0] = 30
    drain(poller)
    assert inventory[-1] == frozenset({"uid-o", "uid-k"})
    assert observations(sent, "/rooms")[-1]["discovery"] == "degraded"
    assert observations(sent, "/state")[-1]["state"]["title"] == "Still available"


def test_removed_rooms_tombstone_and_empty_inventory(rig):
    poller, speakers, now, sent, _ = rig
    drain(poller)
    speakers.clear()
    now[0] = 60
    drain(poller)
    assert observations(sent, "/rooms")[-1]["rooms"] == []
    assert len([p for p in sent if not p.payload and p.retained]) == 4
    assert poller.snapshot()["rooms_known"] == 0


def test_reconnect_does_not_relabel_old_state_and_retries_failed_publish(rig):
    poller, speakers, now, sent, _ = rig
    drain(poller)
    now[0] = 1
    speakers[0].title = "Fresh despite HTTP TTL"
    poller.force_refresh()
    drain(poller)
    assert observations(sent, "/state")[-2]["state"]["title"] == "Fresh despite HTTP TTL"
    sent.clear()
    poller.publish = lambda p: False
    now[0] = 4
    speakers[0].title = "Publish failure"
    drain(poller)
    poller.publish = lambda p: sent.append(p) or True
    now[0] = 7
    drain(poller)
    assert observations(sent, "/state")[-1]["state"]["title"] == "Publish failure"


def test_no_missed_poll_backlog_and_forced_room_first(rig):
    poller, _, now, sent, _ = rig
    drain(poller)
    now[0] = 1000
    drain(poller)
    assert not poller.poll_one()
    sent.clear()
    poller.force_refresh("uid-k")
    assert poller.poll_one()
    assert observations(sent, "/state")[0]["state"]["room_id"] == "uid-k"


def test_room_observation_cannot_switch_to_name_after_discovery_changes(rig, monkeypatch):
    poller, _speakers, now, sent, _ = rig
    drain(poller)
    original_state = poller.backend.state
    replacement = Speaker("uid-o", "uid-b")

    def changed_inventory(room, *args, **kwargs):
        if room == "uid-o":
            poller.backend._inventory_at = float("-inf")
            poller.backend._discover = lambda **kw: [replacement]
        return original_state(room, *args, **kwargs)

    monkeypatch.setattr(poller.backend, "state", changed_inventory)
    now[0] = 3
    drain(poller)
    states = observations(sent, "/uid-o/state")
    assert all(s["state"]["room_id"] == "uid-o" for s in states)
    assert observations(sent, "/uid-o/availability")[-1]["status"] == "offline"
