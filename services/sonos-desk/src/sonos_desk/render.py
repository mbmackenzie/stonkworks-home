"""Server-rendered, stable e-ink frames with bounded in-memory artwork caches."""

import logging
import threading
import time
import warnings
from collections import OrderedDict
from hashlib import sha256
from io import BytesIO
from urllib.parse import urlparse

import httpx
from PIL import Image, ImageDraw, ImageFont, ImageOps, UnidentifiedImageError

log = logging.getLogger(__name__)
MAX_BYTES = 5 * 1024 * 1024
MAX_PIXELS = 16_000_000
# A track change passes through TRANSITIONING; frames keep the settled label this long.
TRANSITION_HOLD_SECONDS = 10


def font(size, bold=False):
    path = "/usr/share/fonts/truetype/dejavu/DejaVuSans"
    try:
        return ImageFont.truetype(path + ("-Bold.ttf" if bold else ".ttf"), size)
    except OSError:
        return ImageFont.load_default(size=size)


def lines(draw, text, face, width, count):
    # Pixel-based wrapping also handles unbroken names and long radio metadata.
    text = " ".join(str(text or "").split())[:4096]
    result = []
    while text and len(result) < count:
        end = 1
        while end < len(text) and draw.textlength(text[: end + 1], font=face) <= width:
            end += 1
        if end < len(text) and " " in text[:end]:
            end = text.rfind(" ", 0, end) or end
        part, text = text[:end].rstrip(), text[end:].lstrip()
        if len(result) == count - 1 and text:
            while part and draw.textlength(part + "…", font=face) > width:
                part = part[:-1]
            part += "…"
        result.append(part)
    return result


