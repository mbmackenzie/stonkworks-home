# Sonos Desk firmware

Rust firmware is being built in small learning chunks. The current chunk is
only an input decoder that runs on your computer: no GPIO access, Wi-Fi,
MQTT, or display driver is implemented yet. It has no external dependencies.

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

`Inputs` remembers the previous snapshot and timing. The caller supplies
elapsed milliseconds, so this module needs neither a hardware clock nor
threads. Call `sample()` regularly with the latest snapshot; it returns any
new events. Simultaneous button presses can produce more than one event.

A button must remain steady for 20 ms before its new state counts. This
filters contact bounce. Only a new press produces an event; holding does not
repeat. A button held at startup must first be released.

The encoder's contacts change in a sequence, such as `00 → 01 → 11 → 10 → 00`.
The decoder counts a completed four-transition cycle as one volume step.
Reversals cancel partial movement, and impossible two-contact jumps reset it.
Actual direction and transitions per physical click still need checking on
the KY-040; this is a starting calibration, not a hardware validation.

`disarm()` forgets partial rotation and suppresses held button presses. The
future connection handler will use it to prevent offline inputs from turning
into actions after reconnecting.

[`tests/controls.rs`](tests/controls.rs) feeds short input traces into the real
decoder. Its two tests cover buttons and rotation without mocking hardware.

Next chunk: read the assigned ESP32-C6 GPIOs and print these events over native
USB, without connecting to Sonos. Verify pin electrical constraints before
flashing; the supplied wiring has not been validated by this host-only chunk.
