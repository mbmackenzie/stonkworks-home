import pytest
import requests
from fastapi.testclient import TestClient
from sonos_desk.app import create_app
from sonos_desk.render import Renderer
from sonos_desk.sonos import SonosBackend
from test_sonos import Speaker


@pytest.fixture
def setup():
    speaker = Speaker("Office", "uid-office")
    speaker.get_current_track_info = lambda: {
        "title": speaker.title,
        "artist": "Artist",
        "album": "Album",
        "album_art": "",
    }
    backend = SonosBackend(discover=lambda **kw: [speaker])
    with TestClient(create_app(backend, Renderer())) as client:
        yield client, speaker


def test_live_contract_and_error_codes(setup):
    client, _speaker = setup
    assert client.get("/health").json()["status"] == "ok"
    assert client.get("/rooms").json()[0]["room"] == "Office"
    assert client.get("/state?room=OFFICE").json()["title"] == "A song"
    assert client.get("/state?room=missing").status_code == 404
    assert client.get("/state").status_code == 422
    assert (
        client.post("/command", json={"room": "office", "command": "pause"}).json()["playback_state"]
        == "PAUSED_PLAYBACK"
    )
    assert client.post("/command", json={"room": "office", "command": "volume", "value": 0}).json()["volume"] == 0
    assert client.post("/command", json={"room": "office", "command": "mute", "value": False}).json()["mute"] is False


@pytest.mark.parametrize(
    "payload",
    [
        {"command": "volume"},
        {"command": "volume", "value": True},
        {"command": "volume", "value": "35"},
        {"command": "volume", "value": 101},
        {"command": "volume", "value": -1},
        {"command": "volume", "value": 1.5},
        {"command": "volume_relative", "value": 101},
        {"command": "volume_relative", "value": False},
        {"command": "mute", "value": 1},
        {"command": "mute", "value": "true"},
        {"command": "mute"},
        {"command": "pause", "value": 1},
        {"command": "pause", "value": None},
        {"command": "unknown"},
        {"command": "pause", "extra": "bad"},
    ],
)
def test_reject_invalid_commands_without_changing_speaker(setup, payload):
    client, speaker = setup
    response = client.post("/command", json={"room": "office", **payload})
    assert response.status_code == 422
    assert speaker.volume == 40
    assert speaker.transport == "PLAYING"


def test_frames_and_conditional_requests(setup):
    client, _speaker = setup
    first = client.get("/frame?room=office")
    assert first.status_code == 200
    assert first.headers["content-type"] == "image/png"
    assert first.content.startswith(b"\x89PNG\r\n\x1a\n")
    etag = first.headers["etag"]
    assert etag.startswith('"') and etag.endswith('"')
    for match in (etag, "W/" + etag, '"other", ' + etag, "*"):
        response = client.get("/frame?room=office", headers={"If-None-Match": match})
        assert response.status_code == 304
        assert response.content == b""
    assert client.get("/frame?room=office&mode=mono").headers["etag"] != etag
    assert client.get("/frame?room=office&mode=invalid").status_code == 422
    client.post("/command", json={"room": "office", "command": "volume", "value": 35})
    assert client.get("/frame?room=office").headers["etag"] != etag


def test_service_lives_without_speakers():
    backend = SonosBackend(discover=lambda **kw: [])
    with TestClient(create_app(backend, Renderer())) as client:
        assert client.get("/health").status_code == 200
        assert client.get("/health").json()["rooms"] == 0
        assert client.get("/rooms").json() == []
        assert client.get("/frame?room=office").status_code == 404


def test_failed_state_read_after_command_does_not_repeat_mutation(setup):
    client, speaker = setup

    def skip_then_break():
        speaker.skips += 1
        speaker.get_current_track_info = lambda: (_ for _ in ()).throw(requests.Timeout())

    speaker.next = skip_then_break
    response = client.post("/command", json={"room": "office", "command": "next"})
    assert response.status_code == 503
    assert "state refresh failed" in response.json()["detail"]
    assert speaker.skips == 1


def test_duplicate_room_and_unreachable_speaker():
    speakers = [Speaker("Office", "uid1"), Speaker("Office", "uid2")]
    backend = SonosBackend(discover=lambda **kw: speakers)
    with TestClient(create_app(backend, Renderer())) as client:
        assert client.get("/state?room=office").status_code == 409
        speakers[0].get_current_track_info = lambda: (_ for _ in ()).throw(requests.Timeout())
        assert client.get("/state?room=uid1").status_code == 503


def test_browser_page_is_self_contained_and_has_physical_controls(setup):
    client, _speaker = setup
    response = client.get("/")
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/html")
    # The behavioral browser check exercises these controls; this checks that
    # the packaged page is served, including when installed in the image.
    for element in (
        'id="frame"',
        'id="room"',
        'id="previous"',
        'id="play-pause"',
        'id="next"',
        'id="volume"',
        'id="mute"',
    ):
        assert element in response.text
    assert "https://cdn" not in response.text


def test_shutdown_closes_renderer_owned_http_client():
    renderer = Renderer()
    backend = SonosBackend(discover=lambda **kwargs: [])
    with TestClient(create_app(backend, renderer)) as client:
        assert client.get("/health").status_code == 200
        assert not renderer.client.is_closed
    assert renderer.client.is_closed


def test_shutdown_leaves_injected_http_client_open():
    import httpx

    with httpx.Client() as artwork_client:
        renderer = Renderer(client=artwork_client)
        backend = SonosBackend(discover=lambda **kwargs: [])
        with TestClient(create_app(backend, renderer)) as client:
            assert client.get("/health").status_code == 200
        assert not artwork_client.is_closed


def test_startup_failure_closes_renderer_owned_http_client():
    renderer = Renderer()

    def failed_discovery(**kwargs):
        raise RuntimeError("Unexpected discovery failure")

    backend = SonosBackend(discover=failed_discovery)
    with pytest.raises(RuntimeError, match="Unexpected discovery failure"), TestClient(create_app(backend, renderer)):
        pass
    assert renderer.client.is_closed
