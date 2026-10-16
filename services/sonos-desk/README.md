# Sonos Desk

A local FastAPI service for Sonos discovery, playback controls, and 800 × 480
e-ink display frames. The bundled browser controller is served at `/`.

## Development

Requires Python 3.13 or newer and uv. From the repository root:

```sh
uv sync --all-packages --locked
uv run --package sonos-desk sonos-desk
```

Open `http://127.0.0.1:8008/`. The service discovers speakers on the local network.
Set `SONOS_INTERFACE` to the host's LAN IPv4 address to select a discovery interface.

```sh
uv run --package sonos-desk pytest -q
uv run ruff check services/sonos-desk
```

Tests use simulated speakers and artwork downloads without controlling live devices.
The optional Playwright check in `tests/browser_check.py` requires a separate
Playwright installation and a Chromium CDP endpoint.

## Package structure

- `app.py`: application factory, startup discovery, error handling, and shutdown.
- `routes.py`: HTTP endpoints and command validation.
- `sonos.py`: speaker discovery, cached state, and serialized commands through SoCo.
- `render.py`: artwork downloads through HTTPX and cached Pillow display frames.
- `templates/browser.html`: self-contained browser controller, included in the package.

The renderer closes HTTP clients it creates when the application shuts down.
Callers that inject an HTTP client retain responsibility for closing it.

## Deployment and API

Build from the repository root so Docker can use the workspace lockfile:

```sh
docker build -f services/sonos-desk/Dockerfile -t stonkworks/sonos-desk:local .
```

See the [repository README](../../README.md) for Compose deployment and
the [API documentation](../../docs/sonos-desk.md) for endpoints and polling behavior.
Use one worker to preserve shared locks and caches. The API has no authentication;
bind it only to a trusted LAN.
