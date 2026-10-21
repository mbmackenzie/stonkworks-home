# Sonos Desk Partial Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Sonos Desk e-ink screen follow volume, playback and track changes within about a second or two, using partial refreshes and no flash during use.

**Architecture:** MQTT state and command results wake the display worker, which fetches the existing mono `/frame`, diffs it against the frame on the panel, and partially refreshes only the changed box. A pure scheduler decides fetch versus full refresh, and cleans up ghosting when the user is idle. A hardware spike settles the partial waveform before the panel code is finalized.

**Tech Stack:** Rust 2024, ESP-IDF (`esp-idf-svc`), `ssd1677` 0.1.0 interface trait, `oxiarc-png`. Host tests: `cargo test --locked --all-features` from `firmware/sonos-desk/`.

**Spec:** [Partial refresh design](../specs/2026-10-21-sonos-desk-partial-refresh-design.md), including its "Decisions added during planning" section.

## Global Constraints

- Work in `firmware/sonos-desk/`. No change to `protocols/v1/`, the server, or the 800 × 480 PNG contract.
- Frame buffers are 48,000 bytes (`panel::Frame = Box<[u8; 48_000]>`); the decoder keeps its 64 KiB contiguous-allocation limit.
- Full refresh interval backstop is 24 hours (`86_400_000` ms); fallback poll stays 180 seconds (`180_000` ms); idle cleanup after 60 seconds (`60_000` ms); settle re-fetch 2,500 ms; change debounce 150 ms.
- Full refreshes never fire while the user is interacting; they wait for idle.
- A fetch or decode error keeps the old frame; a panel/SPI error disables display updates until reset; controls and MQTT keep running. No command replay.
- 32-bit atomics only (no `AtomicU64`) in anything shared across threads.
- Keep tests lean and in the existing files (`tests/display.rs`, `tests/controls.rs`); no mock framework. Reuse the `Wire` fake in `tests/display.rs`.
- No credentials, household addresses or device assignments in committed files.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Review Focus

- A command result or state message carries the old track or a `TRANSITIONING` state, including when the first fetch returns 304 or identical pixels: the settle re-fetch must still happen (Task 4 test).
- The frame endpoint failing before the first draw must not cause a tight retry loop (Task 4 test).
- A burst of dial turns must produce one fetch in flight, not a queue of stale fetches (Task 4 test).
- A panel draw that mutates its input would corrupt the cached "old" frame and make the next diff wrong (Task 3 test).
- A 304 or identical frame after a state change must cause no panel traffic, except when the 24-hour backstop is due (Task 4 and Task 7).
- Sustained partial updates must not postpone the 24-hour backstop beyond the next idle window (Task 4 test).
- Holding Mute for 2 seconds must not toggle mute, and a short press must still toggle it (Task 6 test).
- A BUSY timeout during a partial refresh must stop the sequence, not retry, and not leave the cache claiming the new frame (Task 3 test and Task 7).

---

### Task 1: Hardware spike — partial waveform on this panel

Throwaway diagnostic, in the style of `display-check`. Its output is findings, not kept code beyond what Task 3 promotes.

**Files:**
- Create: `firmware/sonos-desk/src/partial_main.rs`
- Modify: `firmware/sonos-desk/Cargo.toml` (feature `partial-check = ["display-check"]` and a `[[bin]] partial-check` entry like `display-check`)
- Modify: `firmware/sonos-desk/src/panel.rs` (experimental `pub fn show_partial_experiment` only for this task)
- Modify: `docs/superpowers/specs/2026-10-21-sonos-desk-partial-refresh-design.md` (append "Spike findings")

**Interfaces:**
- Produces: a "Spike findings" section in the spec naming (a) the partial command sequence with exact `0x22` argument and any LUT or register writes, (b) whether RAM contents survive `PWR` low, (c) whether the panel can stay powered between partials, (d) measured partial duration for a volume-bar box, a text box and the 360 × 360 art box, (e) a ghosting verdict per region after 20 consecutive partials, (f) whether art needs a brief localized flash.

