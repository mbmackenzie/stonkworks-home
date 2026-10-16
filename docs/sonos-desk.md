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
