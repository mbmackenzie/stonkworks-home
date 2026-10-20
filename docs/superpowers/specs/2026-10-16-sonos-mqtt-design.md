# Sonos Desk MQTT integration

Date: 2026-10-16

Status: Approved by the user on 2026-10-16; implemented on `feat/sonos-mqtt`.
No live deployment is included.

## Intent and success criteria

Add an optional MQTT interface to Sonos Desk so a software client can observe
every discovered room and issue controls before physical hardware exists.
Preserve the existing HTTP API, browser controls, renderer, and trusted-LAN
deployment model. Demonstrate commands using simulated speakers; live checks
observe state without changing household playback.

This is the first application slice of the MQTT milestone in the Stonkworks
Home proposal. The user selected publishing all discovered rooms and approved
the service structure, topic layout, command behavior, and operational design
in conversation. This document supplies the concrete contract for review.

Success means:

- An authenticated MQTT 5 client can discover rooms, inspect current state,
  distinguish stale or unavailable data, and correlate command results.
- Duplicate delivery cannot repeat an accepted action within the documented
  protection window. Commands from a previous service process are rejected.
- Broker failure does not take down HTTP. A failed room does not prevent
  subsequent rooms from being polled.
- Existing HTTP behavior and simulated-speaker regression checks still pass.
- Configuration supports an external broker without household-specific values
  or credentials in shared source.

## Scope and ownership

Sonos Desk owns discovery, playback, room volume/mute, and rendering. The
broker supplies transport and retained-message storage. Future Home software
will own authoritative device assignments; this slice creates no assignments
or editable device configuration.

The household independently installs Mosquitto and MQTTX Web. Its broker plan
provides authenticated TCP on a trusted LAN and a separate browser account.
Both accounts are restricted to `stonkworks/#`. That infrastructure remains
owned by the homeserver repository and is not modified by this work.

Excluded: Home, onboarding, firmware, device credentials, a portable broker
stack, registry publication, licensing changes, and live deployment. Rendered
frames remain on HTTP; frame-change notifications are deferred. The proposal's
stale repository links and publication wording are separate documentation
cleanup, not prerequisites for this feature.

## Architecture and alternatives

Use an in-process MQTT adapter sharing the existing `SonosBackend` with HTTP.
This reuses discovery, strict command semantics, and command serialization.
A separate HTTP-to-MQTT bridge would add another service and network failure
boundary. Sonos event subscriptions could improve latency but add renewal and
recovery behavior; polling is sufficient for this first slice.

Keep the following responsibilities separate within the Sonos Desk package:

- Protocol models and topic helpers: validation, serialization, topic parsing,
  and command/result envelopes. They do not contact speakers or the broker.
- MQTT transport: connection lifecycle, subscriptions, bounded outgoing work,
  Last Will, and receipt of MQTT properties. Network callbacks never perform
  speaker I/O.
- Integration worker: room polling, prioritized command execution, expiry,
  duplicate protection, and publication decisions using the shared backend.
- Application lifecycle: starts/stops the adapter and exposes cached status.

One worker schedules MQTT speaker operations. It checks pending commands
between room reads, so commands take priority over the next poll. Existing
HTTP calls still use the backend's shared lock. An in-flight speaker request
cannot be preempted; polling is not a guarantee of fixed control latency.
Preserve the single application process/worker requirement.

The default polling interval is three seconds per room, measured from that
room's last completed attempt. Due rooms are visited fairly; when work takes
longer than the interval, do not run overlapping requests or build a backlog
of missed polls. Discovery retains the backend's existing 60-second cache.
Do not initiate extra discovery on every three-second polling pass.

## MQTT contract

Use MQTT 5 over TCP, with TLS available when configured. No WebSocket support
is needed in the backend. Use QoS 1 for application publications and subscribe
to commands at QoS 1. Clients publish commands at QoS 1.

The base topic is `stonkworks/v1/sonos-desk/<instance>`. The instance defaults
to `main` and is configurable. Only one running service may own a given base
topic. Instance and application client IDs match `[A-Za-z0-9_-]{1,64}`.
The MQTT connection client ID is derived from the instance and must be unique
within the broker. Room topic segments use stable Sonos `room_id` values,
never friendly names. Reject IDs containing topic separators or wildcards.

