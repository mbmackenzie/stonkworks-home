# Hide TRANSITIONING on the e-ink frame Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the header's transport label from changing twice on every track change, so partial refreshes redraw less text and ghost less.

**Architecture:** The server's `Renderer` remembers the last settled `playback_state` per room. While Sonos reports `TRANSITIONING` it renders that remembered label instead, for at most 10 seconds, then shows the real state. `/state`, MQTT and the browser UI are unchanged. The firmware needs no change: its frame diff simply finds fewer changed pixels.

**Tech Stack:** Python 3.13, Pillow, FastAPI, pytest, ruff, ty (the existing `services/sonos-desk` workspace).

**Spec:** [Partial refresh design](../specs/2026-10-21-sonos-desk-partial-refresh-design.md) for the goal; the idea comes from the ghosting brainstorm in this session (idea 1, "Hide TRANSITIONING"). No separate spec: the change is one bounded renderer behavior.

## Global Constraints

- Preserve HTTP behavior in `docs/sonos-desk.md` except the deliberate change documented in Task 1. `/state`, `/command`, MQTT payloads and `/health` do not change.
- Keep one application worker; the memory lives in the `Renderer` under its existing `self.lock`, in memory only, bounded to 32 rooms (the same bound as `self.frames`).
- Preserve 800 × 480 PNG frames, the mono/gray4 palettes, ETags, `format=bitmap`, and the bounded artwork and frame caches.
- Run from the repository root: `uv run --package sonos-desk pytest -q`, `uv run ruff check services/sonos-desk`, `uv run ruff format --check services/sonos-desk`, `uv run ty check`.
- Keep credentials, household addresses and device assignments out of code and docs.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Review Focus

- The very first frame requested during a transition (no settled label known) must show `TRANSITIONING`, not blank or crash (Task 1 test).
- A speaker stuck in `TRANSITIONING` must not hide the truth forever: after 10 seconds the real state shows (Task 1 test).
- Two rooms must not share memory: room B transitioning must not display room A's label (Task 1 test).
- Remembered state must not grow without bound across many rooms (Task 1 test).
- A transition that begins after a pause must show `PAUSED`, not a stale `PLAYING` from earlier (Task 1 test).

---

### Task 1: Remember the settled label and substitute it during transitions

**Files:**
- Modify: `services/sonos-desk/src/sonos_desk/render.py` (class `Renderer`, `frame`)
- Modify: `docs/sonos-desk.md` (the "Frames and polling" section)
- Test: `services/sonos-desk/tests/test_render.py`

**Interfaces:**
- Consumes: the existing `Renderer(client=None, clock=time.monotonic)`, `Renderer._put(cache, key, value, limit)`, `Renderer.frame(state, mode="gray4") -> bytes`, `Renderer.bitmap(state) -> bytes`, and `state` dicts with `room_id` and `playback_state` keys.
- Produces: `TRANSITION_HOLD_SECONDS = 10` (module constant) and `Renderer._transport(self, state) -> str`, the `playback_state` string actually drawn and used in the frame cache key. `frame()` and `bitmap()` signatures do not change.

- [ ] **Step 1: Write the failing tests** in `test_render.py`, using the existing `artwork_client` fixture and `STATE`, and a fake clock `now = [0.0]` passed as `Renderer(client=client, clock=lambda: now[0])`:
  - `test_transitioning_keeps_the_last_settled_label`: render `STATE` (PLAYING), then render `dict(STATE, title="Next", playback_state="TRANSITIONING")`; assert the bytes equal those of a fresh renderer rendering `dict(STATE, title="Next")` (PLAYING).
  - `test_first_frame_during_a_transition_shows_it`: a fresh renderer rendering `dict(STATE, playback_state="TRANSITIONING")` differs from one rendering `STATE`.
  - `test_long_transitions_are_shown_as_is`: after a settled PLAYING frame, render a TRANSITIONING state at clock 0, then again with clock at `TRANSITION_HOLD_SECONDS`; assert the second equals a fresh renderer's TRANSITIONING frame, and the first equals the PLAYING frame.
  - `test_transition_memory_is_per_room_and_follows_settling`: room `uid` settles PLAYING; `dict(STATE, room_id="other", playback_state="TRANSITIONING")` shows `TRANSITIONING` (equals a fresh renderer's); then `uid` settles `PAUSED_PLAYBACK` and a following TRANSITIONING frame for `uid` equals the `PAUSED_PLAYBACK` frame.
  - `test_transition_memory_is_bounded`: render settled frames for 40 distinct `room_id`s; assert `len(renderer.transport) <= 32`.
- [ ] **Step 2: Run** `uv run --package sonos-desk pytest -q services/sonos-desk/tests/test_render.py -k transition` — expect FAIL (`TRANSITION_HOLD_SECONDS` / `transport` missing, labels differ).
- [ ] **Step 3: Implement** in `render.py`: add `self.transport = OrderedDict()` in `Renderer.__init__`, mapping `room_id` (fall back to `room`) to `{"settled": str, "since": float | None}`; implement `_transport` per these rules, called at the top of `frame()` under the lock, with `state = dict(state, playback_state=self._transport(state))` so the cache key and the drawn label both use the result:
  - A state other than `TRANSITIONING`: store it as `settled`, clear `since`, return it.
  - `TRANSITIONING` with no stored settled label: return `TRANSITIONING`.
  - `TRANSITIONING` with a settled label: set `since` to the clock on first sight; return the settled label while `clock() - since < TRANSITION_HOLD_SECONDS`, else `TRANSITIONING`.
  - Store entries with `_put(self.transport, key, entry, 32)` so the bound matches the frame cache.
- [ ] **Step 4: Run** `uv run --package sonos-desk pytest -q` — expect all tests PASS; then `uv run ruff check services/sonos-desk`, `uv run ruff format --check services/sonos-desk` and `uv run ty check` — expect clean.
- [ ] **Step 5: Document** in `docs/sonos-desk.md` under "Frames and polling": while Sonos reports `TRANSITIONING`, frames keep the last settled transport label for up to 10 seconds (and show it as is when no label is known), so the e-ink display does not redraw its header twice per track change; `/state` still reports the real state.
- [ ] **Step 6: Commit** (`git add services/sonos-desk docs && git commit -m "Hold the settled transport label during TRANSITIONING frames"`).

---

### Task 2: Check it on the real display (requires redeploying the service)

**Files:** none changed; results go in the commit message or the final summary.

- [ ] **Step 1: Ask the user to redeploy** the Sonos Desk service from this branch (`docker build -f services/sonos-desk/Dockerfile -t stonkworks/sonos-desk:local .`, then update their stack as usual). Do not modify the household installation.
- [ ] **Step 2: Capture the controller log** with `firmware/sonos-desk/target/tools/bin/espflash monitor --skip-update-check --non-interactive` while the user presses next three times, waiting about 8 seconds between presses.
- [ ] **Step 3: Compare** the log's `Display: partial x … y …` lines for one `next` press with the earlier baseline (a wide partial `x 408..744 y 22..169`, then a label-only partial `x 624..744 y 22..36`). Expected: the label-only partial no longer follows each `next` press, except when the settled state really changes. Report the before and after counts and whether the user sees less text ghosting.
