# Sonos Desk MQTT Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an authenticated software client observe all discovered Sonos rooms and issue safely correlated MQTT commands while preserving HTTP operation.

**Architecture:** An optional in-process MQTT adapter shares `SonosBackend` with HTTP. A single integration worker prioritizes commands between room polls; a separate transport thread owns MQTT network I/O. Pure protocol models, bounded command records, and deterministic polling are independently testable.

**Tech Stack:** Python 3.13+, existing FastAPI/Pydantic/SoCo, Paho MQTT 2.1.0 with MQTT 5 and callback API v2, pytest, disposable Mosquitto 2.0.22.

**Spec:** [Approved Sonos MQTT design](../specs/2026-10-16-sonos-mqtt-design.md).

## Global Constraints

- MQTT is disabled when no host is configured.
- Preserve the single application process/worker requirement.
- Instance and application client IDs match `[A-Za-z0-9_-]{1,64}`.
- Use stable Sonos room IDs in `stonkworks/v1/sonos-desk/<instance>`; instance defaults to `main`.
- Use MQTT 5, QoS 1, Clean Start on every connection, Session Expiry Interval zero, keepalive 15 seconds, and no Will delay.
- Commands/results are non-retained; service availability, inventory, room state, and room availability are retained.
- Commands require 1–10 seconds remaining MQTT expiry; check deadlines under the backend lock immediately before mutation.
- Command payload limit 4 KiB; queue 16; duplicate records 1,024 including pending; terminal records protected for 60 seconds; result expiry 30 seconds.
- Default polling interval three seconds; state/room availability refresh 15 seconds; inventory refresh 30 seconds; retained snapshot expiry 90 seconds; consumer freshness 45 seconds.
- Reconnect backoff one to 30 seconds with jitter; outgoing queued publications limited to 128, in-flight publications to 20.
- No persistent Sonos Desk database/volume, Home, firmware, broker deployment, registry publication, or household deployment changes.
- Credentials and actual household connection values remain outside Git. Live checks are opt-in and read-only.

## Review Focus

- A topic containing a friendly name must not exploit HTTP's room-name fallback to control a speaker; test in Task 3.
- A relative-volume read or play/pause transport read can consume the remaining deadline; check immediately before the subsequent write, tested in Task 2.
- A broken group topology can make `/rooms` fail while individual speakers remain readable; preserve independent polling, tested in Task 4.
- A delayed callback from an abandoned connection must not make a replacement connection online or replay its results; test in Task 5.
- TLS/authentication errors and credential-file failures must produce useful status without secret-bearing exception text; test in Tasks 5–6.

## Files and interfaces

All package paths below are relative to `services/sonos-desk/src/sonos_desk/`.
All test paths below are relative to `services/sonos-desk/tests/`.

| File | Responsibility |
| --- | --- |
| `commands.py` | Shared command/value validation, extracted from `routes.py` |
| `mqtt/__init__.py` | Package marker only; keep public entry points explicit |
| `mqtt/protocol.py` | Topics, MQTT envelopes, publication/receipt value types |
| `mqtt/config.py` | Validated environment inputs and secret loading |
| `mqtt/commands.py` | Bounded command queue and duplicate/result records |
| `mqtt/polling.py` | Inventory, room observations, freshness publication |
| `mqtt/transport.py` | Paho connection supervisor, subscription, outgoing limits |
| `mqtt/runtime.py` | Worker scheduling, adapter lifecycle, cached status |
| `sonos.py` | Narrow deadline and stable inventory-ID helpers |
| `app.py`, `routes.py` | Optional adapter lifecycle and cached status endpoint |
| `protocols/v1/sonos-desk.md` | Portable MQTT contract and topic/payload examples |
| `protocols/v1/sonos-desk.schema.json` | JSON Schema 2020-12 `$defs` for the six message families |
| `services/sonos-desk/examples/mqtt_client.py` | Explicit single-action software client |
| `deploy/stacks/sonos-desk/compose.mqtt.yaml` | Optional MQTT environment and read-only secret overlay |