- [ ] **Step 1: Read the vendor partial sequence.** Download Waveshare's `3in97_e-Paper.zip` (link in `firmware/sonos-desk/README.md`) into a new empty directory outside the repo. Find the partial-update function in its `EPD_3in97.c` and copy its exact command/data sequence into your notes. If the demo has none, use the SSD1677 datasheet's partial-mode `0x22` values and say so in the findings.
- [ ] **Step 2: Implement `show_partial_experiment`.** Signature: `pub fn show_partial_experiment<I: DisplayInterface, D: DelayNs>(io: &mut I, old: &[u8; 48_000], new: &[u8; 48_000], window: (u16, u16, u16, u16), delay: &mut D) -> Result<(), I::Error>` where `window` is `(x0, x1, y0, y1)` in frame pixels with x multiples of 8. It reuses the init commands from `show_frame`, writes the window of `old` to RAM `0x26` and of `new` to `0x24`, then runs the vendor partial trigger from Step 1.
- [ ] **Step 3: Write `partial_main.rs`.** It draws a full test frame, then runs these scenarios separated by 5-second pauses, logging elapsed ms for each: volume-bar box (410,423)–(770,435) stepping 10 values; title text box; 360 × 360 art box at (24,76) alternating two dithered patterns; 20 consecutive volume-bar partials. Use `Panel` from `display.rs` for pin ownership.
- [ ] **Step 4: Build and flash.** Close the USB monitor first. Run `just firmware-build partial-check` then `just firmware-flash partial-check`. Expected: build succeeds; the monitor logs each scenario's timing.
- [ ] **Step 5: User observes the screen.** Ask the user, per scenario: flash or not, visible ghosting (none / faint / bad), and overall responsiveness. Record answers verbatim.
- [ ] **Step 6: Write "Spike findings" in the spec**, covering items (a)–(f), and commit. If partial quality is unacceptable for the art region, state the localized-flash fallback and its measured duration. **Stop and show the user the findings before starting Tasks 3 and 7.**

```bash
git add firmware/sonos-desk docs/superpowers/specs
git commit -m "Spike partial refresh on the 3.97-inch panel"
```

---

### Task 2: Changed-region detection

**Files:**
- Modify: `firmware/sonos-desk/src/frame.rs`
- Test: `firmware/sonos-desk/tests/display.rs`

**Interfaces:**
- Produces:
  - `pub struct Region { pub x0: u16, pub x1: u16, pub y0: u16, pub y1: u16 }`, pixel coordinates in frame orientation, `x0`/`x1` multiples of 8, `x1` and `y1` exclusive.
  - `pub fn changed_region(old: &[u8; 48_000], new: &[u8; 48_000]) -> Option<Region>`, the smallest byte-aligned box covering every differing byte, or `None` when identical.

- [ ] **Step 1: Write the failing test `changed_region_bounds_differences`** (gated `frame-check`): identical frames give `None`; one differing bit at pixel (17, 3) gives `Region { x0: 16, x1: 24, y0: 3, y1: 4 }`; differences at opposite corners (pixel 0,0 and pixel 799,479) give the full `Region { x0: 0, x1: 800, y0: 0, y1: 480 }`.
- [ ] **Step 2: Run** `cargo test --locked --all-features changed_region` — expect FAIL (not defined).
- [ ] **Step 3: Implement `changed_region`** by scanning rows of 100 bytes and tracking min/max byte column and row.
- [ ] **Step 4: Run** the same command — expect PASS.
- [ ] **Step 5: Commit** (`git add firmware/sonos-desk && git commit -m "Detect the changed region between frames"`).

---

### Task 3: Panel partial update (depends on Task 1 findings)

**Files:**
- Modify: `firmware/sonos-desk/src/panel.rs`, `firmware/sonos-desk/src/display.rs`
- Test: `firmware/sonos-desk/tests/display.rs`

**Interfaces:**
- Consumes: `frame::Region` (Task 2), Task 1's chosen sequence.
- Produces:
  - `pub fn show_frame<I, D>(io: &mut I, frame: &[u8; 48_000], delay: &mut D) -> Result<(), I::Error>` — same behavior and byte output as today but takes `&` and never mutates the frame (apply the 180° byte/bit reversal while sending 100-byte chunks).
  - `pub fn show_partial<I, D>(io: &mut I, old: &[u8; 48_000], new: &[u8; 48_000], region: Region, delay: &mut D) -> Result<(), I::Error>`.
  - `Panel::draw(&mut self, frame: &[u8; 48_000]) -> anyhow::Result<()>` (full, sleeps and powers off, as now).
  - `Panel::draw_partial(&mut self, old: &[u8; 48_000], new: &[u8; 48_000], region: Region) -> anyhow::Result<()>` and `Panel::finish(&mut self) -> anyhow::Result<()>` (deep sleep and power off), following Task 1's finding on whether the panel stays powered between partials. If RAM does not survive power-off, `draw_partial` writes `old` to `0x26` anyway; it always does.
  - Remove `show_partial_experiment` and the `partial-check` binary unless Task 1 findings say to keep them.

