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

## Decisions added during planning

These come from reading the code and `docs/sonos-desk.md`. They refine the
design above and need the user's confirmation.

- **Triggers include command results.** The service polls rooms about every
  three seconds, so a state message can lag a dial turn. A completed command
  result already carries the new room state, so a result also wakes the
  display worker. A state message wakes it only when title, artist, album,
  artwork URL, `playback_state`, volume or mute changed.
- **One settle re-fetch.** The service caches state for one second and can
  return `TRANSITIONING`, so after each partial update the worker fetches once
  more about 2.5 seconds later. A 304 makes that free.
- **Forced-refresh gesture: hold Mute for 2 seconds.** Buttons currently act
  on press, so holding Mute would toggle it. Mute therefore acts on release
  when held under 2 seconds, and a longer hold requests a full refresh and
  does not toggle mute. Other buttons are unchanged.
- **Cross-thread signalling uses three `AtomicU32` counters** (changed,
  activity, force); the C6 has no 64-bit atomics.
- **The ETag no longer expires at 24 hours.** The 24-hour backstop redraws the
  cached frame instead, per the scheduling contract above.

## Spike findings (2026-10-21, hardware: ESP32-C6 + Waveshare 3.97" HAT+)

Method: throwaway `partial-check` image drew a synthetic now-playing screen
(dithered 360 x 360 art, text bars, volume bar), then partially refreshed a
volume-bar box (8 steps, then 20 alternating one-second updates), a title box
and the art box, under four variants. Source: Waveshare's
`EPD_3in97_Display_Partial` in the official demo archive.

- **Sequence that works:** hardware reset (unless the panel is awake), `0x18 =
  0x80`, `0x3c = 0x80`, data entry mode `0x11 = 0x03`, RAM window with
  `0x44`/`0x45` and counters `0x4e`/`0x4f`, window data to `0x24`, then `0x22 =
  0xff`, `0x20`, wait for BUSY. No deep-sleep command is needed between
  partials. Vendor window ends are set as the start pixel of the last byte (x)
  and last row (y).
- **Orientation:** the 180-degree mounting maps a frame window to the same
  rows and to x mirrored (RAM x = 800 - frame x). Data goes out row by row in
  ascending RAM-y order, each row's bytes in reverse column order and
  bit-reversed.
- **Variants, all error-free:** (0) new plane only, (1) old window also written
  to `0x26`, (2) variant 0 without the reset when already awake, (3) variant 1
  without that reset.
- **Timing:** full draw about 3.1 s. Partial: volume-bar box 512-564 ms, title
  box 563-666 ms, art box 651-840 ms; no slowdown across a 20-update burst.
  Skipping the reset saves about 45 ms.
- **Visual result (user):** updates are clean, ghosting not noticeable, no
  whole-screen flash. The art region needed no localized flash. The user did not
  distinguish between the variants.
- **Not tested:** whether RAM contents survive `PWR` low, and partial quality
  after hours of use. Because it is unknown, Task 3 must write the old window to
  `0x26` on every partial (variant 1), keep `PWR` high while partials are
  active, and write both planes in full after any power-off before the first
  partial.
- **Chosen for implementation:** variant 1 (reset each time, old plane
  written). It is robust to deep sleep and power-off, and costs about 45 ms over
  variant 3.

## Implementation outcome (2026-10-21)

What shipped differs from the design above in these ways, all found on hardware:

- **Plane swap.** The controller swaps its two RAM planes after every refresh. A partial
  that wrote only its window made the next partial draw the previous one back. Every
  partial now writes the new window into both planes after refreshing. The spike did not
  catch this because its updates were too regular to notice.
- **Memory.** PNG decoding peaked at about 82 KB beside two 48 KB frames and crashed the
  controller on playlist artwork. The service gained `GET /frame?...&mode=mono&format=bitmap`
  (48,000 packed bytes) and the controller fetches only that, refusing PNG responses with a
  recoverable error. See `docs/sonos-desk.md`.
- **PWR stays high** after every draw; the panel sleeps with RAM retained. The planned reload
  after power-off was unnecessary and removed.
- **Boxes.** Changes are drawn as separate boxes (art, text, labels), at most four, rather
  than one bounding box.
- **Drive style.** One pass. Two passes, three passes and erase-then-write were compared by
  eye: no pass count helped and erase-then-write was clearly worse.
- **Cleanup.** Not idle-only. Four seconds after a partial changes the art or the text
  (`Region::is_content`), the controller redraws the cached frame with the panel's fast full
  waveform (one flash, about 2.7 s in total). Volume, mute and label changes are cleaned after
  60 seconds of quiet, and a 24-hour backstop remains. Holding Mute forces the slow, thorough
  refresh. Presses do not trigger cleanup.
- **Transport label.** The service holds the last settled label through a `TRANSITIONING`
  state for up to 10 seconds, so the header does not change twice per track change.
- **Measured:** a `next` press settles in about 2 s; partial refreshes take about 0.55 s each
  plus transfer; heap low-water mark is about 106 KB free.

Not done: local drawing of the volume bar (not needed: latency is acceptable), and a custom
waveform (no vendor data for this panel; risk of damage).
