# Sonos Desk

See [the README](../README.md) for deployment and testing. Addresses below
are documentation examples; substitute your installation address.

## API and commands

`GET /rooms` returns an array of rooms: `room`, `room_id`, `ip`, `coordinator`,
and `coordinator_id`. Use a case-insensitive room name or the stable Sonos
`room_id`. Duplicate names return 409; use their IDs instead. URL-encode names
containing spaces, for example `/state?room=Living%20Room`.

`GET /state?room=office` returns:

```json
{
  "room": "Office",
  "room_id": "RINCON_...",
  "coordinator": "Office",
  "coordinator_id": "RINCON_...",
  "title": "Lump Sum",
  "artist": "Bon Iver",
  "album": "For Emma, Forever Ago",
  "artwork_url": "http://192.0.2.11:1400/getaa?...",
  "playback_state": "PLAYING",
  "volume": 40,
  "mute": false,
  "duration": "0:03:21"
}
```

`POST /command` accepts JSON and returns newly read state. Playback and metadata
follow the group coordinator; volume and mute apply only to the selected room.

| Command | `value` | Typical control |
| --- | --- | --- |
| `play_pause` | Omit | Middle button |
| `next` | Omit | Right button |
| `previous` | Omit | Left button |
| `play`, `pause` | Omit | Explicit playback control |
| `volume` | Integer 0–100 | Absolute volume |
| `volume_relative` | Integer -100–100 | Encoder increment, typically ±1 |
| `mute` | Boolean | Explicit mute/unmute |

Examples:

```bash
curl -fsS http://192.0.2.20:8008/command \
  -H 'Content-Type: application/json' \
  -d '{"room":"office","command":"play_pause"}'

curl -fsS http://192.0.2.20:8008/command \
  -H 'Content-Type: application/json' \
  -d '{"room":"office","command":"volume_relative","value":-1}'

curl -fsS http://192.0.2.20:8008/command \
  -H 'Content-Type: application/json' \
  -d '{"room":"office","command":"mute","value":true}'
```

Numbers must be JSON integers, not strings or booleans. Mute requires a boolean.
Extra fields and values on playback commands are rejected. Relative volume
clamps at 0 and 100. Commands are serialized to protect encoder increments.

| Status | Meaning |
| --- | --- |
| 404 | Room not found |
| 409 | Ambiguous name or action unsupported by the source/queue |
| 422 | Invalid command, room parameter, or render mode |
| 503 | Speaker/discovery unavailable, uncertain command, or post-command state read failed |

Errors have a JSON `detail` field. A 503 after sending a command can mean it
took effect before the response failed. Read state before retrying; do not
blindly retry a skip, toggle, or relative-volume command. Successful commands
may return `TRANSITIONING`; poll for the settled transport state. A timeout on
the ESP32 also leaves the outcome uncertain.

## Frames and polling

`GET /frame?room=office` returns an 800 × 480 grayscale PNG. The default
`mode=gray4` uses exactly the allowed intensities 0, 85, 170, 255.
`mode=mono` uses black and white. Artwork is dithered; text remains crisp.
Track/artist/album are wrapped and truncated within the display bounds. Missing
or bad artwork uses a record placeholder. Playback progress does not redraw
the display.

`format=bitmap` (valid only with `mode=mono`, otherwise 422) returns the same image
as 48,000 raw bytes, `application/octet-stream`: 100 bytes per row, most significant
bit first, 1 = white, 0 = black. It exists for small controllers that cannot afford a PNG
decoder; it has its own ETag and honors `If-None-Match` like the PNG. `format=png` is the
default.

Start with a frame request every 3–5 seconds. Store the response's quoted
`ETag`, then send it as `If-None-Match` on subsequent frame requests. A 304
contains no image body: keep the current display without refreshing it. Use
separate ETags for each room and render mode. After a command, poll state and
fetch the next frame; the one-second state TTL can briefly show a transition.

Discovery is refreshed on requests after 60 seconds or a room lookup miss.
State is cached for one second. Artwork caches hold 16 resized images for up to
one hour; failures retry after 30 seconds. Frame caches hold 32 PNGs and include
the decoded artwork identity, so recovered artwork replaces a placeholder.
Caches are bounded in memory and safely disappear on restart. No persistent
mount or database is required.