Use standard `dict[str, object]` JSON payloads at the backend boundary; avoid
rewriting the existing backend's types. Use small typed dataclasses for the
new adapter's internal boundaries. All clocks are injectable: monotonic seconds
for scheduling/deadlines and timezone-aware UTC datetimes for public timestamps.

## Task 1: Establish the versioned protocol and shared command validation

**Files:** Create `commands.py`, `mqtt/__init__.py`, `mqtt/protocol.py`, both `protocols/v1/sonos-desk.*` files, and `test_mqtt_protocol.py`. Modify `routes.py:12` (the existing `Command` model).

**Interfaces:**
- `CommandFields(BaseModel)`: existing `command`/`value` fields and validator, strict inputs, extra fields forbidden. HTTP `Command(CommandFields)` keeps `room` and the same validation behavior.
- `CommandRequest(CommandFields)`: `request_id: UUID`, `client_id: str`, `service_session: UUID`; reject unknown fields and non-string UUID inputs.
- `Topics(instance: str)`: `availability`/`inventory` properties; `room_state(room_id)`, `room_availability(room_id)`, `room_command(room_id)`, `results(client_id)` return strings; `command_room(topic: str) -> str` validates exact topic structure.
- `IncomingCommand(topic: str, payload: bytes, retained: bool, remaining_expiry: int | None, received_at: float)` frozen dataclass.
- `Publication(topic: str, payload: bytes, retained: bool, expiry: int | None)` frozen dataclass; `encode_payload(payload: dict[str, object]) -> bytes` emits strict JSON without NaN/Infinity.

