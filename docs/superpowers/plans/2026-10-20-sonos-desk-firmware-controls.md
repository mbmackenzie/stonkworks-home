# Sonos Desk Rust Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (recommended here) or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a lean USB-powered ESP32-C6 Sonos controller, beginning with USB input logs and then MQTT 5 over verified TLS.

**Architecture:** One Rust package with a hardware-independent library and an ESP-IDF binary. Small input/controller modules produce actions; the device adapter handles GPIO, Wi-Fi, time, and MQTT. Display rendering comes later.

**Tech Stack:** Rust, ESP-IDF, `esp-idf-svc`, Serde JSON, UUID request IDs; native USB serial/JTAG console. Start with `esp-idf-svc = "=0.53.0"` and ESP-IDF `v5.5.1`, verifying the combination with a target build before adding features. The published svc manifest uses HAL 0.47; prefer its HAL/sys reexports rather than duplicate bindings. Pin a compatible dated nightly and dependency lockfile after the first successful target build.

**Spec:** [Approved controls design](../specs/2026-10-20-sonos-desk-firmware-controls-design.md), amended for the user's subsequent request for lean tests.

## Global Constraints

- No backend contract changes, display implementation, onboarding, or live deployment.
- Preserve supplied GPIO assignments; validate GPIO4/5 strapping and peripheral pulls before flashing. Resolve a demonstrated conflict with the user.
- Three once-per-press buttons; encoder detents change volume by one, without acceleration; press toggles confirmed mute.
- Batch relative volume for at most 100 ms; clamp each batch to -100 through 100; four outstanding commands maximum.
- MQTT 5, QoS 1, retain off, five-second PUBLISH expiry, fresh request IDs, 15-second result timeout.
- Readiness requires acknowledged subscriptions and matching fresh service/inventory/room observations; 45-second freshness, five-second future-skew allowance.
- Discard offline inputs; no command replay on reconnect and no automatic application retry after missing or uncertain results.
- Verified TLS only, default port 8883; credentials and actual room assignments excluded from Git and logs.
- Keep tests lean: roughly six table-driven tests in one file, targeting risky behavior. No automated broker/container/certificate harness, exhaustive field matrix, or mock framework.

## Review Focus

- Held buttons across boot/reconnect must require release before firing (Task 1 traces).
- Partial encoder motion and contact bounce must not count as whole detents (Task 1 traces).
- A stale retained online snapshot must not enable control (Task 2 event cases).
- A late result must not overwrite newer confirmed state or clear a newer mute guard (Task 2 event cases).
- An unacknowledged QoS 1 command must not reappear after reconnect (Task 3 adapter inspection and manual wire check).

## File map

Create only files needed by the running slice under `firmware/sonos-desk/`:

- `Cargo.toml`, `Cargo.lock`: one package; device dependencies target-gated so host tests stay independent of ESP-IDF.
- `src/lib.rs`, `src/inputs.rs`, `src/controller.rs`: portable input decoding and controller state machine.
- `src/protocol.rs`: topic names, typed incoming envelopes, command serialization.
- `src/main.rs`, `src/device.rs`, `src/mqtt.rs`: entry point, board/network setup, narrow MQTT adapter.
- `tests/controls.rs`: compact input traces and controller scenarios.
- `build.rs`, `.cargo/config.toml`, `rust-toolchain.toml`, `sdkconfig.defaults`: ESP-IDF build and native USB console setup.
- `config.example.toml`, `README.md`: local configuration template and build/acceptance instructions.

Modify root `.gitignore` and `README.md` to ignore generated/secrets files and link implemented firmware; update `AGENTS.md` to describe firmware once implemented. Avoid a root Cargo workspace or shared framework for a single package.

## Task 1: Input decoding and USB bring-up

Progress: the user requested small learning chunks during execution. Chunk 1
implements portable input decoding with two passing host tests and a README
walkthrough. GPIO/USB bring-up and target builds remain the next chunk; Task 1
as a whole is not yet complete. Pause between learning chunks as requested.

**Interfaces:** `Inputs::new(levels: Levels, now_ms: u64) -> Inputs`; `Inputs::sample(&mut self, levels: Levels, now_ms: u64) -> Vec<InputEvent>`; `Inputs::disarm(&mut self, levels: Levels, now_ms: u64)`. `Levels` holds previous/play/next/encoder-switch pressed flags and A/B levels; `InputEvent` is `Previous`, `PlayPause`, `Next`, `Mute`, or `Volume(i8)`. Emit at most five events per sample, preserving simultaneous presses.