class Renderer:
    def __init__(self, client=None, clock=time.monotonic):
        self._owns_client = client is None
        self.client = httpx.Client(max_redirects=3, trust_env=False) if client is None else client
        self.clock = clock
        self.artwork, self.frames, self.transport = OrderedDict(), OrderedDict(), OrderedDict()
        self.lock = threading.Lock()

    def close(self):
        if self._owns_client:
            self.client.close()

    @staticmethod
    def _put(cache, key, value, limit):
        cache[key] = value
        cache.move_to_end(key)
        while len(cache) > limit:
            cache.popitem(last=False)

    def _transport(self, state):
        """The playback_state to draw: hold the settled label through a brief TRANSITIONING.

        The header word changing twice per track change makes thin text ghost on the
        partially refreshed e-ink panel. /state still reports the real state.
        """
        key = state.get("room_id") or state.get("room", "")
        current = state.get("playback_state", "")
        entry = self.transport.get(key)
        if current != "TRANSITIONING":
            self._put(self.transport, key, {"settled": current, "since": None}, 32)
            return current
        if entry is None:
            return current
        if entry["since"] is None:
            entry["since"] = self.clock()
        if self.clock() - entry["since"] >= TRANSITION_HOLD_SECONDS:
            return current
        return entry["settled"]

    def _art(self, url):
        cached = self.artwork.get(url)
        if cached and self.clock() < cached[0]:
            self.artwork.move_to_end(url)
            return cached[1]
        image = None
        try:
            if not url or urlparse(url).scheme not in {"http", "https"}:
                return None
            started = self.clock()
            with self.client.stream(
                "GET",
                url,
                timeout=httpx.Timeout(3, connect=2),
                follow_redirects=True,
                headers={"Accept-Encoding": "identity"},
            ) as response:
                response.raise_for_status()
                if int(response.headers.get("Content-Length", 0)) > MAX_BYTES:
                    raise ValueError("Artwork exceeds download limit")
                data = bytearray()
                # No chunk size: yield available bytes so trickling responses
                # cannot postpone deadline checks while filling a buffer.
                for chunk in response.iter_raw():
                    if self.clock() - started > 10:
                        raise ValueError("Artwork exceeds time limit")
                    data.extend(chunk)
                    if len(data) > MAX_BYTES or self.clock() - started > 10:
                        raise ValueError("Artwork exceeds download or time limit")
            with warnings.catch_warnings():
                warnings.simplefilter("error", Image.DecompressionBombWarning)
                with Image.open(BytesIO(data)) as source:
                    if source.width * source.height > MAX_PIXELS:
                        raise ValueError("Artwork exceeds pixel limit")
                    image = ImageOps.fit(
                        ImageOps.exif_transpose(source).convert("L"), (360, 360), method=Image.Resampling.LANCZOS
                    )
        except (
            httpx.HTTPError,
            httpx.InvalidURL,
            OSError,
            ValueError,
            UnidentifiedImageError,
            Image.DecompressionBombError,
            Image.DecompressionBombWarning,
        ):
            log.warning("Artwork unavailable; using placeholder")
        self._put(self.artwork, url, (self.clock() + (3600 if image else 30), image), 16)
        return image

    def bitmap(self, state):
        """The mono frame as 48,000 packed bytes: 100 per row, MSB first, 1 = white."""
        with Image.open(BytesIO(self.frame(state, "mono"))) as image:
            return image.convert("1", dither=Image.Dither.NONE).tobytes()

    def frame(self, state, mode="gray4"):
        with self.lock:
            state = dict(state, playback_state=self._transport(state))
            art = self._art(state.get("artwork_url", ""))
            art_id = sha256(art.tobytes()).digest() if art else b""
            visible = ("room", "title", "artist", "album", "playback_state", "volume", "mute")
            key = (mode, tuple(state.get(k, "") for k in visible), art_id)
            if key in self.frames:
                self.frames.move_to_end(key)
                return self.frames[key]

            canvas = Image.new("L", (800, 480), 255)
            draw = ImageDraw.Draw(canvas)
            heading, title, body, small = font(22, True), font(32, True), font(23), font(19)
            draw.text(
                (24, 18),
                lines(draw, state.get("room"), heading, 490, 1)[0] if state.get("room") else "Sonos",
                font=heading,
                fill=0,
            )
            transport = state.get("playback_state", "").replace("_PLAYBACK", "").replace("_", " ")
            draw.text((776, 22), transport[:24], font=small, fill=0, anchor="rt")
            draw.line((24, 57, 776, 57), fill=0, width=2)

            if art:
                if mode == "mono":
                    art = art.convert("1", dither=Image.Dither.FLOYDSTEINBERG).convert("L")
                else:
                    palette = Image.new("P", (1, 1))
                    palette.putpalette([v for level in (0, 85, 170, 255) for v in (level, level, level)] + [0] * 756)
                    art = art.convert("RGB").quantize(palette=palette, dither=Image.Dither.FLOYDSTEINBERG).convert("L")
                canvas.paste(art, (24, 76))
            else:
                draw.rectangle((24, 76, 383, 435), fill=255, outline=170, width=2)
                draw.ellipse((118, 170, 288, 340), outline=85, width=6)
                draw.ellipse((193, 245, 213, 265), fill=85)
                draw.text((204, 375), "SONOS", font=heading, fill=85, anchor="mt")

            y = 84
            for text, face, count, spacing in (
                (state.get("title") or "Nothing playing", title, 3, 40),
                (state.get("artist"), body, 2, 31),
                (state.get("album"), small, 2, 27),
            ):
                for line in lines(draw, text, face, 360, count):
                    draw.text((410, y), line, font=face, fill=0)
                    y += spacing
                y += 18

            volume = state.get("volume", 0)
            label = f"VOLUME {volume}" + ("  /  MUTED" if state.get("mute") else "")
            draw.text((410, 390), label, font=small, fill=0)
            draw.rectangle((410, 423, 769, 434), outline=0, width=1)
            if volume:
                draw.rectangle((412, 425, 412 + round(355 * volume / 100), 432), fill=0)
            draw.text((24, 454), "SONOS DESK", font=font(13), fill=85)

            # Quantize antialiased text too: PNG pixels use exactly the panel levels.
            canvas = canvas.point(
                lambda p: (255 if p >= 128 else 0) if mode == "mono" else min(255, ((p + 42) // 85) * 85)
            )
            output = BytesIO()
            canvas.save(output, format="PNG", optimize=True)
            result = output.getvalue()
            self._put(self.frames, key, result, 32)
            return result