- [ ] **1. Write protocol tests before extraction.** Add `test_http_validation_survives_extraction`, parametrizing the existing invalid HTTP cases; `test_topic_ids_are_single_safe_segments`, asserting rejection of empty IDs, `/`, `+`, `#`, and invalid instance/client IDs; `test_mqtt_command_envelope_is_strict`, asserting playback `value:null`, numeric UUIDs, unknown keys, string integers, and bool volume are rejected. Assert default base is `stonkworks/v1/sonos-desk/main`.
- [ ] **2. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_mqtt_protocol.py services/sonos-desk/tests/test_api.py`. Confirm new tests fail because the new interfaces do not exist; record the red result.
- [ ] **3. Implement the interfaces and schemas.** Extract validation without changing HTTP's field-set semantics. Define schemas for availability, inventory, state, room availability, command, and result, preserving the spec's exact fields. Keep request IDs canonical UUID strings. Document expiry, duplicate-window limits, session matching, non-atomic snapshots, and freshness requirements. Strictly reject non-standard JSON constants before model validation.
- [ ] **4. Verify the same command passes.** Add fixture-based validation of representative payloads against the schema using a dev-only `jsonschema` dependency; reject mismatched command/value combinations and extra command fields. Update root `pyproject.toml`/`uv.lock` only for that test dependency. Run `uv run ruff check services/sonos-desk` and `uv run ty check`.
- [ ] **5. Commit only these task files**, including workspace metadata if changed, with `feat: define Sonos MQTT v1 protocol`.

## Task 2: Enforce deadlines at the actual speaker mutation boundary

**Files:** Modify `sonos.py:20` (`BackendError` vicinity) and `sonos.py:142` (`command`). Extend `test_sonos.py`.

**Interfaces:**
- `CommandExpired(Exception)` is an internal error, not a change to HTTP error handling.
- `SonosBackend.command(self, room, command, value=None, *, deadline: float | None = None)` uses the backend's injected monotonic clock. Existing callers omit the keyword.
- `SonosBackend.room_ids(self) -> list[str]` refreshes through the existing discovery TTL and returns stable IDs under the lock without accessing group coordinators.

- [ ] **1. Write `test_expired_command_never_mutates`, `test_deadline_rechecked_after_lock_and_resolution`, and `test_deadline_rechecked_after_toggle_or_volume_read`.** Advance an injected clock while blocked on the lock, during resolution, and from the mocked volume/transport getter. Assert `CommandExpired` and no setter/play/pause/skip calls. For the concurrent case use events with bounded waits, not sleeps. Add `test_room_ids_survive_broken_group_lookup` and assert discovery still observes the existing 60-second TTL.
- [ ] **2. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_sonos.py`. Confirm new tests fail for missing interfaces/guards.
- [ ] **3. Implement the optional guard and ID helper.** Resolve coordinator and any action prerequisite reads, then check the deadline immediately before the mutating call. For relative volume calculate the value before checking; for toggle read transport before checking. Never retry an action. Preserve command cache invalidation and post-command state semantics, including HTTP's existing 503 behavior.
- [ ] **4. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_sonos.py services/sonos-desk/tests/test_api.py`; expect all pass, including coordinator, relative-volume clamp, uncertain write, and renderer-facing API cases.
- [ ] **5. Commit** the backend and tests with `feat: guard MQTT commands at speaker dispatch`.

## Task 3: Implement bounded command acceptance and correlated results

**Files:** Create `mqtt/commands.py`, `test_mqtt_commands.py`. Consume Tasks 1–2.

**Interfaces:**
- `CommandProcessor(backend: SonosBackend, topics: Topics, service_session: str, publish: Callable[[Publication], bool], clock: Callable[[], float], utcnow: Callable[[], datetime], on_command: Callable[[str], None])`.
- `set_accepting(accepting: bool) -> None`; `accept(message: IncomingCommand) -> None` validates/reserves/queues without speaker I/O; `run_one() -> bool` executes at most one queued request; `reject_pending() -> None` drains with rejections during shutdown. `on_command(room_id)` schedules refresh after an attempted action.
- `set_room_ids(room_ids: frozenset[str]) -> None` supplies the authoritative currently discovered ID set from the poller. No fallback to friendly names is permitted for commands.

- [ ] **1. Write `test_duplicate_next_executes_once`, `test_pending_duplicate_reserves_one_slot`, `test_conflicting_request_id_is_rejected`, and `test_restart_session_rejects_old_request`.** Assert one speaker mutation, identical cached terminal results, and no re-execution after publication failure. Assert the cache survives transport reconnect within the same process.
- [ ] **2. Write rejection tests.** Cover missing/zero/>10 expiry, live retain, >4 KiB, malformed UTF-8/JSON, invalid envelope, wrong session, friendly-name topics, and unknown stable IDs. Invalid/unroutable identifiers produce no publication. Assert callback-time `accept` never calls `rooms`, `state`, or `command`.
- [ ] **3. Write bounds/timing/outcome tests.** Assert the 17th queued action is rejected, 1,024 protected records cannot be evicted, pending records never expire prematurely, terminal records release capacity at 60 seconds, and queued expiry prevents dispatch. Assert backend 404/409 yields `rejected`, 503 yields `uncertain`, success yields `completed` plus state, and result expiry is 30 seconds/non-retained. Assert a cached duplicate can return its result without speaker access even when its original local deadline has passed. A new expired request cannot execute.
- [ ] **4. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_mqtt_commands.py`; confirm the new tests fail before implementation.
- [ ] **5. Implement the processor.** Use a lock around acceptance/records and a bounded queue; release it during speaker I/O. Reserve before enqueueing. Validate transport/session safety before duplicate lookup; check cached duplicates before new-work deadline/capacity checks. Store fingerprint, original deadline, pending status, and terminal result. Re-delivery must never extend the original deadline. Map `CommandExpired` to rejection. All result publication is bounded/nonblocking and failure cannot repeat an action. Keep retained-command rejections from being mistaken for valid duplicates.
- [ ] **6. Run** the protocol, command, backend, and API tests; expect all pass. Commit with `feat: process MQTT commands with bounded duplicate protection`.

## Task 4: Publish independent room observations and reconnect snapshots

