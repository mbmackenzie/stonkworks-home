# Sonos Desk firmware: live partial refresh

Status: design, awaiting review. Nothing here is implemented.

## Goal

When the user turns the dial, the on-screen volume moves. When they press
next, the new track appears within a few seconds without a full-screen flash.
The screen is a controller's visual confirmation that inputs worked.

Current behavior: the display worker fetches `/frame` every three minutes and
does a full OTP-waveform refresh (`firmware/sonos-desk/src/panel.rs`), so
changes can take up to three minutes to appear.

## Decisions made

- MQTT stays. The controller already receives `rooms/<id>/state` and uses it
  for readiness; it also becomes the trigger for display updates.
- The server keeps rendering the 800 x 480 frame. The firmware diffs frames and
  partially refreshes the changed region. No new HTTP contract.
- Local firmware drawing of the volume bar is out of scope. Revisit only if
  hardware shows the round trip (dial, command, Sonos, state, frame, partial
  refresh) is too slow.
- Ghosting cleanup: a full refresh after the screen has been idle, plus a
  long-press gesture that forces one, plus the existing 24 hour backstop.
  Full refreshes never fire during active interaction.

## Non-goals

Grayscale, MQTT frame transport, retained or replayed commands, server-side
deltas, Home/device configuration, and changes to `protocols/v1/`.

## Design

### 0. Spike: partial waveform on this panel

A throwaway diagnostic binary (in the style of `display-check`) answers, on
the real 3.97" SSD1677 panel: which partial command sequence works, how long a
partial refresh takes, how visible ghosting is for a text/volume-bar region and
for a 360 x 360 art region, and whether art needs a brief localized flash.
The Waveshare `EPD_3in97.c` partial sequence is not yet verified and must be
checked against the vendor source in `third-party/` or the demo archive.
Outcome: a recommendation, and the sequence used by section 1. Its code is
throwaway unless promoted by a later decision.

### 1. Panel (`panel.rs`)

Add a partial-update function beside `show_frame`:

- Restrict the RAM window (`0x44`, `0x45`, counters `0x4e`, `0x4f`) to the
  changed box, x aligned to 8 pixels.
- Write the new window to RAM `0x24` and the previously displayed window to
  `0x26`, so the controller drives only changed pixels.
- Run the partial refresh sequence selected by the spike.
- Keep the same orientation correction as `show_frame`.
- Stay powered between partials while active; deep-sleep and power off after
  the idle cleanup. A BUSY timeout stops the sequence without retry, as now.

### 2. Display worker (`updater.rs`, `display.rs`, `frame.rs`)

- Keep the last displayed 48,000-byte frame alongside the ETag.
- An MQTT state change (track, `playback_state`, volume, mute) wakes the
  worker. Changes coalesce: one update in flight, latest state wins.
- Fetch `/frame` with `If-None-Match`. Unless a full refresh is due (below), a
  304 or an identical frame does nothing. Otherwise diff, compute the bounding
  box, partially refresh it, and store the frame and ETag only after the
  refresh succeeds.
- Track `last_full_refresh` separately from the time of the last displayed
  frame. Partial updates never advance it; only a successful full refresh does.
- Idle cleanup: after about 60 seconds without input or state changes, if any
  partials have accumulated since `last_full_refresh`, do one full refresh of
  the cached displayed frame and sleep the panel.
- 24 hour backstop: when `last_full_refresh` is older than 24 hours, a full
  refresh is due even if the fetch returns 304 or an identical frame, and the
  refresh redraws the cached frame. If the user is actively interacting when
  it falls due, it waits for the idle window, so it never flashes mid-use.
  Sustained partial updates cannot postpone it past the next idle window.
- Long-press (gesture to be chosen against `inputs.rs`) forces a full refresh.
- Failure behavior is unchanged: fetch or decode errors keep the old frame;
  panel or SPI failure disables display updates until reset; controls and
  MQTT keep running. No command replay.
- The three-minute poll becomes a fallback for missed state messages.

### 3. Testing

Host tests, using the existing fake interface and timing helpers:

- Diff and bounding-box math, including byte alignment and no-change.
- Partial command sequence: window, both-plane writes, order, BUSY failures.
- Scheduler: coalescing, idle cleanup timing, forced refresh, 304 handling.
- Cleanup deadline: unchanged-frame expiry (304 or identical frame still
  triggers the due full refresh), sustained partial updates not advancing
  `last_full_refresh`, and a due refresh deferred until idle.
- Gesture decoding for the forced refresh.

Hardware acceptance: dial turn updates the volume bar; next track updates art
and title without a full flash; ghosting is judged visually; idle cleanup
occurs and is unobtrusive; MQTT controls remain responsive throughout.

## Risks

- Partial waveform quality on this panel is unknown until the spike.
- Dithered art in a partial region may ghost; the fallback is a brief
  localized flash of the art region only.
- Latency may exceed the target; the follow-up is local volume drawing.
- Memory: a second 48 KB buffer on the ESP32-C6 must be checked against heap
  headroom.

## Documentation to update on implementation

`firmware/sonos-desk/README.md` (the "separate panel experiment" and
three-minute statements) and the `AGENTS.md` firmware summary.