- [ ] Write two small table-driven tests: `buttons_require_stable_press_and_release` and `encoder_counts_complete_detents`. Assert bounce produces no extra press, 20 ms stable press produces one event, holds never repeat, initial held buttons require release, invalid quadrature jumps produce no step, and a complete calibrated detent produces exactly +1 or -1. Disarming resets partial motion and held-button eligibility.
- [ ] Run `cargo test --manifest-path firmware/sonos-desk/Cargo.toml`; establish the tests fail on missing behavior, then implement only enough portable input logic to pass. Keep startup configuration independent of these tests.
- [ ] Inspect official board schematic, chip strapping documentation, display PWR circuit, and actual KY-040 pulls. Record findings in the firmware README with links. Confirm GPIO17 is free when native USB console is used, document grounding/3.3 V logic, and establish inactive display pin levels. Do not silently remap pins.
- [ ] Add the minimal ESP-IDF binary and build configuration, selecting `riscv32imac-esp-espidf`, `ldproxy`, `rust-src`, and native USB console. Use target-gated dependencies and build script so ordinary host tests need neither ESP-IDF nor local credentials. Pin versions only after a successful build. Install/download tools only when executing, using normal permission escalation if needed.
- [ ] Implement `device::run_inputs() -> anyhow::Result<()>`: sample at 1 ms, log semantic input events, and never initialize Wi-Fi/MQTT. Reserve display pins and apply only verified inactive levels. Verify actual detent count/direction on hardware; calibration may change transition count, never user-visible volume-per-click semantics.
- [ ] Verify host tests, formatting, and an ESP32-C6 build. Document explicit `input-check` feature commands; ensure that mode can build without secrets. Commit this working bring-up slice. Hardware checks can remain pending if no board is connected.

## Task 2: MQTT protocol and portable command behavior

**Interfaces:** `Controller::new(config: ControllerConfig) -> Controller`; `Controller::handle(&mut self, event: Event, now: Time, next_id: &mut dyn FnMut() -> uuid::Uuid) -> Vec<Effect>`. `Time` carries monotonic milliseconds plus optional synchronized UTC milliseconds. Events cover input, tick, connection loss, acknowledged subscription readiness, incoming observations/results, and publication failure. Effects cover `Publish(Command)`, readiness changes, and sanitized diagnostics. `Command` owns request UUID, session, command/value, topic, and original monotonic deadline. Device code supplies a random UUID generator; host tests supply a deterministic one.

- [ ] Add four focused event-sequence tests: `readiness_requires_matching_fresh_snapshots`, `volume_batches_without_backlog`, `mute_uses_confirmed_state`, and `uncertain_actions_are_never_replayed`. Cover 45-second age, five-second allowed future skew, fresh inventory removal, session changes, fixed 100 ms batching, net-zero batches, four-slot overload, mute guard, newer-state precedence, 15-second missing result, disconnect, and subsequent readiness without replay. Use plain timestamps and synthetic JSON fixtures; keep these as compact scenarios, not a generic simulator.
- [ ] Run host tests to establish failures, then implement `controller.rs` and `protocol.rs`. Use fresh request IDs per action, not per detent inside a batch. Choose a bounded pending array of four; do not retain a secondary command queue. Suppress duplicate/unknown/old-session results. Require observations newer than the mute action's starting confirmed observation to resolve its guard; stale pre-action cached state cannot unlock it.
- [ ] Implement typed topic/envelope parsing against existing `protocols/v1/sonos-desk.schema.json`. Tolerate extra response fields, reject invalid required values, and omit playback `value` entirely. Parse only needed room metadata, avoiding storage of full artwork/album strings. Keep transport payload assembly bounded separately from deserialized state.
- [ ] Return a drop/log effect when full or unready, never a delayed command. A result timeout or publication failure after possible transmission means uncertain; a definite failure before submission can be reported as unsent. Neither case retries automatically.
- [ ] Run host tests and Clippy; commit the portable behavior. Use a few direct serialization assertions inside existing scenarios, without adding a separate test matrix.

## Task 3: Wi-Fi, verified TLS, and MQTT device adapter

**Interfaces:** `device::run(config: DeviceConfig) -> anyhow::Result<()>`; `mqtt::Connection::publish(&mut self, command: &Command, now_ms: u64) -> anyhow::Result<()>`. The device loop translates GPIO/network events into Task 2 events. It exclusively owns connection creation/destruction and publication. No other thread may set MQTT properties or publish.