**Files:** Create `mqtt/polling.py`, `test_mqtt_polling.py`. Consume Task 1 and `room_ids()` from Task 2.

**Interfaces:**
- `StatePoller(backend: SonosBackend, topics: Topics, service_session: str, publish: Callable[[Publication], bool], clock: Callable[[], float], utcnow: Callable[[], datetime], poll_interval: float, on_inventory: Callable[[frozenset[str]], None])`.
- `poll_one() -> bool`: performs one scheduled inventory operation or one room read; returns whether work ran. `force_refresh(room_id: str | None = None) -> None` changes scheduling without I/O. `snapshot() -> dict[str, object]` returns cached polling status.
- `on_inventory` supplies stable discovered IDs even when group inventory metadata cannot be assembled. The published inventory itself remains the last successful `/rooms` result on that failure.

- [ ] **1. Write deterministic-clock tests.** `test_all_rooms_publish_by_id` asserts fresh backend reads, preserved HTTP state fields, matching session, UTC observation time, QoS-independent publications retained with 90-second expiry. `test_unchanged_state_refreshes_after_15_seconds` and `test_inventory_refreshes_after_30_seconds` assert suppression before and renewal at those intervals; room reads use the three-second default per completed attempt.
- [ ] **2. Write failure/recovery tests.** Assert one broken room publishes offline without suppressing a healthy room; unsuccessful reads never advance state timestamps; failed discovery preserves inventory with degraded status; successful empty discovery publishes `rooms:[]`; failed group metadata still permits ID-based room polls. Removed rooms emit zero-byte retained tombstones. After reconnect, require newly observed state, not old data with a replaced timestamp.
- [ ] **3. Write scheduler/load tests.** Assert each due room gets a turn, a slow attempt does not accumulate missed polls, and there is at most one observation for a room at a time. Simulate publish rejection and verify a later pass retries the snapshot without unbounded storage. Confirm backend discovery is not forced every poll and command-triggered `force_refresh` schedules the affected room first.
- [ ] **4. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_mqtt_polling.py`; confirm failures before implementation.
- [ ] **5. Implement the poller.** Keep state proportional to current inventory; compare state content excluding timestamps; update publication suppression only after transport acceptance. Refresh inventory before room reads on initial/reconnect passes; preserve cached inventory on metadata failures while obtaining independent IDs. `force_refresh(None)` resets reconnect suppression and due times. Emit cached status only; neither `snapshot()` nor `on_inventory` performs network I/O.
- [ ] **6. Run** polling, backend, protocol, and command tests; expect all pass. Commit with `feat: publish MQTT room inventory and fresh state`.

## Task 5: Add configured MQTT 5 transport with bounded reconnect behavior

**Files:** Create `mqtt/config.py`, `mqtt/transport.py`, `test_mqtt_config.py`, `test_mqtt_transport.py`. Modify service `pyproject.toml` and root `uv.lock`.

**Interfaces:**
- `MqttConfig` frozen dataclass: `host`, `port`, `username`, secret `password` excluded from repr, `instance`, `poll_interval`, `tls`, `ca_file`.
- `load_mqtt_config(environ: Mapping[str, str], read_secret: Callable[[Path], str]) -> MqttConfig | None`; validates all spec settings before starting threads.
- `MqttTransport(config: MqttConfig, topics: Topics, service_session: str, on_message: Callable[[IncomingCommand], None], on_ready: Callable[[], None], on_disconnect: Callable[[], None])`.
- `start() -> None` is nonblocking; `publish(publication: Publication) -> bool` is bounded/nonblocking; `close() -> None` requests offline and bounded shutdown; `snapshot() -> dict[str, object]` returns cached transport status.

- [ ] **1. Write configuration tests.** Assert no host disables MQTT; explicit bad port, empty username/password, unreadable secret, nonfinite/nonpositive interval, invalid instance, unknown boolean, and CA without TLS fail clearly. Assert ports 1883/8883, system/custom CA verification, exactly one trailing line ending removed, and password absent from repr/errors. Supply credential-reader exceptions containing a sentinel secret and assert diagnostics do not echo it.
- [ ] **2. Write transport tests with a fake Paho client/factory.** Assert MQTTv5 and callback v2, `clean_start=True`, `SessionExpiryInterval=0`, keepalive 15, retained offline QoS 1 Will without expiry, and subscription options QoS 1/Retain Handling 2/Retain As Published. `on_ready` must wait for successful SUBACK; denied subscriptions keep the integration unavailable. Validate expiry/retain propagation into `IncomingCommand` and no callback speaker I/O.
- [ ] **3. Write reconnect/bounds tests.** Assert one-to-30-second jittered backoff with injected randomness/clock, interruptible waits, outgoing budget 128 and in-flight ceiling 20, and no blocking wait for PUBACK in callbacks. A full queue rejects results and coalesces replaceable snapshots. A reconnect discards all previous outgoing work; callbacks from an old connection generation cannot change readiness or deliver messages to the new one. Test failed authentication and certificate errors with sanitized status.
- [ ] **4. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_mqtt_config.py services/sonos-desk/tests/test_mqtt_transport.py`; confirm new tests fail. Then add the verified dependency with `uv add --package sonos-desk 'paho-mqtt>=2.1.0,<3'` and retain the resolved version in `uv.lock`.
- [ ] **5. Implement transport ownership.** Use one dedicated supervisor thread with Paho's manual `loop()` and application-owned reconnect/backoff. Disable Paho automatic reconnect; create a new client for every connection attempt, discard its predecessor and outgoing queue, and tag callbacks by generation. This avoids Paho resending stale outgoing messages. Apply global pending-publication accounting across application and library queues rather than accidentally allowing two independent 128-message backlogs. Network callbacks perform only bounded validation, status changes, and enqueueing. Never log raw Paho/auth exceptions or payloads.
- [ ] **6. Run** config/transport tests, `uv run ruff check services/sonos-desk`, and `uv run ty check`; expect all pass. Commit with `feat: connect Sonos Desk to authenticated MQTT 5 brokers`.