- [ ] **Step 1: Write failing tests** in `tests/display.rs`:
  - `full_refresh_does_not_modify_the_frame`: call `show_frame` on a copy of `pattern()` and assert it equals the original afterwards, and that the bytes sent to `0x24` equal those the current test asserts.
  - `partial_window_matches_full_ram_image`: using the `Wire` fake, take the `0x24` bytes of a full draw as the RAM image, run `show_partial` on a region, and assert the window commands (`0x44`, `0x45`, `0x4e`, `0x4f`) describe that region mirrored for the 180° mounting, and that the `0x24` data equals the RAM-image rows and byte columns inside that window, with `old` data on `0x26`. Assert the trigger value matches Task 1's findings.
  - `partial_stops_on_busy_timeout`: for each `fail_wait` in the partial sequence, assert `Err("BUSY timeout")` and no further commands after the failure.
- [ ] **Step 2: Run** `cargo test --locked --all-features` — expect FAIL on the new tests.
- [ ] **Step 3: Implement** `show_frame` as non-mutating and `show_partial` per the interfaces; share the init command list between them. Update `Panel` and the one caller in `updater.rs` and `display::run` for the new `&` signature.
- [ ] **Step 4: Run** `cargo test --locked --all-features` — expect all PASS (the original `display_uses_vendor_sequence_and_stops_on_timeout` unchanged).
- [ ] **Step 5: Build the target image** with `just firmware-build controller`; expect success.
- [ ] **Step 6: Commit** (`git commit -m "Add partial panel update and keep drawn frames intact"`).

---

### Task 4: Refresh scheduler and shared signals

**Files:**
- Modify: `firmware/sonos-desk/src/frame.rs` (replace `Refresh`)
- Test: `firmware/sonos-desk/tests/display.rs` (replace `refresh_waits_and_only_caches_displayed_frames`)

**Interfaces:**
- Produces:
  - `pub struct DisplayLink { changed: AtomicU32, activity: AtomicU32, force: AtomicU32 }` with `Default`, `bump_changed(&self)`, `bump_activity(&self)`, `bump_force(&self)` (wrapping `fetch_add(1, Release)`), and `snapshot(&self) -> Signals`.
  - `#[derive(Clone, Copy, PartialEq, Eq)] pub struct Signals { pub changed: u32, pub activity: u32, pub force: u32 }`.
  - `#[derive(Debug, PartialEq, Eq)] pub enum Action { Wait, Fetch, FullRefresh }`.
  - `pub struct Schedule` with `Default` and:
    - `update(&mut self, signals: Signals, now_ms: u64) -> Action`
    - `etag(&self) -> Option<&str>` (no longer expires)
    - `fetched(&mut self, now_ms: u64, ok: bool)` — a fetch attempt ended (`ok` is false for an HTTP or decode failure); sets the next fallback poll to `now_ms + 180_000`, except that a failure before any frame has been shown sets the next attempt to `now_ms + 10_000`
    - `partial_shown(&mut self, etag: Option<String>, now_ms: u64)` — counts a partial
    - `full_shown(&mut self, etag: Option<String>, now_ms: u64)` — resets `last_full_refresh`, partial count and any pending cleanup
  - Rules for `update`: a changed `changed` counter schedules a fetch 150 ms later, coalescing further changes into that one fetch; a changed `activity` or `changed` counter records activity; a `Fetch` returned because of a `changed` counter also arms exactly one settle fetch at that time plus 2,500 ms, whether or not the fetch later finds new pixels, and a settle fetch never arms another;
    a changed `force` counter returns `FullRefresh` at once; with a displayed frame, idle for 60 seconds and at least one partial since the last full refresh returns `FullRefresh`; with a displayed frame, `now_ms - last_full_refresh >= 86_400_000` and idle returns `FullRefresh` even when the last fetch returned 304; before any frame is shown, the first `update` returns `Fetch` immediately and later fetches wait for the retry deadline set by `fetched`, so a failing endpoint is never polled faster than every 10 seconds.