| Suffix | Payload | Retained |
| --- | --- | --- |
| `availability` | Service status and process session | Yes |
| `rooms` | Discovered inventory | Yes |
| `rooms/<room_id>/state` | Last successfully observed room state | Yes |
| `rooms/<room_id>/availability` | Room read availability | Yes |
| `rooms/<room_id>/command` | Control request | No |
| `clients/<client_id>/results` | Correlated command result | No |

JSON payloads are UTF-8 objects. The topic's `v1` defines the contract version.
Consumers tolerate additional response fields; command objects reject unknown
fields. No caller-selectable response topic is supported.

### Service and room state

Generate a random UUID `service_session` at process startup and retain it
through broker reconnects. All service-originated payloads include it.
Observation and result times use UTC RFC 3339 timestamps. Deadlines inside the
service use a monotonic clock.

Service availability contains `status` (`online` or `offline`) and
`service_session`. Configure a retained QoS 1 offline Last Will with no Will
delay and a 15-second keepalive. After the command subscription is confirmed,
publish online availability. Publish offline availability on clean shutdown.
Online means the MQTT interface is accepting requests; room availability is
separate. Broker disconnect detection is eventual, not instantaneous.

Inventory contains `service_session`, `observed_at`, `discovery` (`ok` or
`degraded`), and `rooms`, whose entries preserve the existing `/rooms` fields.
Republish on changes, after reconnect, and at least every 30 seconds while
connected. A successful discovery returning no rooms publishes an empty list.
On a discovery exception, keep the existing inventory and report degraded
discovery; do not invent removals. A `rooms()` failure must not prevent polling
IDs from the last successful inventory. Room reads remain independent.

State contains `service_session`, `observed_at`, and `state`. MQTT polls use
the backend's fresh-read option so observation times reflect actual reads,
including when the configured interval is shorter than the HTTP cache TTL.
The nested `state` preserves the existing `/state` fields and their meanings. Publish
when those fields change and after reconnect; republish unchanged successful
observations at least every 15 seconds. A failed read does not advance the
state's observation time or overwrite its last successful value.

Room availability contains `service_session`, `observed_at`, and `status`
(`online` or `offline`). A successful state read makes the room online; a failed
read makes it offline. Publish changes and refresh at least every 15 seconds
when attempts continue. A failed room does not invalidate other rooms.

Retained inventory, state, and room availability use a 90-second MQTT Message
Expiry Interval. This bounds leftovers after an abrupt stop or removal.
Service availability does not expire, so its offline Last Will remains useful.
The service heartbeat interval and consumer freshness limits are distinct:
consumers treat an observation older than 45 seconds as stale, even if the
broker still retains it. Slow or overloaded installations may therefore report
stale data rather than falsely claim current state.

Consumers require online service availability, matching process sessions,
membership in a fresh inventory, online room availability, and fresh state
before presenting state as current. Consumers should account for ordinary
clock skew; broker expiry additionally bounds retained delivery. This is not
an atomic snapshot across topics; brief mismatches during updates are normal.

On reconnect, invalidate publication suppression and schedule an immediate
inventory refresh and room reads. Publish state only from successful reads,
not by relabeling cached old state with a new observation time. Broker failure
must not create an outgoing backlog of historical state; regenerate snapshots
after reconnection. Remove retained state and availability for rooms removed
from a successful inventory during the current process. The inventory and
expiry rules cover remnants from earlier processes.

### Command envelope

For `rooms/<room_id>/command`, accept:

```json
{
  "request_id": "36cdd095-21d8-48e0-bbe9-fd65fb715012",
  "client_id": "desk-test",
  "service_session": "bb678ecb-e986-4c29-b0dd-7c4bdd79a1ae",
  "command": "volume_relative",
  "value": 1
}
```

`request_id` and `service_session` are UUID strings. Clients generate a new
request ID for each user action, subscribe to their result topic first, and
read the current online service session before sending. The room is supplied
only by the topic. Reuse the HTTP command names and strict value validation;
preserve playback through the coordinator and volume/mute on the selected room.
The existing HTTP request format does not change.