Artwork downloads use only HTTP(S), a 5 MiB byte limit, 16-million-pixel decode
limit, at most three redirects, and 2/3-second connection/read timeouts. Body
reads check a ten-second deadline between socket reads (an in-progress read
can add up to its timeout). Speaker metadata, not API callers, supplies artwork
URLs. Image transfer requests uncompressed HTTP bodies.

`GET /health` reports service liveness, discovered-room count, and whether the
last discovery failed. It never queries speakers. Zero rooms is valid for
service health; `/rooms` and `/state` establish actual Sonos availability.

## Optional MQTT

Sonos Desk can publish every discovered room and accept commands using MQTT 5.
See the [v1 protocol](../protocols/v1/sonos-desk.md) for topics, schemas,
freshness, command expiry, and uncertain outcomes. Frames remain on HTTP.
MQTT is disabled unless `SONOS_MQTT_HOST` is configured. Broker outages do not
stop HTTP. One application worker is still required.

| Variable | Behavior |
| --- | --- |
| `SONOS_MQTT_HOST` | Broker hostname; unset disables MQTT |
| `SONOS_MQTT_PORT` | 1883, or 8883 for TLS when running directly |
| `SONOS_MQTT_USERNAME` | Required application account |
| `SONOS_MQTT_PASSWORD_FILE` | Required UTF-8 password file; one trailing line ending removed |
| `SONOS_MQTT_INSTANCE` | `main`; unique among services on the broker |
| `SONOS_MQTT_POLL_INTERVAL` | Three seconds per room by default; positive finite number |
| `SONOS_MQTT_TLS` | `true` or `false`; default false |
| `SONOS_MQTT_CA_FILE` | Optional trusted CA file for TLS; system trust otherwise |

Create a separate local password file readable by the service; do not commit
it. Keep secrets outside the checkout and mount only the required application's
password, not a broker-wide credential store. Plain TCP sends credentials and
payloads without encryption; use it only on a trusted LAN. TLS always verifies
hostname and certificate trust. Device credential provisioning is future work.

For Compose, populate the optional MQTT values in your local `.env` and use:

```sh
docker compose -f deploy/stacks/sonos-desk/compose.yaml \
  -f deploy/stacks/sonos-desk/compose.mqtt.yaml config --quiet
```

Use those same files with your normal stack installation workflow. The overlay
mounts `SONOS_MQTT_PASSWORD_PATH` read-only at
`/run/secrets/sonos-mqtt-password` and refuses to create a missing host path.
The base Compose example needs no password file when MQTT is disabled. For TLS,
set the overlay's explicit `SONOS_MQTT_PORT` to your TLS listener (usually 8883).
For a private CA, add a read-only bind mount in your local override and point
`SONOS_MQTT_CA_FILE` at its container path. Do not disable certificate checks.

`GET /mqtt/status` returns cached `enabled`, `connected`, `service_session`,
`last_connected_at`, `last_poll_completed_at`, `rooms_known`,
`rooms_unavailable`, and sanitized `last_error`. Disabled state has false
booleans, zero counters, and null session/times/error. No status request
contacts speakers or the broker; `/health` keeps its existing liveness contract.

### MQTT verification

Normal tests use simulated speakers and fake transports. The optional MQTT
integration suite starts a disposable Mosquitto 2.0.22 container with generated
credentials, temporary persistence, a random loopback port, and its own topic
instances. It never uses household credentials or restarts an existing broker.
Docker access and the broker image are required. Without explicit opt-in these
checks skip; with opt-in, unavailable Docker/image startup is a test failure.

```sh
SONOS_MQTT_INTEGRATION=1 uv run --package sonos-desk pytest -q \
  -m mqtt_integration services/sonos-desk/tests/test_mqtt_integration.py
```

The suite covers actual command/results, retained state, duplicate and retained
commands, backend-lock expiry, uncertain writes, process session changes,
abrupt-disconnect Last Will, broker restart with HTTP still serving, broker
message expiry, authentication denial, and the example software client.
