# Sonos Desk Rust firmware: controls milestone

Date: 2026-10-20

Status: Written spec approved by the user; lean verification amendment added
at the user's direction during implementation planning.
No firmware is implemented by this document.

The user approved the written spec, then requested lean verification during
planning: a few table-driven tests and a short manual acceptance checklist,
without an extensive test harness. The verification section below incorporates
that later direction.

## Intent and success criteria

Build a USB-powered physical Sonos controller in Rust. First verify the
buttons and rotary encoder through USB logs, then control one locally
configured Sonos room through the existing MQTT 5 interface. The display
will be brought up in a subsequent milestone.

Success means each button press produces one action, each encoder detent
changes volume by one point without acceleration, and encoder press toggles
mute using confirmed state. Network failures must not produce delayed actions
after recovery. Firmware behavior should be testable without a physical
speaker, with wiring verified independently on hardware.

The first household target is chosen through local configuration. Shared
source and examples contain no actual room assignment, addresses, or secrets.

## Hardware and wiring

- Waveshare ESP32-C6 Zero, powered continuously through USB.
- Three 6 x 6 mm momentary tactile switches.
- KY-040 rotary encoder module, including its push switch.
- Waveshare 3.97inch e-Paper HAT+, with driver board, SPI, 800 x 480 pixels,
  black/white with four gray levels. Display implementation is deferred.

User-supplied assignments:

| Peripheral | Signal | GPIO |
| --- | --- | --- |
| Encoder | CLK | 0 |
| Encoder | DT | 1 |
| Encoder | SW / mute | 2 |
| Button | Previous | 3 |
| Button | Play/pause | 5 |
| Button | Next | 17 (RX label) |
| Display | PWR | 4 |
| Display | BUSY | 14 |
| Display | DIN | 18 |
| Display | CLK | 19 |
| Display | DC | 20 |
| Display | CS | 21 |
| Display | RST | 22 |

The user supplies peripherals from 3V3 and GND. Board 5V, TX, and GPIO15
are unused. Display PWR is a driver-board control signal, separate from its
supply connection. These are proposed assignments, not an electrically
validated schematic. Before flashing, verify them against the board schematic
and display documentation, including boot/strapping behavior, signal levels,
encoder pull-ups, switch grounding, and display power-control polarity.
USB logging must use the board's native USB path rather than claim GPIO17
for UART RX. If any conflict is found, resolve it with the user before
changing the agreed wiring.

## Scope and architecture

Use Rust on ESP-IDF as the recommended foundation, with a small
hardware-independent controller core. Confirm compatible released Rust,
ESP-IDF, and binding versions during implementation planning. In particular,
verify MQTT 5 per-publication expiry and retransmission/outbox behavior;
CONNECT or Will expiry is not a substitute for command PUBLISH expiry.
Where bindings lack necessary publish controls, use a narrowly encapsulated
ESP-IDF interface rather than weaken the protocol.

Responsibilities:

1. Inputs: sample and debounce buttons; decode encoder transitions into
   complete detents; emit semantic input events.
2. Controller: apply readiness rules, batch volume changes, construct actions,
   track results, and maintain confirmed state. This logic runs in host tests
   with simulated clocks and transports.
3. Network adapter: manage Wi-Fi, clock synchronization, verified TLS,
   MQTT 5 subscriptions, bounded message assembly, and publication.

Keep hardware input handling responsive while networking waits. Network
callbacks enqueue bounded events and do not perform controller logic or
blocking peripheral operations. A later display task must not block inputs.

A combined firmware loop would make initial scaffolding smaller but mix
hardware, protocol, and recovery behavior. A general device framework adds
unneeded abstractions. The selected three-part structure provides focused
testing without building future Home or provisioning subsystems.

Create firmware source under `firmware/sonos-desk/` only during implementation.
Do not create empty hub, registry, onboarding, or other-device scaffolds.

## Input behavior

Each debounced press edge triggers once; holding a button never repeats.
Inputs held during boot or a transition into ready must be released before
they can trigger a new press. Partial encoder transitions are discarded when
readiness is lost, so an offline movement cannot complete an online action.

| Input | MQTT command |
| --- | --- |
| Previous button | `previous`, omit value |
| Play/pause button | `play_pause`, omit value |
| Next button | `next`, omit value |
| Clockwise encoder detent | `volume_relative`, +1 |
| Counterclockwise encoder detent | `volume_relative`, -1 |
| Encoder press | `mute`, inverse of confirmed mute state |

Calibrate encoder direction and transitions per detent on the actual module;
one electrical edge is not necessarily one physical click. No acceleration.
Use a 100 ms maximum batching window from the first detent: sum volume steps
in that window, omit net zero, and clamp the batch to -100 through 100.
Flush at the deadline even during continuous rotation. The backend clamps
the resulting speaker volume to 0 through 100.

Use a bounded command tracker and no unbounded input backlog. Start with a
maximum of four outstanding commands; drop and log new actions if full rather
than retaining them for later. Do not delay a volume batch beyond its window
to wait for tracker space. Suppress further mute presses while a mute outcome
is unresolved. Results or a subsequent fresh observation resolve that guard;
cached state from before the action does not.

## Connection and readiness

Startup connects Wi-Fi, obtains a usable UTC clock for certificate validation
and observation freshness, then connects to the configured broker over TLS.
Time synchronization uses a configurable source; deployment can use local
network time so normal operation need not depend on a developer server or
cloud service. Without usable time, controls remain disabled.

Subscribe to service availability, room inventory, the configured room's
state and availability, and this device's result topic before accepting input.
Use a clean MQTT session with no persistent offline subscriptions.

Readiness follows the existing v1 protocol:

