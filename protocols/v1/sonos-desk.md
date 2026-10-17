# Sonos Desk MQTT v1

MQTT is optional. This protocol publishes every discovered room; no Home,
controller assignments, or firmware are required. The service also keeps its
existing HTTP API and 800 × 480 PNG frame endpoint.

Base: `stonkworks/v1/sonos-desk/<instance>`, default instance `main`. Run one
service per instance. Use stable Sonos room IDs rather than room names.

| Suffix | Retained | Content |
| --- | --- | --- |
| `availability` | Yes | `status`, `service_session` |
| `rooms` | Yes | `service_session`, `observed_at`, `discovery`, `rooms` |
| `rooms/<room_id>/state` | Yes | `service_session`, `observed_at`, `state` |
| `rooms/<room_id>/availability` | Yes | `service_session`, `observed_at`, `status` |
| `rooms/<room_id>/command` | No | Command request |
| `clients/<client_id>/results` | No | Correlated command result |

Use MQTT 5 and QoS 1. JSON schemas are in [sonos-desk.schema.json](sonos-desk.schema.json),
under `$defs` (`command`, `availability`, `inventory`, `state`,
`room_availability`, `result`). Response objects may gain fields; commands
reject unknown fields. All times are UTC RFC 3339 strings.

State preserves the fields from HTTP `/state`; inventory preserves `/rooms`.
Playback follows the coordinator; volume and mute apply to the selected room.
The service refreshes state/room availability every 15 seconds and inventory
every 30 seconds; snapshots expire after 90 seconds. Treat observations older
than 45 seconds as stale. Require online service/room availability, matching
service sessions, and membership in fresh inventory before using state as
current. Topics are not an atomic snapshot. Account for ordinary clock skew.
Service availability uses a retained offline Last Will and does not expire.

Commands require `request_id` and `service_session` UUID strings, a safe
`client_id` (`[A-Za-z0-9_-]{1,64}`), and an HTTP command name/value. Omit `value`
for playback commands. Volume requires integer 0–100, relative volume integer
-100–100, and mute a boolean. Read the current online service session first.
Publish with Retain off and Message Expiry Interval between one and ten
seconds. Commands expire before dispatch even after waiting in the service.
A new process session rejects commands created before restart.

Results contain `service_session`, `client_id`, `request_id`, `room_id`,
`completed_at`, `status`, and `detail`; `completed` results include `state`.
`completed` means dispatch and the following state read succeeded;
`rejected` means invalid, expired, unavailable action, or overloaded;
`uncertain` means an attempted action may have taken effect. `TRANSITIONING`
is a valid completed state. Results are non-retained and expire in 30 seconds.
Subscribe to results before sending. A missing result does not prove failure.

QoS 1 can duplicate messages. Request IDs protect accepted actions while
pending and for 60 seconds after completion, within a process session. Never
reuse IDs or renew an old action's expiry. An identical duplicate returns the
saved result; conflicting reuse is rejected. This is bounded protection,
not a permanent exactly-once ledger. Do not automatically retry actions,
especially skip, toggle, or relative volume; inspect state after uncertain or
missing results. Transport acknowledgments do not confirm speaker execution.

Commands are limited to 4 KiB, 16 queued actions and 1,024 pending/protected
records. Overload rejects new work without evicting protected records.
Rendered frames stay on HTTP and continue to support ETags.
