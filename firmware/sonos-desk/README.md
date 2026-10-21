# Sonos Desk firmware

Rust firmware is being built in small learning chunks. The portable input
decoder has an ESP32-C6 GPIO reader and a USB input-check binary. A separate
network-check binary adds Wi-Fi, clock synchronization, and read-only MQTT 5
over verified TLS. The controller binary sends Sonos commands from physical
inputs and periodically updates the display when `http_base_url` is configured.
A separate display-check binary draws one fixed black/white pattern.
A separate frame-check binary fetches and displays one monochrome Sonos frame.
The input decoder uses the small [`tamer`](https://docs.rs/tamer/0.1.0/) crate
for debounce and quadrature decoding. The device binary uses ESP-IDF through
Rust bindings. We keep the mapping to our controls in this repository.

## Try it

From the repository root:

```sh
cargo test --manifest-path firmware/sonos-desk/Cargo.toml
```

## How this chunk works

[`src/inputs.rs`](src/inputs.rs) turns electrical input levels into meaningful
events: previous, play/pause, next, mute, or a volume step.

`Levels` is a snapshot of all six inputs: four switches (including the
encoder's push switch) and the encoder's two rotation contacts. Switch values
are normalized: `true` means pressed. The GPIO adapter translates physical
voltage levels into this representation.

`Inputs` delegates to `tamer::EdgeDetector` for each switch and
`tamer::QuadratureDecoder` for rotation. The caller supplies elapsed
milliseconds, so this module needs neither a hardware clock nor threads.
Call `sample()` regularly with the latest snapshot; it returns any new
events. Simultaneous button presses can produce more than one event.

A button must remain steady for 20 ms before its new state counts. This
filters contact bounce. Only a new press produces an event; holding does not
repeat. A button held at startup must first be released.

The encoder's contacts change in a sequence, such as `00 → 01 → 11 → 10 → 00`.
The library counts four valid transitions as one volume step; reversals cancel
partial movement. Our glue resets the library on impossible two-contact jumps.
Actual direction and transitions per physical click still need checking on
the KY-040; this is a starting calibration, not a hardware validation.

`disarm()` forgets partial rotation and suppresses held button presses. The
connection handler uses it to prevent offline inputs from turning
into actions after reconnecting. It waits for the encoder's resting contacts
before accepting a new rotation sequence.

[`tests/controls.rs`](tests/controls.rs) feeds short input traces into the real
decoder. Its two tests cover buttons and rotation without mocking hardware.

## GPIO and USB chunk

[`src/main.rs`](src/main.rs) applies ESP-IDF's Rust runtime patches and starts
[`src/device.rs`](src/device.rs). The device code does four things in a loop:
read pins, make a `Levels` snapshot, print decoded events, and sleep for about
one millisecond. The 1,000 Hz FreeRTOS tick makes that sleep practical; this
is polling, not a guarantee that every sample happens exactly 1 ms apart.

`PinDriver::input(pin, Pull::Up)` enables an internal pull-up. Each switch
connects its GPIO to ground when pressed, so `is_low()` means pressed. The
encoder's A/B contacts are read as electrical high/low values; they are not
button presses. Rust's `?` returns early if hardware initialization fails.

The native USB console uses GPIO12/13 through the board's USB-C connector.
It does not use UART RX, so the Next button can stay on GPIO17. There is no
USB driver implementation in our code: ESP-IDF supplies the console selected
in [`sdkconfig.defaults`](sdkconfig.defaults).

### Build and monitor

The commands below are for macOS with Xcode Command Line Tools and `uv`
installed. Start in `firmware/sonos-desk/`. One-time setup:

```sh
rustup toolchain install nightly-2026-09-01 --component rust-src --component rustfmt --component clippy
cargo +stable install ldproxy --version 0.3.5 --locked --root target/tools
cargo +stable install espflash --version 4.6.0 --locked --root target/tools
uv venv --python 3.13 target/build-tools
uv pip install --python target/build-tools/bin/python 'cmake==3.31.10' 'ninja==1.13.2'
```

Those tools are already installed locally for this checkout. Use this path
for the build; `/usr/bin` keeps ESP-IDF on a non-virtualenv Python so it can
create its own environment:

```sh
export PATH="/usr/bin:$PWD/target/tools/bin:$PWD/target/build-tools/bin:$PATH"
cargo build --locked -Zbuild-std=std,panic_abort --target riscv32imac-esp-espidf --features input-check --bin input-check
```

[`rust-toolchain.toml`](rust-toolchain.toml) pins the device toolchain. The
lockfile keeps libc at 0.2.189: today's nightly with libc 0.2.191 failed in
ESP-IDF's standard-library filesystem bindings. Use `--locked` rather than
updating this dependency independently. ESP-IDF is pinned to v5.5.1; its
SDK/compiler downloads live in the ignored `.embuild/` directory. Initial
setup downloads several GB; later builds reuse them. All generated tools,
SDK files, and binaries are excluded from Git.

After checking the wiring, flashing this image replaces the board's current
application. Close any monitor first, then flash and reopen it:

```sh
just firmware-flash input-check
just firmware-monitor
```

Choose the correct USB device if prompted. To reopen the monitor without
flashing, run `just firmware-monitor` from `firmware/sonos-desk/`. This uses
the project-local espflash installation; `Ctrl+C` closes the monitor.
If automatic download mode fails, hold
BOOT, press and release RESET, then release BOOT and retry. The monitor
should show the startup line and events such as `Previous`, `PlayPause`,
`Next`, `Mute`, and `Volume(1)`/`Volume(-1)` with timestamps. Physical direction
and clicks per cycle must be calibrated using the actual encoder.

### Wiring

| Input | GPIO | Connection |
| --- | --- | --- |
| Encoder CLK / DT | 0 / 1 | KY-040 CLK / DT |
| Encoder switch | 2 | KY-040 SW |
| Previous | 3 | Switch to GND |
| Play/pause | 5 | Switch to GND |
| Next | 17 (RX) | Switch to GND |

Power the encoder module from 3V3 and share GND. Its pull-ups, if fitted,
must go to 3.3 V rather than 5 V. KY-040 module variants differ; the actual
board's pull resistors and clicks per cycle still require physical checking.
For each four-leg tactile switch, use terminals that connect only when
pressed; two legs on the same contact are already connected internally.

Display assignments remain PWR=4, BUSY=14, DIN=18, CLK=19, DC=20, CS=21,
RST=22. During input-check, PWR/RST/DIN/CLK/DC are held low, CS is high,
and BUSY is an input. The screen is not initialized or refreshed. This
follows Waveshare's active-high PWR convention and low RST on power-down;
confirm the attached HAT revision before flashing with the display connected.

GPIO4/5 are strapping pins, but they select SDIO clock edges, not normal
flash/download boot mode. This firmware does not use SDIO. GPIO8/9 select
boot mode; GPIO15 controls the JTAG source under the documented eFuse
settings. None is repurposed here. No pin remapping or eFuse changes are
needed for this input-check wiring.

References checked:

- [Waveshare ESP32-C6 Zero schematic](https://files.waveshare.com/wiki/ESP32-C6-Zero/ESP32-C6-Zero.pdf): native USB and RX/header routing. Its chip label differs from the current product description; do not assume flash capacity from the label alone.
- [ESP32-C6 datasheet, boot configurations](https://documentation.espressif.com/esp32-c6_datasheet_en.html?q=ESP32-C6): GPIO4/5 SDIO timing straps and GPIO8/9 boot mode.
- [Waveshare display manual](https://www.waveshare.com/wiki/3.97inch_e-Paper_HAT%2B_Manual): 3.3 V supply/logic and SPI interface.
- [Waveshare GPIO configuration source](https://github.com/waveshareteam/e-Paper/blob/master/RaspberryPi_JetsonNano/python/lib/waveshare_epd/epdconfig.py): active-high PWR and low PWR/RST at shutdown. The exact HAT schematic was unavailable during this check, so board-revision confirmation remains a physical acceptance item.

### Hardware acceptance

- [ ] Inspect connections with USB power unplugged; switches close to GND.
- [ ] Confirm the encoder uses 3V3 and the display's power-control revision matches.
- [x] Boot and see `Input check ready` on the native USB console.
- [ ] Each button prints its named event once; holding never repeats.
- [x] Volume events in both directions; corrected direction confirmed by user.
- [ ] Encoder press prints `Mute` once per press.
- [ ] Reset while holding a button: no event until release and a new press.

The first hardware run confirmed all four switch events and volume events
in both directions, with the display disconnected. The user confirmed the corrected volume direction works on the hardware.
Hold behavior and reset-with-button-held remain manual acceptance checks. No Sonos commands can be sent by this binary.

## Connection-check chunk

Run `just firmware-build network-check` from this directory to build the
connection image. `just firmware-build` still builds the input-only image,
which needs no credentials and does not initialize networking.

Close the USB monitor, then run `just firmware-flash network-check` to flash.
The flash recipe supplies `partitions.csv` directly to espflash: the board has
8 MB of flash, with a 3 MB factory app partition for the TLS-enabled image.
Use this recipe for either image so the bootloader and partition table match.
Afterward, run `just firmware-monitor` to watch the connection stages.

Copy `config.example.toml` to the ignored `config.local.toml` and fill in the
Wi-Fi passphrase, broker hostname/port/account, unique client ID, instance,
and NTP server. Copy the trusted broker CA PEM to `broker-ca.local.pem`.
The hostname must match the broker certificate; IPv4 addresses require an IP
subject alternative name in that certificate. This slice accepts DNS names
or IPv4, WPA2/WPA3 passphrases of 8–63 ASCII bytes, and a dedicated broker
username/password. Enterprise/open Wi-Fi and mutual TLS are not implemented.

Both files are embedded in the network-check image. Do not share its ELF,
binary, or build output directory: it contains credentials. Source files and
examples contain placeholders only. Changing local settings triggers a rebuild.
`SONOS_DESK_CONFIG_DIR` optionally selects another directory containing the
same two filenames; it does not change their format.

[`src/config.rs`](src/config.rs) uses Serde/TOML to load settings. Its checks
reject invalid lengths, broker identifiers, and URI prefixes, and return fixed
error messages so parser errors cannot quote passwords. There is one small
configuration test alongside the two input tests; no network test framework.

[`src/network_main.rs`](src/network_main.rs) gives the modem to a separate
worker and keeps the existing GPIO loop on the main thread.
[`src/network.rs`](src/network.rs) sequences library services:

1. Connect Wi-Fi and wait for a DHCP address. Wi-Fi settings stay in RAM.
2. Wait for time from the configured NTP server. Configure a LAN time server
   to keep normal operation independent of public Internet services.
3. Connect with MQTT 5 using TLS, CA validation, and hostname verification.
4. Subscribe at QoS 1 to `stonkworks/v1/sonos-desk/<instance>/availability`.

The MQTT callback queues short status events. It does not print payloads or
perform blocking work. The owner recreates the client after Wi-Fi/MQTT loss
or queue overflow with backoff from one to 30 seconds and a small jitter.
Clean Start and session expiry zero prevent restoring an offline session.

Expected USB status lines are `Wi-Fi: connected; IP address acquired`,
`Clock: synchronized`, `MQTT: connected over verified TLS`, subscription
acknowledgment, and service availability received. If time is unavailable,
TLS is not attempted. Certificate/authentication failures never fall back
to plaintext or disable verification. Check the preceding stage and broker
logs if the firmware reports that the network is unavailable.

This is connection diagnostics, not controller readiness. It does not yet
parse the service session, room inventory, or speaker state, and it never
publishes commands. Buttons continue to print locally during connection
attempts. Transport subscription acknowledgment alone is not proof of topic
permission; receiving service availability verifies the read path.

For the dedicated broker account, this chunk only requires read access to
`stonkworks/v1/sonos-desk/<instance>/availability`. Additional room/result reads
and command-topic write permission belong to the controls chunk. Broker
installation, accounts, certificates, and NTP installation remain separately
managed household infrastructure.

Manual acceptance: verify Wi-Fi, synchronized clock, verified TLS, subscription,
and receipt of service availability; keep input logging working throughout.
Wrong CA/hostname/account and interrupted connections should remain offline
and retry. Live results are recorded separately from the build/host checks.

Hardware check (2026-10-20): flashed the network image on the ESP32-C6 Zero;
observed Wi-Fi/DHCP, NTP synchronization, verified TLS MQTT connection,
subscription acknowledgment, and service availability receipt. The display
remained disconnected. Input operation alongside networking, deliberate
connection failures, and reconnect recovery remain manual acceptance checks.

## Controller chunk

The `controller` image connects the same GPIO inputs to the existing Sonos
MQTT protocol. Keep the display disconnected for this milestone. Add the
stable speaker ID as top-level `room_id` in the ignored local configuration;
find it in the service's `/rooms` response. A room's display name is not its ID.
The network-check image still works without this assignment and sends no commands.

From this directory, with the USB monitor closed:

```sh
just firmware-build controller
just firmware-flash controller
just firmware-monitor
```

Wait for `Controller: ready` before testing physical controls. This image
changes real playback in the configured room when you press or turn them.

| Input | Command |
| --- | --- |
| Previous / play-pause / next | `previous` / `play_pause` / `next` |
| Clockwise / counterclockwise click | `volume_relative`, +1 / -1 |
| Encoder press | `mute`, inverse of confirmed room mute state |

[`src/controller.rs`](src/controller.rs) is the small portable decision layer.
It accepts input, observations, and time, then returns effects to send or log.
It requires all five QoS 1 subscriptions plus matching fresh inventory, room
state, room availability, and an online service session. Observations age out
at 45 seconds; future skew up to five seconds is allowed. Inputs while unready
are discarded, and entering ready disarms held buttons and partial rotation.

Volume clicks are summed in a fixed 100 ms window, clamped to ±100, and a
net-zero batch is omitted. Four pending commands are allowed; overload drops
new actions. Each command gets a fresh UUID. A mute guard prevents repeated
presses from toggling unconfirmed state. Results correlate by request, client,
room, and service session; older results cannot overwrite newer room state.
A missing result after 15 seconds is uncertain and is never retried.

[`src/mqtt.rs`](src/mqtt.rs) keeps the SDK client on one owner task. The SDK
handles Wi-Fi, SNTP, TLS, and MQTT; Serde/serde_json, time, and uuid handle the
wire format, timestamps, and IDs. Incoming messages are assembled up to 32 KiB,
with a bounded callback queue and 64 KiB aggregate payload budget. Overflow,
malformed updates, failed subscriptions, or disconnects disable control and
rebuild the connection. Input queues carry a readiness generation and timestamp;
stale events cannot become commands after reconnect.

The pinned safe MQTT wrapper omits PUBLISH expiry, SUBACK grant codes, and
retransmission timing. Narrow SDK calls fill those gaps: only granted QoS 1
subscriptions count; each publication sets remaining expiry (at most five
seconds), QoS 1, retain off. Properties remain allocated until SDK destruction.
The stock outbox reuses encoded expiry on retransmit, so commands expire from
its outbox after five seconds, before its ten-second retransmission threshold.
The SDK sweeps expired entries before retransmit selection, refuses publishing
while disconnected, and has auto-reconnect disabled. Dropping the client
clears its outbox before a replacement is created. An SDK upgrade must recheck
these assumptions against `mqtt_client.c`, `mqtt5_client.c`, and `mqtt_outbox.c`.

A successful input logs `Command: sent ...` then `Command: completed`.
`sent` alone is a transport submission, not proof of speaker execution.
Rejection, uncertainty, timeout, and connection loss never trigger an automatic
application retry. Inspect room state before choosing another physical action.

The dedicated device account needs read access below its instance prefix to
`availability`, `rooms`, `rooms/<room_id>/state`,
`rooms/<room_id>/availability`, and `clients/<client_id>/results`; grant write
access only to `rooms/<room_id>/command`. Readiness cannot establish command
write permission; the first actual command/result verifies that path.

Verification uses seven host tests in one file: two input traces, one local
configuration check, and four controller scenarios. There is no broker test
framework. From this directory:

```sh
cargo test --locked --all-features
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo fmt --check
# Use the PATH setup above for the target check:
cargo clippy --locked -Zbuild-std=std,panic_abort --target riscv32imac-esp-espidf --all-features --bins -- -D warnings
```

Hardware acceptance (2026-10-20): controller flashed, reached ready, and the
user confirmed it controlled Sonos. USB logs showed volume batches in both
directions, previous, play/pause, and mute with completed results. The reviewed
mute-guard correction was reflashed and reached ready. Browser state
remains available from the existing Sonos Desk HTTP service.

Remaining opt-in checks:

- [ ] Confirm next, one action per press, repeated mute guard behavior, holds, and boot with a button held.
- [ ] Inspect an isolated command in MQTTX: fresh UUID/session, QoS 1, retain off, expiry ≤5 seconds, correct value omission/type.
- [ ] On isolated topics, withhold PUBACK/disconnect and reconnect; verify no old command returns.
- [ ] Interrupt connectivity while operating inputs; verify recovery accepts only new presses/complete clicks.
- [ ] Exercise service restart, wrong CA/hostname/account, and unavailable NTP; none may bypass readiness.

These fault/wire checks have not been performed. Host tests and successful
live controls do not establish those hardware/broker failure cases. Synthetic
room updates belong only on an isolated instance with a separate account;
never publish them over the household service's state topics.

## First e-ink chunk: fixed pattern

With the display connected to the agreed pins above, close the USB monitor
and run these commands from this directory:

```sh
just firmware-build display-check
just firmware-flash display-check
just firmware-monitor
```

This diagnostic image replaces the controller temporarily. It needs no local
credentials, starts no networking, and sends no Sonos commands. Resetting the
board draws the pattern once more; there is no automatic refresh/retry loop.

The expected 800 × 480 image has an eight-pixel black border, a solid square
at top-left, hollow square at top-right, three bars at bottom-left, checkerboard
at bottom-right, and a right-pointing arrow. The framebuffer is rotated 180° to match the
user-confirmed mounting. Distinct corners expose rotation
and mirroring, and black/white areas expose inverted pixel polarity.

[`src/display.rs`](src/display.rs) uses ESP-IDF's SPI driver in mode 0 at 1 MHz
on GPIO19/18/21 (clock/data/CS). GPIO20 is DC, GPIO22 reset, GPIO14 BUSY,
and GPIO4 active-high power control. VCC remains on 3V3; it is separate from
PWR. The board adapter follows Waveshare's reset timing: high 20 ms, low 2 ms,
high 20 ms. A dedicated 12 KiB worker keeps setup away from the small main
stack. The 48,000-byte monochrome framebuffer is allocated on the heap.

[`src/panel.rs`](src/panel.rs) uses the existing
[`ssd1677` 0.1.0 interface](https://docs.rs/ssd1677/0.1.0/ssd1677/interface/index.html)
for SPI/DC commands and BUSY polling with a 15-second timeout per wait. The
panel settings follow Waveshare's exact `EPD_3in97.c` sequence, with both RAM
planes initialized, full refresh using the OTP waveform, and deep sleep after
completion. The crate's generic initialization writes additional VCOM/auto-clear
settings, so it is intentionally bypassed. No custom waveform is supplied.
The vendor permission notice is preserved in
[third-party/Waveshare.txt](third-party/Waveshare.txt).

The power-control polarity and shutdown behavior were checked against the
exact panel's `DEV_Config.c` in
[Waveshare's official demo archive](https://files.waveshare.com/wiki/3.97inch_e-Paper_HAT%2B/3in97_e-Paper.zip).
The [manual](https://www.waveshare.com/wiki/3.97inch_e-Paper_HAT%2B_Manual)
specifies SPI mode 0 and matching supply/IO levels. This checks the supplied
black/white HAT+ model's documentation; it does not electrically inspect the
user's wiring or identify an unreported board revision.

Successful USB output ends with `refresh finished; sleep command sent` and
`power off; image retained`. A BUSY timeout
stops the sequence and powers off rather than retrying. PWR stays driven low
while the program parks. A completed SPI sequence alone does not prove the
screen displayed the image; visual confirmation is required.

One focused test checks panel commands, addressing, RAM data, full refresh,
sleep ordering, and stopping at each injected BUSY failure. There are eight
host tests total with `cargo test --locked --all-features`; the existing seven
control tests remain unchanged.

Hardware run (2026-10-21): image flashed, booted, and reported completed refresh,
sleep, and power-off without timeout. The user saw the complete pattern rotated
180°; byte/bit reversal corrects that mounting orientation. The correction was
rebuilt and reflashed, and the user confirmed the corrected orientation. This
diagnostic is one-shot; the controller adds live partial updates alongside MQTT controls.

To restore physical Sonos controls, close the monitor, then run
`just firmware-flash controller`. With `http_base_url` configured, that image also
updates the display as described below.


## One Sonos frame over HTTP

Set top-level `http_base_url` and `room_id` in the ignored `config.local.toml`,
using `config.example.toml` as the template. The HTTP address must be a plain
`http://host[:port]` origin on the trusted LAN, with no path or credentials.
This chunk uses the existing unauthenticated HTTP frame endpoint; MQTT retains
its separate verified TLS configuration.

```sh
just firmware-build frame-check
just firmware-flash frame-check
just firmware-monitor
```

This replaces the controller temporarily. On each boot it connects Wi-Fi,
fetches `/frame?room=<configured-id>&mode=mono`, disconnects Wi-Fi, decodes the
PNG, draws once in the confirmed orientation, and sleeps the panel. The power
pin stays low until the frame has been downloaded and validated. Errors stop
without refreshing or retrying. Buttons do not control Sonos in this image.

The download accepts a 200 `image/png` response, has a 128 KiB size cap,
five-second socket timeout and twenty-second body deadline, and follows no
redirects. Only complete, non-interlaced 800 × 480 grayscale PNGs with eight-bit
black/white pixels are accepted. `oxiarc-png` supplies PNG/DEFLATE decoding;
our small adapter packs rows into the panel's 48,000-byte buffer. A dedicated
24 KiB worker handles this image. No custom image codec is implemented.

The original PNG decoder passed host tests but requested a 205,312-byte buffer
on the C6 and exhausted its heap. The replacement uses bounded row buffers.
One focused decoder test checks packing, malformed input, format rejection,
and a 64 KiB maximum individual allocation for its fixture. All nine host
tests pass; that allocation guard does not measure total device memory use.
This diagnostic remains one-shot. The controller adds live partial updates below;
grayscale remains separate work.

Hardware run (2026-10-21): the C6 fetched a 9,640-byte real Sonos frame,
disconnected Wi-Fi, validated the PNG, and reported completed refresh, panel
sleep, and power-off without timeout or allocation failure. The user confirmed the Sonos artwork and text are upright and complete. That run used the frame-check image.


## Controls with live display updates

Build and flash `controller` using the existing justfile commands. Set the
optional `http_base_url` and existing `room_id` in local configuration to enable
the display. Omitting the HTTP origin keeps the panel off and controls available.
The diagnostic images retain their previous roles.

The display worker waits for fresh MQTT controller readiness. It shares the
network worker's Wi-Fi connection and never starts or stops Wi-Fi. HTTP, PNG
row decoding, SPI transfer and BUSY waits run separately from MQTT/input handling.
Only the display task owns the display pins; the input loop owns the controls.

**What wakes it.** The MQTT thread wakes the worker when the room's visible state
changes (title, artist, album, artwork URL, `playback_state`, volume or mute) and
when a command result completes; results carry the new state sooner than the
service's roughly three-second polling. Bursts, such as spinning the dial, coalesce
into one fetch 150 ms after the first change. Each such fetch is followed by one
settle fetch 2.5 seconds later, because the service caches state for a second and
can briefly report `TRANSITIONING`. A 180-second poll remains as a fallback; a
failed fetch before the first frame retries after 10 seconds.

**How it draws.** The first frame is a full refresh. After that, the worker
compares each fetched frame with the cached one (`changed_region`) and partially
refreshes only the byte-aligned box that changed, using Waveshare's partial
sequence on both RAM planes (`show_partial`). Identical frames and HTTP 304
responses cost no panel traffic. The cached frame is replaced only after the panel
call succeeds. A partial takes about 0.5 to 0.9 seconds on this panel (see the
spike findings in the design spec). PWR stays high after every draw; the panel
sleeps with its RAM retained.

This controller **swaps its two RAM planes after every refresh**. A partial that
wrote only its changed window would leave the rest of the screen holding "old" in
the plane that now counts as new, and the next partial would draw the previous one
back (found on hardware, 2026-10-21: each partial reverted its predecessor's box).
`show_partial` therefore writes the new window into both planes after each refresh.

**Ghosting cleanup.** Full, flashing refreshes never happen while you are using the
controls. After 60 seconds without input or visible changes, if any partials were
drawn since the last full refresh, the worker redraws the cached frame in full. A
24-hour backstop does the same even when every fetch was a 304; sustained partials
do not postpone it, and it waits for the same idle window. **Hold Mute for two
seconds** to force a full refresh immediately. Because of this gesture, Mute now
toggles on release (a hold under two seconds), not on press; the other buttons are
unchanged.

`If-None-Match` carries the ETag of the last frame shown. Fetch/decode failures retain
that ETag and the old image. Panel/SPI failure disables display updates until reset,
leaving controls running. No image queue or automatic replay of Sonos commands is
added. The worker logs free heap after each update.

Hardware acceptance for live updates is recorded below once run. The earlier
periodic-refresh build's acceptance follows for history.

Hardware acceptance, periodic build (2026-10-21): the combined controller reached MQTT readiness,
fetched and drew the first frame with Wi-Fi/MQTT still active, then fetched a
changed frame and refreshed again after the three-minute interval. Both refreshes
completed sleep/power-off without allocation failure or timeout. The user confirmed
controls work and the automatic screen update appeared. Volume, play/pause, mute
and next commands logged successful results between refreshes. HTTP 304 handling
and the daily force-refresh are implemented; those paths were not observed during
this short hardware run. The combined controller remains flashed; monitor closed.
