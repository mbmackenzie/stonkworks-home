# Sonos Desk firmware

Rust firmware is being built in small learning chunks. The portable input
decoder now has an ESP32-C6 GPIO reader and a USB input-check binary. It only
prints events: Wi-Fi, MQTT, Sonos commands, and display rendering come later.
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
are normalized: `true` means pressed. Later, the GPIO adapter will translate
the physical voltage levels into this representation.

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
future connection handler will use it to prevent offline inputs from turning
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
application. To flash and open a serial monitor:

```sh
espflash flash --chip esp32c6 --monitor target/riscv32imac-esp-espidf/debug/input-check
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