- Broker connected and required subscriptions acknowledged.
- Service online with a known process session.
- Inventory, room state, and room availability match that session.
- Configured stable room ID belongs to current inventory and is online.
- All three observations are no more than 45 seconds old.

Compare observation times using synchronized UTC; use monotonic time for
local deadlines, debounce, and backoff. Allow observations up to five seconds
in the future for clock skew, treating their initial age as zero; reject
observations further in the future. They still expire at 45 seconds of age.

Invalidate readiness immediately on disconnect, service session change,
offline status, fresh inventory removal, or stale observations. Clear unsent
actions and volume batches. Retained data after reconnect must pass the same
freshness checks; retained online alone is insufficient.

Reconnect Wi-Fi/MQTT with exponential backoff from one to 30 seconds and
jitter. Certificate, authentication, and time failures appear in sanitized
USB diagnostics. Never disable certificate or hostname verification.

## Commands, results, and uncertainty

Follow [MQTT v1](../../../protocols/v1/sonos-desk.md) without changing backend
contracts. Playback follows the coordinator; volume/mute target the selected
room. Address topics using the configured stable speaker ID, never its label.

Each emitted action (including a volume batch) receives a new UUID request ID,
the configured application client ID, and the current service session. Send
QoS 1, retain off, with a five-second MQTT Message Expiry Interval. Omit value
entirely for playback actions. Subscribe to results before publishing.

Correlate results by request ID and validate service session, client ID, and
room ID. Completed results update confirmed state. Rejected results log the
rejection and do not repeat the action. Uncertain results and a missing result
after 15 seconds mark the outcome uncertain; inspect subsequent state rather
than automatically repeating it. Deduplicate received results and do not
apply older result state over newer observations.

Disconnect makes outstanding results uncertain and clears unsent work. MQTT
QoS 1 transport retransmission during a live connection is distinct from an
application retry and keeps the same identity and original expiry deadline.
Destroy or clear the connection's outgoing command state on disconnect: no
transport outbox may replay actions after reconnect or renew their expiry.
Broker acknowledgments do not establish speaker execution.

While disconnected or not ready, discard all physical input events. Recovery
accepts only new actions once readiness is restored. Report connecting,
ready, unavailable, rejected action, and uncertain outcome through USB logs
without logging credentials or complete sensitive configuration.

## Configuration and security

Provide a documented local build configuration excluded from Git, plus an
example containing placeholders/documentation values. Required settings cover
Wi-Fi SSID/password, broker hostname/port, device username/password, trusted
CA certificate, instance, unique client identity, and stable room ID. Use TLS
port 8883 by default and permit a configured TLS port. Never fall back to
plaintext when TLS fails. Validate identifiers and required values at build
or startup with clear diagnostics.

Use a dedicated broker account. Document permissions to read the required
service/inventory/room/result topics and publish only the assigned room's
command topic. Broker installation, TLS listener setup, and account creation
remain separately managed household infrastructure.

Initial provisioning is local build-and-flash configuration, not an onboarding
interface. Document that secrets included in a firmware image remain present
in that local build artifact; exclude generated configuration and artifacts
from Git. No OTA update, secure-boot rollout, or credential-management service
is included.

## Validation and delivery

Deliver an explicit input-check mode that logs semantic inputs over native
USB and cannot publish Sonos commands. Normal mode enables MQTT operation.

Keep automated verification small: table-driven input traces and controller
event sequences cover debounce, complete detents, batching, stale/session
gates, and uncertain/disconnected command behavior. Use supplied timestamps
rather than sleeping or introducing a clock framework. Prefer a handful of
tests with several cases over one test per method or protocol field.

Use a short opt-in manual broker checklist for verified TLS, authentication,
MQTT 5 expiry, QoS/retain flags, results, and absence of reconnect replay.
Do not build an automated broker/certificate/container harness for this slice.
Observe commands using an isolated test instance and synthetic room data
before intentionally testing live controls. Never use a real command topic
for synthetic tests or alter household infrastructure automatically.

Build the ESP32-C6 target and run Rust formatting/linting and host tests.
Document exact toolchain and flashing commands. Hardware acceptance proceeds
through USB boot/logging, individual buttons, full encoder detents and press,
then explicitly initiated live command checks. Do not claim hardware success
from host tests or initiate live playback changes as part of automated checks.

For this documentation-only stage, verify the spec and whitespace; firmware
tests begin during implementation. Existing Python behavior is unchanged.

## Deferred display milestone and exclusions

The connected display remains uninitialized in this controls milestone;
verify its documented inactive power/control levels before hardware tests.
A separate milestone will test a fixed image, then fetch existing HTTP frames
using the stable room ID and ETags. Preserve 800 x 480 output and existing
mono/gray4 contracts; image decoding, buffer sizing, refresh scheduling, and
the exact display driver are designed then. Frames do not travel over MQTT.

Excluded here: battery sleep, room selection UI, onboarding, retained device
configuration contracts, Home availability, container management, changes to
the Python service, live deployment, registry releases, and licensing.

## References

- [Existing MQTT v1 contract](../../../protocols/v1/sonos-desk.md).
- [Existing HTTP/frame contract](../../sonos-desk.md).
- [Display listing supplied by user](https://www.amazon.com/dp/B0FHHHSDCX).
- [Waveshare display specifications](https://www.waveshare.com/3.97inch-e-paper-hat-plus.htm).
- [Rust ESP-IDF MQTT configuration](https://esp-rs.github.io/esp-idf-svc/esp_idf_svc/mqtt/client/struct.MqttClientConfiguration.html).

Before implementation planning is complete, confirm board electrical details,
dependency versions, and per-message MQTT expiry/outbox controls against
official documentation. These checks can refine implementation details but
must not silently change the agreed hardware or command semantics.
