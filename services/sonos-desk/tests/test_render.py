from contextlib import ExitStack
from io import BytesIO
from types import SimpleNamespace

import httpx
import pytest
from PIL import Image
from sonos_desk.render import Renderer


def png(color="gray", size=(80, 80)):
    out = BytesIO()
    Image.new("RGB", size, color).save(out, format="PNG")
    return out.getvalue()


STATE = {
    "room": "Office",
    "room_id": "uid",
    "title": "A song",
    "artist": "An artist",
    "album": "An album",
    "playback_state": "PLAYING",
    "volume": 40,
    "mute": False,
    "artwork_url": "http://speaker:1400/art",
}


@pytest.fixture
def artwork_client():
    with ExitStack() as stack:

        def make(body):
            source = SimpleNamespace(body=body, calls=0)

            def respond(request):
                source.calls += 1
                if isinstance(source.body, Exception):
                    raise source.body
                return httpx.Response(200, stream=httpx.ByteStream(source.body))

            client = stack.enter_context(httpx.Client(transport=httpx.MockTransport(respond)))
            return client, source

        yield make


@pytest.mark.parametrize("mode,levels", [("gray4", {0, 85, 170, 255}), ("mono", {0, 255})])
def test_dimensions_and_levels(mode, levels, artwork_client):
    client, _ = artwork_client(png())
    renderer = Renderer(client=client)
    im = Image.open(BytesIO(renderer.frame(STATE, mode)))
    assert im.size == (800, 480)
    assert set(im.convert("L").get_flattened_data()) <= levels


def test_artwork_appears_and_state_changes_but_position_does_not(artwork_client):
    client, _ = artwork_client(png("black"))
    renderer = Renderer(client=client)
    frame = renderer.frame(STATE)
    image = Image.open(BytesIO(frame)).convert("L")
    assert image.getpixel((180, 180)) == 0
    assert renderer.frame(dict(STATE, position="0:01:02")) == frame
    assert renderer.frame(dict(STATE, volume=41)) != frame
    assert renderer.frame(dict(STATE, title="Different title")) != frame
    client, _ = artwork_client(b"invalid")
    placeholder = Renderer(client=client).frame(STATE)
    assert placeholder != frame


@pytest.mark.parametrize(
    "body",
    [b"not an image", httpx.ReadTimeout("timeout"), b"x" * (5 * 1024 * 1024 + 1), png(size=(4100, 4100))],
    ids=["invalid-image", "timeout", "oversized-body", "oversized-image"],
)
def test_failed_artwork_is_placeholder_and_retried(body, artwork_client):
    now = [0]
    client, source = artwork_client(body)
    renderer = Renderer(client=client, clock=lambda: now[0])
    frame = renderer.frame(STATE)
    assert Image.open(BytesIO(frame)).size == (800, 480)
    source.body = png("black")
    assert renderer.frame(STATE) == frame
    assert source.calls == 1
    now[0] = 31
    assert renderer.frame(STATE) != frame
    assert source.calls == 2


def test_long_metadata_and_no_artwork(artwork_client):
    state = dict(
        STATE, title="X" * 5000, artist="你好 — Björk " * 300, album="", room="Very long room " * 100, artwork_url=""
    )
    client, source = artwork_client(httpx.ReadTimeout("timeout"))
    frame = Renderer(client=client).frame(state)
    assert Image.open(BytesIO(frame)).size == (800, 480)
    assert source.calls == 0


def test_caches_are_bounded_and_unsafe_schemes_are_not_fetched(artwork_client):
    client, source = artwork_client(png())
    renderer = Renderer(client=client)
    renderer.frame(dict(STATE, artwork_url="file:///etc/passwd"))
    assert source.calls == 0
    for number in range(40):
        renderer.frame(dict(STATE, title=str(number), artwork_url=f"http://speaker/{number}"))
    assert len(renderer.artwork) <= 16
    assert len(renderer.frames) <= 32


def test_malformed_url_is_a_placeholder(artwork_client):
    client, source = artwork_client(png())
    frame = Renderer(client=client).frame(dict(STATE, artwork_url="http://[broken"))
    assert Image.open(BytesIO(frame)).size == (800, 480)
    assert source.calls == 0


def test_slow_stream_is_stopped_before_accumulating_a_full_chunk():
    now, reads, closed = [0], [0], []

    class Trickle(httpx.SyncByteStream):
        def __iter__(self):
            for _ in range(100):
                now[0] += 2
                reads[0] += 1
                yield b"x"

        def close(self):
            closed.append(True)

    transport = httpx.MockTransport(lambda request: httpx.Response(200, stream=Trickle()))
    with httpx.Client(transport=transport) as client:
        frame = Renderer(client=client, clock=lambda: now[0]).frame(STATE)
    assert Image.open(BytesIO(frame)).size == (800, 480)
    assert reads[0] == 6
    assert closed


@pytest.mark.parametrize("failure", ["status", "timeout", "disconnect", "size"])
def test_stream_failures_close_response_and_use_placeholder(failure):
    closed = []

    class BrokenStream(httpx.SyncByteStream):
        def __iter__(self):
            yield b"x"
            if failure == "timeout":
                raise httpx.ReadTimeout("timeout")
            if failure == "disconnect":
                raise httpx.RemoteProtocolError("disconnected")
            yield b"x" * (5 * 1024 * 1024)

        def close(self):
            closed.append(True)

    def respond(request):
        return httpx.Response(503 if failure == "status" else 200, stream=BrokenStream())

    with httpx.Client(transport=httpx.MockTransport(respond)) as client:
        frame = Renderer(client=client).frame(STATE)
    assert Image.open(BytesIO(frame)).convert("L").getpixel((180, 180)) == 255
    assert closed


def test_artwork_follows_redirects_and_stops_redirect_loops():
    seen = []

    def respond(request):
        seen.append(request.url.path)
        if request.url.path == "/cover":
            return httpx.Response(200, stream=httpx.ByteStream(png("black")))
        location = "/cover" if request.url.path == "/art" else "/loop"
        return httpx.Response(302, headers={"Location": location})

    with httpx.Client(transport=httpx.MockTransport(respond), max_redirects=3) as client:
        renderer = Renderer(client=client)
        frame = renderer.frame(STATE)
        assert Image.open(BytesIO(frame)).convert("L").getpixel((180, 180)) == 0
        assert seen == ["/art", "/cover"]
        seen.clear()
        frame = renderer.frame(dict(STATE, artwork_url="http://speaker/loop"))
        assert Image.open(BytesIO(frame)).convert("L").getpixel((180, 180)) == 255
        assert seen == ["/loop"] * 4