- [ ] **Step 1: Write failing tests** (gated `frame-check`) as one scenario-per-test set:
  - `schedule_coalesces_dial_bursts`: 20 `changed` bumps within 100 ms produce exactly one `Fetch`, at 150 ms after the first, then `Wait`.
  - `schedule_settles_once_even_when_pixels_are_unchanged`: a `changed` bump yields `Fetch`; after `fetched(ok = true)` with no `partial_shown` (a 304 or identical frame), `update` still returns `Fetch` 2,500 ms after the first fetch and then `Wait` after that settle fetch ends, without arming another.
  - `schedule_does_not_hammer_a_failing_startup_fetch`: with no frame shown, the first `update` returns `Fetch`; after `fetched(ok = false)`, repeated `update` calls every 100 ms return `Wait` until 10,000 ms have passed.
  - `schedule_cleans_up_only_when_idle`: after a partial, `Wait` while activity occurs; `FullRefresh` 60,000 ms after the last activity; `full_shown` returns to `Wait`; no cleanup when no partials occurred.
  - `schedule_backstop_survives_partials_and_304`: partials every few seconds for 24 hours never reset `last_full_refresh`; once 24 hours have passed and the user is idle, `FullRefresh` is returned even though every fetch was a 304; it is deferred while activity is recent.
  - `schedule_forced_refresh_is_immediate`: a `force` bump returns `FullRefresh` regardless of activity.
  - `schedule_keeps_etag_and_polls_on_failure`: `etag()` is retained across `fetched`; the fallback poll is due 180,000 ms after `fetched`.
- [ ] **Step 2: Run** `cargo test --locked --all-features schedule` — expect FAIL.
- [ ] **Step 3: Implement** `DisplayLink`, `Signals`, `Action`, `Schedule` per the interfaces, deleting `Refresh` and its test. Update the one `Refresh` user in `updater.rs` minimally so the crate builds (Task 7 rewrites it).
- [ ] **Step 4: Run** `cargo test --locked --all-features` — expect all PASS.
- [ ] **Step 5: Commit** (`git commit -m "Schedule display fetches, settle checks and idle cleanup"`).

---

### Task 5: Display wake-ups from MQTT state and results

**Files:**
- Modify: `firmware/sonos-desk/src/protocol.rs`, `firmware/sonos-desk/src/controller.rs`, `firmware/sonos-desk/src/network.rs`
- Test: `firmware/sonos-desk/tests/controls.rs`

**Interfaces:**
- Consumes: `frame::DisplayLink` (Task 4). `frame` is gated behind `frame-check`; make the new `Effect` variant and field handling compile in the plain `input-check`/`network-check` builds by keeping `DisplayLink` ownership in `network.rs`/`control_main.rs` behind `#[cfg(feature = "controller")]`, and the controller effect ungated.
- Produces:
  - `RoomState` gains `title`, `artist`, `album`, `artwork_url`, `playback_state` as `#[serde(default)] String` fields.
  - `Effect::DisplayChanged`, emitted by `Controller::handle` when an accepted state observation changes any of title, artist, album, `artwork_url`, `playback_state`, volume or mute from the last accepted one, and when a `Completed` result for this client is accepted. The controller stores the last display key (a tuple of those fields).
  - `network::apply` bumps `DisplayLink::changed` on `Effect::DisplayChanged`; `ControlIo` gains `link: Arc<DisplayLink>`.

- [ ] **Step 1: Write failing test `display_wakes_only_on_visible_changes`** in `tests/controls.rs` using the existing `step`, `snapshots` and `ready` helpers: an identical repeated state gives no `DisplayChanged`; a volume change gives one; a title-only change gives one; a `Completed` result gives one; stale or wrong-session state gives none.
- [ ] **Step 2: Run** `cargo test --locked --all-features display_wakes` — expect FAIL.
- [ ] **Step 3: Implement** the protocol fields, the stored key and `Effect::DisplayChanged` in the controller's `observe` path, and the `apply` handling in `network.rs`.
- [ ] **Step 4: Run** `cargo test --locked --all-features` — expect all PASS.
- [ ] **Step 5: Commit** (`git commit -m "Wake the display on visible state changes and results"`).

---

### Task 6: Hold Mute to force a full refresh

**Files:**
- Modify: `firmware/sonos-desk/src/inputs.rs`, `firmware/sonos-desk/src/device.rs`
- Test: `firmware/sonos-desk/tests/controls.rs`