Require an MQTT Message Expiry Interval with a remaining value of 1–10 seconds
on receipt. Clients initially set ten seconds or less. Record a local deadline
from the received remaining interval and check again immediately before
calling the backend. Also enforce that deadline inside the backend lock,
after room/coordinator resolution and immediately before command dispatch;
waiting for an HTTP operation or discovery must not allow an expired action
to execute. Expired queued work is rejected. Expiry limits starting
an action, not completion of a speaker call already underway. Broker expiry
does not cancel work after delivery, hence the service-side deadline check.

Use Clean Start and Session Expiry Interval zero on every broker connection;
there is no offline command subscription. Subscribe with Retain Handling 2
and Retain As Published enabled: suppress stored retained messages and detect
the retain bit on live publications. Reject live retained commands.

Set a 4 KiB command payload limit, 16 queued commands, and 1,024 duplicate
records including pending work. Validate topic, payload, session, expiry, and
capacity before accepting work. Invalid requests do not call the backend.
If identifiers are safe to route, publish a rejection; otherwise log a bounded
diagnostic without echoing the payload or publishing to a guessed topic.

### Duplicate handling and results

Key duplicate records by `(client_id, request_id)` within the process session.
Reserve a record before queuing the action. Store a fingerprint including the
room and validated command/value. Conflicting reuse is rejected. Identical
duplicates pending execution do not queue more work; they receive the eventual
result. Identical completed duplicates republish the saved result.

Keep pending records until completion and terminal records for 60 seconds
after completion. Do not evict protected records to admit new requests; reject
overload instead. This is bounded duplicate protection, not a permanent
exactly-once ledger. Clients must never reuse action IDs or republish an old
action with renewed expiry. After process restart, the new service session
rejects requests created against the lost duplicate cache.

Results contain `service_session`, `client_id`, `request_id`, `room_id`,
`completed_at`, `status`, and `detail`. The statuses are:

- `completed`: the backend command and its following state read succeeded.
  Include `state` with the backend's returned state; `TRANSITIONING` is valid.
- `rejected`: validation, expiry, session, capacity, missing room, or a known
  unsupported-action failure. No successful execution is being claimed.
- `uncertain`: a speaker/backend failure where an attempted action may have
  taken effect, including a failed post-command read. Conservatively classify
  backend 503 responses here rather than retry them.

Results are QoS 1, non-retained, with a 30-second Message Expiry Interval.
They may be duplicated or lost across disconnects. A missing result is not
evidence that nothing happened. Do not enqueue results for later reconnect
delivery; within the duplicate window a client may repeat the identical
request, using the original action deadline, to obtain a cached result.
Check a cached duplicate before rejecting its elapsed execution deadline so
retrieving a completed result cannot repeat the action. A new or still-queued
expired action never executes. MQTT expiry can itself prevent delivery of a
late duplicate, so clients cannot rely on this as a general result-query API.

Neither the service nor its test client automatically retries speaker actions.
After an uncertain result or missing result, observe state and let the user
decide the next action. Do not turn a skip, toggle, or relative-volume request
into a fresh request merely to obtain a response. Broker acknowledgments are
transport acknowledgments, not speaker execution results.

After a command attempt, prioritize refreshing affected state. Playback can
affect grouped rooms; all discovered rooms remain due for their next poll.
A failed publication never triggers another speaker action.

## Configuration and lifecycle

MQTT is disabled when no host is configured. Invalid supplied configuration
fails startup clearly; a valid configuration with an unreachable broker does
not block HTTP startup. Expose these settings:

| Setting | Default / behavior |
| --- | --- |
| `SONOS_MQTT_HOST` | Unset disables MQTT |
| `SONOS_MQTT_PORT` | 1883, or 8883 when TLS is enabled |
| `SONOS_MQTT_USERNAME` | Required when enabled |
| `SONOS_MQTT_PASSWORD_FILE` | Required readable UTF-8 secret file |
| `SONOS_MQTT_INSTANCE` | `main` |
| `SONOS_MQTT_POLL_INTERVAL` | 3 seconds; finite positive number |
| `SONOS_MQTT_TLS` | `false`; strict boolean configuration |
| `SONOS_MQTT_CA_FILE` | Optional custom CA; otherwise system trust |

Strip only one trailing line ending from the password file. Never log its
contents. TLS verifies certificate trust and hostname; no insecure bypass.
The initial household TCP listener supplies no transport encryption, consistent
with its separate trusted-LAN broker design. TLS configuration supports other
installations; mutual TLS provisioning is outside this slice.