- [ ] Implement `build.rs` loading `config.local.toml` only for the normal device binary; emit `rerun-if-changed` for it and its CA file. Validate required values and safe instance/client IDs; never print contents. Supply placeholders in `config.example.toml`. Ignore `config.local.toml`, local certificates/secrets, `target/`, and generated sdkconfig/build outputs. Generate config into `OUT_DIR`; host tests and input-check bypass secret loading.
- [ ] Implement Wi-Fi and configurable SNTP, gating TLS on synchronized time; fail closed on time/certificate errors. Use username/password and a configured CA with hostname verification. Configure native USB logs and avoid secret-containing Debug/URI output.
- [ ] Configure MQTT 5, Clean Start, zero session expiry, QoS 1 subscriptions, and 1–30 second exponential backoff with jitter. Disable library auto-reconnect. Subscribe to required topics and track SUBACKs before enabling readiness. Handle fragmented payloads with a 32 KiB receive cap; oversized/invalid updates fail closed and log a bounded diagnostic. A receive queue overflow invalidates readiness and rebuilds the connection rather than dropping an offline event unnoticed.
- [ ] Inspect the pinned ESP-MQTT source before implementing publication. Use `esp_mqtt5_client_set_publish_property` plus PUBLISH in one exclusively owned adapter operation if the safe wrapper lacks expiry. Reject expired commands before enqueue; compute remaining expiry from the original deadline without renewing it. Never call the setter concurrently with another publish or pass null properties.
- [ ] Ensure the pinned outbox cannot retransmit with renewed expiry: prefer native deadline-aware behavior only if verified in source. Otherwise expire queued entries by five seconds and set retransmission later than that lifetime; verify the actual SDK settings and ordering. If native behavior cannot guarantee the original deadline, implement a narrow custom outbox or stop and report the incompatibility before calling this complete. Do not weaken expiry or substitute QoS 0.
- [ ] On disconnect, immediately invalidate readiness, discard unsent work, mark outstanding actions uncertain, and destroy the MQTT client/outbox from its owner task, never inside its callback. A replacement client starts empty; callbacks carry a connection generation so old events cannot revive readiness. Keep GPIO sampling responsive via a small bounded event queue and separate owner task.
- [ ] Build both input-check and normal firmware. Manually inspect a command on an isolated test instance using MQTTX: QoS 1, retain off, and PUBLISH expiry no greater than five seconds. Withhold PUBACK/disconnect and reconnect; confirm no old command returns. Exercise untrusted CA, wrong hostname, invalid credentials, and missing synchronized time; none may enable controls. Use synthetic room publications only on isolated topics. Record actual results and pending checks in README, without building a new test harness.
- [ ] Commit the connected firmware slice after host checks and target builds pass. Do not claim physical or wire checks passed unless observed.

## Task 4: Lean acceptance and contributor documentation

- [ ] Add the exact working tool versions and commands to the firmware README: host tests, host Clippy, formatting, target build, `espflash` input-check flash/monitor, and normal-mode flash/monitor. Document that build artifacts embed local secrets and must not be shared. Keep input-check prominently available.
- [ ] Add a short acceptance checklist: boot/reboot including buttons held, each playback button, complete encoder clicks in both directions, press-to-mute, readiness after boot, service restart, broker disconnect with active input, recovery without replay, and USB responsiveness. Mark physical checks pending until performed. Live playback checks require an intentional hardware test; do not automate them against the household service.
- [ ] Document broker TLS/account prerequisites and minimal device ACL topics, stable-room-ID lookup, configurable local time source, diagnostic meanings, and display deferral. No infrastructure changes or deployment commands are run as part of implementation.
- [ ] Update root README to describe only implemented controls and link the firmware instructions. Update AGENTS.md's implemented-status statements once source exists; preserve all ownership and scope guidance.
- [ ] Run `cargo test --manifest-path firmware/sonos-desk/Cargo.toml`, host `cargo clippy --manifest-path firmware/sonos-desk/Cargo.toml --all-targets -- -D warnings`, and `cargo fmt --manifest-path firmware/sonos-desk/Cargo.toml --all -- --check`. Run both pinned target build commands from the firmware directory, then `git diff --check`. No Python regression run is needed if Python code is unchanged.
- [ ] Review the diff against the spec, checking particularly replay, stale-state gating, GPIO assignments, and secrets. Remove unnecessary scaffolding or tests that repeat implementation. Commit documentation and report build/test results separately from manual/hardware results.

## Planning verification and references

Only documentation is changed by this plan. Compatibility is checked against
published documentation; a successful target build and actual board wiring
checks are implementation deliverables, not claims made here.

- [Rust ESP-IDF target requirements](https://doc.rust-lang.org/rustc/platform-support/esp-idf.html): ESP32-C6 target, build-std, and ldproxy.
- [Pinned svc manifest](https://raw.githubusercontent.com/esp-rs/esp-idf-svc/v0.53.0/Cargo.toml): HAL compatibility and standard-library support.
- [ESP-IDF v5.5.1 MQTT](https://docs.espressif.com/projects/esp-idf/en/v5.5.1/esp32c6/api-reference/protocols/mqtt.html): MQTT 5, publish properties, outbox/retransmission, and lifecycle APIs.
- [Waveshare board](https://docs.waveshare.com/ESP32-C6-Zero) and its Resources schematic.
- [ESP32-C6 GPIO restrictions](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c6/api-reference/peripherals/gpio.html): GPIO4/5 strapping and GPIO12/13 USB.

Recommended execution: native implementation in this session. The tasks share
a small set of interfaces, and a single implementer can keep the result lean.
Review this plan before selecting execution; firmware implementation has not
started.
