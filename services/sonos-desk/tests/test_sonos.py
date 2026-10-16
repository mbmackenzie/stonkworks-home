from types import SimpleNamespace

import pytest
import requests
from soco.exceptions import NotSupportedException
from sonos_desk.sonos import BackendError, SonosBackend


class Speaker:
    def __init__(self, name, uid):
        self.player_name, self.uid = name, uid
        self.ip_address = "192.0.2.11"
        self.volume, self.mute = 40, False
        self.transport, self.title = "PLAYING", "A song"
        self.group = SimpleNamespace(coordinator=self)
        self.refreshes, self.skips = 0, 0
        self.zone_group_state = SimpleNamespace(clear_cache=self.refresh)

    def refresh(self):
        self.refreshes += 1

    def get_current_track_info(self):
        return {
            "title": self.title,
            "artist": "Artist",
            "album": "Album",
            "album_art": "/cover.png",
            "duration": "0:03:00",
            "position": "0:01:00",
        }

    def get_current_transport_info(self):
        return {"current_transport_state": self.transport}

    def play(self):
        self.transport = "PLAYING"

    def pause(self):
        self.transport = "PAUSED_PLAYBACK"

    def next(self):
        self.skips += 1
        self.title = "Next song"

    def previous(self):
        self.title = "Previous song"


@pytest.fixture
def speakers():
    coordinator, office = Speaker("Kitchen", "uid-k"), Speaker("Office", "uid-o")
    office.group.coordinator = coordinator
    return coordinator, office


def test_case_insensitive_lookup_and_coordinator_metadata(speakers):
    backend = SonosBackend(discover=lambda **kw: set(speakers))
    state = backend.state("OFFICE")
    assert state["room"] == "Office"
    assert state["coordinator"] == "Kitchen"
    assert state["volume"] == 40
    assert state["artwork_url"] == "http://192.0.2.11:1400/cover.png"
    with pytest.raises(BackendError) as err:
        backend.state("missing")
    assert err.value.status == 404


def test_duplicate_names_still_allow_uid(speakers):
    speakers[0].player_name = "office"
    backend = SonosBackend(discover=lambda **kw: speakers)
    with pytest.raises(BackendError) as err:
        backend.state("office")
    assert err.value.status == 409
    assert backend.state("uid-o")["room_id"] == "uid-o"


def test_group_playback_and_room_volume_and_mute(speakers):
    kitchen, office = speakers
    backend = SonosBackend(discover=lambda **kw: speakers)
    assert backend.command("office", "pause")["playback_state"] == "PAUSED_PLAYBACK"
    assert kitchen.transport == "PAUSED_PLAYBACK"
    assert office.transport == "PLAYING"
    assert backend.command("office", "volume_relative", 100)["volume"] == 100
    assert kitchen.volume == 40
    assert backend.command("office", "volume_relative", -100)["volume"] == 0
    assert backend.command("office", "mute", True)["mute"] is True
    assert kitchen.mute is False
    assert office.refreshes >= 4


def test_command_clears_other_group_state(speakers):
    _kitchen, _office = speakers
    backend = SonosBackend(discover=lambda **kw: speakers)
    assert backend.state("kitchen")["title"] == "A song"
    assert backend.command("office", "next")["title"] == "Next song"
    assert backend.state("kitchen")["title"] == "Next song"


def test_failed_mutation_is_not_retried_and_cache_is_cleared(speakers):
    kitchen, _office = speakers
    backend = SonosBackend(discover=lambda **kw: speakers)
    backend.state("office")

    def uncertain_next():
        kitchen.skips += 1
        kitchen.title = "Changed despite timeout"
        raise requests.Timeout("timeout")

    kitchen.next = uncertain_next
    with pytest.raises(BackendError) as err:
        backend.command("office", "next")
    assert err.value.status == 503
    assert kitchen.skips == 1
    assert backend.state("office")["title"] == "Changed despite timeout"


def test_discovery_recovers_and_state_expires(speakers):
    now, found = [0], []
    backend = SonosBackend(discover=lambda **kw: found, clock=lambda: now[0])
    assert backend.rooms() == []
    found.extend(speakers)
    assert backend.state("office")["title"] == "A song"
    speakers[0].title = "Changed"
    assert backend.state("office")["title"] == "A song"
    now[0] = 3
    assert backend.state("office")["title"] == "Changed"


def test_discovery_failure_does_not_prevent_recovery(speakers):
    fail = [True]

    def discover(**kw):
        if fail[0]:
            raise requests.Timeout()
        return speakers

    backend = SonosBackend(discover=discover)
    assert backend.rooms() == []
    fail[0] = False
    assert backend.state("office")["room"] == "Office"


@pytest.mark.parametrize(
    "action,expected", [("play", "PLAYING"), ("pause", "PAUSED_PLAYBACK"), ("play_pause", "PAUSED_PLAYBACK")]
)
def test_transport_actions(speakers, action, expected):
    backend = SonosBackend(discover=lambda **kw: speakers)
    assert backend.command("office", action)["playback_state"] == expected


def test_malformed_artwork_does_not_break_state(speakers):
    speakers[0].get_current_track_info = lambda: {"title": "Song", "album_art": "http://[broken"}
    state = SonosBackend(discover=lambda **kw: speakers).state("office")
    assert state["title"] == "Song"
    assert state["artwork_url"] == ""


def test_unsupported_transport_returns_conflict(speakers):
    def unsupported():
        raise NotSupportedException("Source cannot skip")

    speakers[0].next = unsupported
    with pytest.raises(BackendError) as err:
        SonosBackend(discover=lambda **kw: speakers).command("office", "next")
    assert err.value.status == 409


@pytest.mark.parametrize("interface", [None, "", "192.0.2.20"])
def test_discovery_interface_is_installation_configuration(monkeypatch, interface):
    if interface is None:
        monkeypatch.delenv("SONOS_INTERFACE", raising=False)
    else:
        monkeypatch.setenv("SONOS_INTERFACE", interface)
    calls = []
    backend = SonosBackend(discover=lambda **kwargs: calls.append(kwargs) or [])
    assert backend.rooms() == []
    assert calls == [{"timeout": 3, "interface_addr": interface or None}]
