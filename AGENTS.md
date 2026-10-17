# Stonkworks Home contributor guidance

## Purpose and scope

Stonkworks Home is a local-first, distributable system for custom household
devices. This repository owns reusable software, firmware, protocols, and
portable deployment definitions. Each household supplies its own credentials,
network settings, and device assignments.

The current implementation is the Sonos Desk source extraction: a FastAPI/SoCo
backend, browser controller, and Pillow e-ink renderer. Home management, MQTT,
device onboarding, firmware, registry releases, and Pi packaging are future
work. Do not describe them as implemented or scaffold empty subsystems.

Scope comes from the “Clarify Stonkworks Home Scope” conversation
(`01a14624-ab48-78e1-ade3-d3f2892a6573`) and its proposal. On the reference host,
the proposal lives at `/opt/homeserver/docs/stonkworks-home-proposal.md`; it is
background context, not a portable build dependency. Use the repository's
current source and documentation to establish implemented behavior.

## Ownership and intended architecture

- This checkout, `/opt/stonkworks-home`, owns shared product source. The separate
  `/opt/homeserver` repository owns this household's deployed stacks and local
  infrastructure. Do not change that installation as a side effect of source work.
- Stonkworks Home is intended to own the everyday UI, integration settings,
  device registration, names, speaker assignments, browser controls, and status.
  It configures installed services; v1 does not install containers.
- Dockge/Compose owns stack installation, container lifecycle, and upgrades.
  Home should not require Docker control permissions or become a container manager.
- Sonos Desk owns Sonos discovery, playback, state, and display rendering.
  Existing applications such as Mealie and Home Assistant are outside the
  Stonkworks service catalog.
- The intended MQTT broker is a separate stack, with support for an existing
  broker. Home will persist authoritative assignments and publish retained
  configuration using stable speaker IDs. Commands must not be retained;
  large rendered frames stay on HTTP. Topics, schemas, authentication, and
  duplicate handling still require design.
- Home should remain usable when an integration fails. Configured controllers
  should eventually continue operating through a temporary Home outage using
  their last configuration, provided MQTT and their integration remain available.
- A future Pi installation uses the same software and selected stacks. Normal
  household operation must not depend on the developer's server or a cloud service.

## Repository map

- `services/sonos-desk/src/sonos_desk/`: application factory (`app.py`), HTTP
  validation/routes (`routes.py`), SoCo integration (`sonos.py`), Pillow rendering
  (`render.py`), and packaged browser UI (`templates/browser.html`).
- `services/sonos-desk/tests/`: simulated-speaker API, backend, and renderer
  tests; optional Playwright browser check.
- `deploy/stacks/sonos-desk/`: portable Linux host-network Compose example and
  `.env.example`, not the household's deployed stack.
- `docs/sonos-desk.md`: HTTP contract, command semantics, rendering, and polling.
- Root `pyproject.toml` and `uv.lock`: Python 3.13+ uv workspace and shared lockfile.

Introduce `hub/`, `firmware/`, `protocols/`, or additional stacks when their first
implementation is requested. Extract shared abstractions when actual reuse exists.

## Development and validation

Run commands from the repository root:

```sh
uv sync --all-packages --locked
uv run --package sonos-desk pytest -q
uv run ruff check services/sonos-desk
uv run ruff format --check services/sonos-desk
uv run ty check
```

The pre-commit configuration also defines whitespace, YAML, Python, Ruff, and
ty checks; `uv run prek run --all-files` runs the configured hooks. Run checks
appropriate to the change and report actual results. Documentation-only edits
do not require speaker access or container deployment.

`uv run --package sonos-desk sonos-desk` starts the service on
`127.0.0.1:8008` and discovers real LAN speakers. Unit tests use simulated
speakers and artwork downloads. Prefer those tests for verification; do not
change real playback, volume, or mute unless the task calls for device control.

`tests/browser_check.py` under the service is optional and needs a separate
Playwright installation plus `SONOS_CDP_URL`. See its module instructions;
`SONOS_BROWSER_URL` optionally checks a deployed page with read-only requests.

## Sonos behavior to preserve

- Preserve HTTP behavior documented in `docs/sonos-desk.md`. Update documentation
  when deliberately changing a contract.
- Playback and metadata follow the group coordinator; volume and mute affect
  the selected room. Use stable IDs to disambiguate duplicate room names.
- Keep command serialization and strict input validation. A failed response
  after sending a command may have an uncertain outcome; do not blindly retry
  skip, toggle, or relative-volume operations.
- Preserve 800 × 480 PNG frames, the documented mono/gray4 palettes, conditional
  requests with ETags, bounded caches, and artwork download/decode limits.
- `/health` reports liveness and cached discovery status without querying speakers.
  Zero discovered rooms is not itself a failed liveness check.
- Keep one application worker: command locks and caches are shared in memory.
  Sonos Desk currently needs no persistent database or volume.

## Deployment and publication

Build from the repository root so Docker can access workspace metadata:

```sh
docker build -f services/sonos-desk/Dockerfile -t stonkworks/sonos-desk:local .
```

The image defaults to loopback. The Compose example requires `SONOS_INTERFACE`
to select an explicit trusted LAN IPv4 address for discovery and the listener.
Copy its `.env.example` to a local `.env` and set that address before validating
with `docker compose -f deploy/stacks/sonos-desk/compose.yaml config --quiet`.
The API has no authentication; preserve trusted-LAN deployment guidance.

Keep credentials, actual device assignments, and household-specific addresses
out of shared source and examples. Use configurable inputs and documentation
addresses. Do not copy household app data or deployment secrets into this repo.

No registry release or firmware is published, and no open-source license has
been selected or granted yet. Publication, licensing, and live deployment are
separate work; initialization does not authorize them.
