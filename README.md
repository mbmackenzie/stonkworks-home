# STONKWORKS HOME

Local-first household devices and services. Initial extraction: Sonos Desk's
FastAPI/SoCo backend, browser controller, and Pillow display renderer.

## Structure

- `services/sonos-desk/`: source, Dockerfile, pinned dependencies, and tests.
- `deploy/stacks/sonos-desk/`: portable Linux host-network Compose example.
- `docs/sonos-desk.md`: HTTP API, rendering, and development instructions.
- `firmware/sonos-desk/`: Rust ESP32-C6 Sonos controls and diagnostics.

Sonos Desk optionally publishes room state and accepts commands over MQTT 5;
see [the protocol](protocols/v1/sonos-desk.md) and
[configuration](docs/sonos-desk.md#optional-mqtt).
Sonos Desk firmware controls one configured room using buttons and an encoder
over MQTT 5 with verified TLS. Separate input/network diagnostic images remain available;
see [the firmware walkthrough](firmware/sonos-desk/README.md).
From `firmware/sonos-desk/`, `just firmware-monitor` opens the USB monitor
after the local tool setup.
The `display-check` firmware draws a fixed e-ink test pattern.
The `frame-check` image fetches and displays one monochrome Sonos frame over HTTP.
The controller redraws the screen with partial refreshes when volume, playback or the track changes, with idle cleanup, while MQTT controls run.
Home management is future work. Existing household applications remain outside this project.

## Run from source

Copy `deploy/stacks/sonos-desk/.env.example` to `.env` in the same directory
and replace its documentation address with this host's trusted LAN IPv4 address.
Then run from the repository root:

```sh
docker compose -f deploy/stacks/sonos-desk/compose.yaml config --quiet
docker compose -f deploy/stacks/sonos-desk/compose.yaml up -d --build
```

Open `http://<LAN-IP>:8008/`. The API has no authentication: use a trusted LAN.
Linux host networking supports SSDP discovery. No persistent volume is needed.
The image alone defaults to loopback; Compose explicitly sets the LAN listener.
Keep one worker to preserve shared command locks and caches.

## Development

```sh
uv sync --all-packages --locked
uv run --package sonos-desk pytest -q
```

Tests use simulated speakers and do not control household devices.

Run locally with `uv run --package sonos-desk sonos-desk`, which listens on
`http://127.0.0.1:8008/`. Build the image from the repository root with
`docker build -f services/sonos-desk/Dockerfile -t stonkworks/sonos-desk:local .`.

## Distribution status

This is a local source extraction. No registry release or firmware is published.
License selection and publication are pending; no open-source license is granted
yet. Household deployment definitions remain in their installation repository.