## Task 6: Wire lifecycle, status, and optional portable configuration

**Files:** Create `mqtt/runtime.py`, `test_mqtt_runtime.py`, and the Compose overlay. Modify `app.py:15`, `routes.py:40`, `.env.example`, `README.md`, and `docs/sonos-desk.md`.

**Interfaces:**
- `MqttAdapter(backend: SonosBackend, config: MqttConfig)` owns one UUID session, transport, command processor, poller, and integration worker.
- `start() -> None`, `close() -> None`, `status() -> dict[str, object]` implement lifecycle/status. Inject clocks/transport factory into the constructor as keyword-only test seams without adding public HTTP inputs.
- `create_app(backend=None, renderer=None, *, mqtt_config=None)` preserves positional callers. `None` disables MQTT for directly constructed test apps; production `app = create_app(mqtt_config=load_mqtt_config(os.environ, ...))` loads environment configuration explicitly.
- `create_router(backend, renderer, mqtt_status: Callable[[], dict[str, object]])` serves `/mqtt/status` from a callback. Disabled status uses `enabled:false`, `connected:false`, null session/times/error, and zero room counters.

- [ ] **1. Write runtime/API tests.** Assert environment-free injected apps never connect; disabled status and enabled cached status have exactly the documented fields and `Cache-Control:no-store`. Monkeypatch speaker/broker methods to fail if either status endpoint calls them. An unreachable broker does not delay HTTP startup or break `/rooms`, `/state`, or `/frame`. A pending command is chosen before the next room poll.
- [ ] **2. Write lifecycle tests.** Assert readiness opens command acceptance only after SUBACK, reconnect forces fresh inventory/state, one UUID survives transport reconnect, and stop rejects pending work before offline/disconnect. In-flight writes are not retried or forcibly interrupted. Renderer closes after adapter teardown, including startup failures. Bound thread joins and ensure no surviving worker accesses a closed renderer. Verify a falsey/malformed credential configuration fails without logging secrets.
- [ ] **3. Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_mqtt_runtime.py services/sonos-desk/tests/test_api.py`; confirm new tests fail before wiring.
- [ ] **4. Implement runtime and HTTP wiring.** Share one backend and the injected monotonic clock across deadline checks. Compose readiness/disconnect callbacks with `set_accepting`; preserve duplicate records across reconnect. The worker tries one command, otherwise one poll operation, then uses an interruptible wait capped at 100 ms when idle. Snapshot/status access uses short locks, no speaker I/O. Shutdown has a finite adapter grace of ten seconds; if an active call outlasts it, report the incomplete shutdown and allow process termination without another action attempt.
- [ ] **5. Document/configure optional Compose use.** Keep base Compose MQTT-disabled with no mandatory secret mount. The overlay sets `SONOS_MQTT_*` inputs and mounts `${SONOS_MQTT_PASSWORD_PATH:?Set a local secret file}` read-only at `/run/secrets/sonos-mqtt-password`; set `SONOS_MQTT_PASSWORD_FILE` to that container path. Use a bind mount with `create_host_path:false`, so missing files fail instead of becoming directories. Document how to mount an optional CA read-only. Preserve host networking, explicit Sonos LAN interface, one worker, and existing health check.
- [ ] **6. Verify** all service tests, then `SONOS_INTERFACE=192.0.2.20 docker compose -f deploy/stacks/sonos-desk/compose.yaml config --quiet`. Validate the overlay using documentation host `mqtt.example.test`, user `example-app`, and a temporary non-secret password file under `/tmp`; include both `-f` paths. No `up` against the household stack. Commit with `feat: enable optional MQTT lifecycle and status`.

## Task 7: Prove the complete flow against a disposable broker

**Files:** Create `test_mqtt_integration.py`, `conftest.py` for broker fixtures, and `services/sonos-desk/examples/mqtt_client.py`. Modify root `pyproject.toml` for an `mqtt_integration` pytest marker; extend `protocols/v1/sonos-desk.md` and `docs/sonos-desk.md` with usage and verification.

**Interfaces:**
- Session-scoped integration fixture `mqtt_broker` starts its own pinned `eclipse-mosquitto:2.0.22` container with an ephemeral loopback host port, random container name, temporary password/ACL/config/persistence, and no household mounts. Explicit integration opt-in is `SONOS_MQTT_INTEGRATION=1`.
- `mqtt_broker.stop()`, `.start()`, and `.endpoint` support recovery tests; teardown removes only its own container and temporary state.
- Test helper `wait_for_message(topic, predicate, timeout=10)` uses an event/queue and a monotonic deadline; timeouts show sanitized diagnostics.
- Example CLI `main(argv: list[str] | None = None) -> int` uses the same configurable broker inputs, required `--room-id`/`--command`, typed `--value`, and optional `--instance`/`--client-id`. It requires an explicit action; there is no default playback mutation and no automatic retry.

- [ ] **1. Write end-to-end tests before the harness.** `test_publish_state_and_execute_one_command` uses the existing simulated `Speaker`, actual adapter, and separate Paho client; assert retained inventory/state, one `next` mutation despite duplicate publication, and a correlated completed result. Test retained replay suppression and live retained rejection separately. Check results are not retained for a new subscriber.
- [ ] **2. Write recovery/failure integration tests.** Verify actual broker expiry prevents an offline queued command, adapter expiry prevents a delivered waiting command, expired retained snapshots disappear, and ungraceful connection loss triggers offline Last Will. Restart only the disposable broker and require fresh snapshots without old result replay. Check rejected authentication, no anonymous access, and HTTP liveness/read access during the outage. Simulate an uncertain skip and assert one mutation with `uncertain`, then replace the adapter process session and reject the old request.
- [ ] **3. Run** `SONOS_MQTT_INTEGRATION=1 uv run --package sonos-desk pytest -q -m mqtt_integration services/sonos-desk/tests/test_mqtt_integration.py`; confirm new tests fail before harness completion. Without opt-in, these tests skip with an explicit reason; with opt-in, unavailable Docker/image startup is a failure, not a silent skip. No public or household broker fallback is allowed.
- [ ] **4. Implement the harness and example client.** Obtain Docker access/image downloads through normal approval if required. Generate disposable credentials without command-line password arguments or printed secrets. The example subscribes to results and service availability, obtains the current session, sends one QoS 1 non-retained command with expiry ten seconds, waits up to 15 seconds, and exits with a distinct failure for `rejected`, `uncertain`, or missing result. Test CLI value conversion and verify it never publishes twice after timeout/disconnect. Do not run the example against real speakers during verification.
- [ ] **5. Document MQTTX and software-client usage.** Show MQTT 5 selection, a result subscription before send, current service-session lookup, Message Expiry Interval ten, Retain off, and stable room IDs. Use documentation broker names and placeholders. Explain the 45-second freshness rule, uncertain outcomes, no-retry policy, 60-second duplicate limit, instance uniqueness, plain-TCP trust boundary, and secret-file setup. Keep household guide/credential JSON outside portable instructions.
- [ ] **6. Run final verification from the repository root:**

  ```sh
  uv sync --all-packages --locked
  uv run --package sonos-desk pytest -q
  SONOS_MQTT_INTEGRATION=1 uv run --package sonos-desk pytest -q -m mqtt_integration services/sonos-desk/tests/test_mqtt_integration.py
  uv run ruff check services/sonos-desk
  uv run ruff format --check services/sonos-desk
  uv run ty check
  uv run prek run --all-files
  ```

  Expect zero failures; integration tests skip only in the non-opt-in suite.
  Repeat Compose validation from Task 6 after final documentation/config edits.
  Inspect `git diff --check` and the final scope diff. Do not repeat passing
  checks without a new change or unresolved concern. Commit with
  `test: verify Sonos MQTT flow and broker recovery`.

## Plan review and execution handoff

The design is approved; this implementation plan still needs user review and
an execution-method selection before implementation. Recommend native/inline
execution because the seven tasks share closely coupled protocol, scheduling,
and transport interfaces. Subagent-driven execution remains an option if the
user prefers independent task-by-task review. Do not start either method or
create implementation scaffolding while plan review is pending.

At execution time, apply the worktree skill, preserve the existing unrelated
`.gitignore` modification, and use task-scoped commits. Use the final review
workflow required by the selected execution skill. Report actual checks and
any remaining limitations; source completion does not authorize deployment.

## Self-review coverage map

| Spec area | Tasks |
| --- | --- |
| Purpose, boundaries, unchanged HTTP | 1, 2, 6, 7 |
| Topics, schemas, strict commands | 1, 3 |
| Mutation safety, expiry, duplicate bounds, outcomes | 2, 3, 7 |
| All rooms, discovery failures, freshness, removals | 2, 4, 7 |
| MQTT 5, authentication, TLS, Last Will, backoff, queue bounds | 5, 7 |
| Lifespan, status, shutdown, reconnect, optional deployment inputs | 5, 6 |
| Portable documentation, software client, regression/integration evidence | 1, 6, 7 |

## Library references checked during planning

- [Paho package release](https://pypi.org/project/paho-mqtt/): 2.1.0 is the current published version checked on 2026-10-16; execution must verify the resolved lockfile on Python 3.13+.
- [Paho Client API](https://eclipse.dev/paho/files/paho.mqtt.python/html/client.html): MQTT 5 properties, subscription options, callback v2, publication limits, manual network loop, and outgoing-message retention across reconnects. Replacing a client avoids replaying its old outgoing queue.
- [MQTT 5 specification](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html): retain flags, session expiry, message expiry, and transport acknowledgment semantics.