Reconnect with exponential backoff from one to 30 seconds and jitter. Limit
outgoing queued publications to 128 and in-flight publications to 20. Queue
pressure must not block network callbacks or speaker execution indefinitely.
Coalesce replaceable snapshots; regenerate them on reconnect. Failed results
remain only in the bounded duplicate records. Record publication failures in
cached status and logs, without leaking credentials or complete commands.

Start the adapter through the FastAPI lifespan. On shutdown, stop accepting
commands, reject queued work where possible, allow an active backend call to
finish within the application's shutdown grace, and publish offline before
disconnecting. Do not forcibly interrupt a speaker write or claim an unobserved
outcome. Termination during a write leaves its outcome uncertain to the client.
Close the MQTT adapter before the existing renderer cleanup.

`GET /health` remains unchanged. Add `GET /mqtt/status`, with `Cache-Control:
no-store`, returning cached `enabled`, `connected`, `service_session` (null when
disabled), `last_connected_at`, `last_poll_completed_at`, `rooms_known`,
`rooms_unavailable`, and `last_error` (null when absent). Times are UTC strings
or null. `last_error` is a sanitized diagnostic. The endpoint performs no
network I/O. Individual room failures and inventory failures are reflected in
status without redefining HTTP liveness.

## Implementation boundaries

Add actual protocol documentation and machine-readable message schemas under
`protocols/v1/` when implementing this contract. Keep behavior-specific adapter
code in Sonos Desk; do not create a generic integration framework or empty
Home/firmware directories. Add a maintained MQTT 5 client dependency through
the uv workspace and lockfile; exact package version selection belongs to the
implementation plan and must be verified against current official docs.

Share HTTP command validation without changing its accepted/rejected inputs.
Avoid unrelated backend refactoring. Add an optional internal command deadline
for MQTT, leaving HTTP behavior unchanged when no deadline is supplied.
If polling needs a small inventory
snapshot helper to preserve independently readable rooms after inventory
errors, keep it narrowly scoped and preserve existing HTTP responses.

Update Sonos Desk documentation, README status, and portable Compose inputs
to describe optional MQTT and a read-only mounted password secret. Supply
documentation addresses only. No persistent Sonos Desk volume is introduced.

## Verification

Use simulated speakers, a controllable clock, and a fake transport for focused
checks of topic parsing, strict inputs, session matching, deadline handling,
deduplication before and after completion, conflicting IDs, protected-cache
capacity, queue overload, uncertain writes, room failure isolation, polling
fairness, reconnect snapshots, state freshness, and lifecycle cleanup.

Exercise the actual adapter against a disposable authenticated Mosquitto
broker and simulated backend. Verify QoS 1 command/result flow, MQTT expiry,
retained-command rejection including live retained messages, retained state,
Last Will after abrupt connection loss, authentication failure, broker restart
recovery, and continued HTTP operation during outages. The integration harness
must use its own broker port, credentials, topics, and temporary persistence.
It must not connect to or restart the household broker by default.

Run the existing pytest suite, Ruff lint and format checks, and ty. Validate
the portable Compose example with documentation settings. Document MQTTX
setup, MQTT 5 properties needed to send commands, and a small software-client
example/test tool that does not automatically retry actions. Live verification
is opt-in and read-only; no household playback or volume changes are needed.

## Design review checklist

- No Home, firmware, broker deployment, or publication work is implied.
- Existing HTTP contracts, room/coordinator semantics, bounded rendering
  caches, and single-worker deployment remain intact.
- Retained snapshots are not interpreted as automatically current.
- Every accepted command is bounded by a session, deadline, queue, and
  duplicate record; no database-backed exactly-once guarantee is claimed.
- MQTT outages, slow rooms, and result loss have explicit behavior.
- Household credentials and settings remain outside shared source.

## References

- [MQTT 5.0 specification](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html):
  QoS 1 duplicates, Message Expiry Interval, Session Expiry Interval,
  subscription retain options, and Last Will behavior.
- [Existing Sonos Desk HTTP contract](../../sonos-desk.md).
- The separately managed household broker design and plan were reviewed as
  context; their absolute host paths are not portable build dependencies.