**Interfaces:**
- Consumes: `frame::DisplayLink` (Task 4).
- Produces:
  - `InputEvent::Refresh`, emitted once when Mute has been held for 2,000 ms; Mute then does not emit `InputEvent::Mute` on release.
  - Mute now emits `InputEvent::Mute` on release when it was held under 2,000 ms; other buttons and the encoder are unchanged. `Inputs::disarm` clears the hold state.
  - `device::InputSink` gains `link: Arc<DisplayLink>`; `run_control_inputs` calls `link.bump_activity()` for every event, `link.bump_force()` for `Refresh`, and never sends `Refresh` to the controller channel.

- [ ] **Step 1: Write failing test `mute_acts_on_release_and_long_hold_requests_refresh`**: a 100 ms Mute press yields `Mute` only at release; a 2,100 ms hold yields `Refresh` at 2,000 ms and nothing at release; after `disarm` during a hold nothing fires. Update the existing `buttons_require_stable_press_and_release` expectations for Mute accordingly.
- [ ] **Step 2: Run** `cargo test --locked --all-features mute_acts` — expect FAIL.
- [ ] **Step 3: Implement** the hold tracking in `Inputs::sample` using the existing `Edge::Rising`/`Edge::Falling` detector output, plus the device-side handling.
- [ ] **Step 4: Run** `cargo test --locked --all-features` — expect all PASS.
- [ ] **Step 5: Commit** (`git commit -m "Hold Mute to force a full display refresh"`).

---

### Task 7: Display worker rewrite, wiring, docs and hardware acceptance (depends on Tasks 1–6)

**Files:**
- Modify: `firmware/sonos-desk/src/updater.rs`, `firmware/sonos-desk/src/control_main.rs`, `firmware/sonos-desk/README.md`, `AGENTS.md`

**Interfaces:**
- Consumes: `Schedule`, `Action`, `DisplayLink` (Task 4); `changed_region`, `Region` (Task 2); `Panel::draw`, `Panel::draw_partial`, `Panel::finish` (Task 3); `frame_http::fetch_connected(&Config, Option<&str>) -> anyhow::Result<Response>` (existing).
- Produces: `updater::run(panel: Panel, config: Config, ready: Arc<AtomicU32>, link: Arc<DisplayLink>)`; `control_main.rs` creates one `Arc<DisplayLink>` and passes it to the network thread (`ControlIo.link`), the input loop (`InputSink.link`) and the display thread.

- [ ] **Step 1: Rewrite the worker loop** as: poll every 100 ms; skip while `ready == 0`; `schedule.update(link.snapshot(), now)`; on `Fetch` call `fetch_connected(&config, schedule.etag())`, treat `None` PNG as no change, decode otherwise, compare with the cached displayed frame using `changed_region`; no region means no panel traffic; a region means `draw_partial(old, new, region)` then `partial_shown`; no cached frame means a full `draw` then `full_shown`; always call `schedule.fetched(now, ok)` after an attempt, with `ok` false on HTTP or decode failure; on `FullRefresh` call `draw` on the cached frame, then `finish` and `full_shown`. Call `finish` after the idle cleanup. Replace the cached frame only after the panel call succeeds. Keep the existing messages style (`Display: ...`) and the park-forever behavior on panel error.
- [ ] **Step 2: Check heap headroom.** Log free heap (`esp_idf_svc::sys::esp_get_free_heap_size`) before the first draw and after the first partial; both should leave comfortable room beside the cached 48 KB frame and the 128 KB PNG cap. If the minimum free heap is under 40 KB, stop and report to the user.
- [ ] **Step 3: Build and run host tests.** `cargo test --locked --all-features`, `cargo clippy --locked --all-features` and `just firmware-build controller`; expect all clean and success.
- [ ] **Step 4: Update docs.** In `firmware/sonos-desk/README.md` replace the "Faster partial updates require a separate panel experiment" paragraph and the three-minute statements with the new behavior, the Mute-hold gesture and the spike results; update the `AGENTS.md` firmware summary sentence about the three-minute check.
- [ ] **Step 5: Flash and test on hardware** (`just firmware-flash controller`, USB monitor closed first). With the user: turn the dial and confirm the volume bar follows; press next and confirm title and art change without a full flash; press play/pause; let the screen idle 60 seconds and confirm one cleanup refresh; hold Mute 2 seconds and confirm a full refresh without a mute toggle; confirm MQTT controls keep working throughout. Record observed latencies and any ghosting.
- [ ] **Step 6: Commit** (`git commit -m "Refresh the display live with partial updates"`), noting hardware results in the README as the earlier chunks did.
