"""Optional browser integration check; API routes are replaced, no speaker writes.

Install playwright separately, then run with a Chromium CDP endpoint:
  SONOS_CDP_URL=http://localhost:9222 python tests/browser_check.py
Set SONOS_BROWSER_URL to additionally check the deployed page using read-only GETs.
"""

import json
import os
from copy import deepcopy
from importlib.resources import files
from urllib.parse import parse_qs, urlparse

from playwright.sync_api import expect, sync_playwright

BASE_STATE = {
    "room": "Office",
    "room_id": "uid-office",
    "coordinator": "Office",
    "coordinator_id": "uid-office",
    "title": "A song",
    "artist": "An artist",
    "album": "An album",
    "artwork_url": "",
    "playback_state": "PLAYING",
    "volume": 40,
    "mute": False,
    "duration": "0:03:00",
}


def main():
    with sync_playwright() as playwright:
        browser = playwright.chromium.connect_over_cdp(os.environ["SONOS_CDP_URL"])
        context = browser.new_context(viewport={"width": 1100, "height": 950})
        page = context.new_page()
        errors, commands = [], []
        page.on("pageerror", lambda error: errors.append(str(error)))
        state = deepcopy(BASE_STATE)
        fail = [False]
        frames = [0]
        hold_state, held_state = [False], []

        def api(route):
            path = urlparse(route.request.url).path
            if path == "/":
                route.fulfill(
                    content_type="text/html",
                    body=files("sonos_desk").joinpath("templates/browser.html").read_text(encoding="utf-8"),
                )
            elif path == "/rooms":
                route.fulfill(
                    json=[{"room": "Office", "room_id": "uid-office"}, {"room": "Kitchen", "room_id": "uid-kitchen"}]
                )
            elif path == "/state":
                selected = parse_qs(urlparse(route.request.url).query)["room"][0]
                state["room"] = "Office" if selected == "uid-office" else "Kitchen"
                if hold_state[0]:
                    hold_state[0] = False
                    held_state.append((route, deepcopy(state)))
                    return
                route.fulfill(json=state)
            elif path == "/frame":
                frames[0] += 1
                # Real renderer output; no artwork downloads or live speaker calls.
                from sonos_desk.render import Renderer

                route.fulfill(content_type="image/png", body=Renderer().frame(state), headers={"ETag": '"test-frame"'})
            elif path == "/command":
                payload = route.request.post_data_json
                commands.append(payload)
                if fail[0]:
                    route.fulfill(status=503, json={"detail": "Command outcome uncertain"})
                    return
                action, value = payload["command"], payload.get("value")
                if action == "play_pause":
                    state["playback_state"] = "PAUSED_PLAYBACK" if state["playback_state"] == "PLAYING" else "PLAYING"
                elif action == "volume":
                    state["volume"] = value
                elif action == "volume_relative":
                    state["volume"] = max(0, min(100, state["volume"] + value))
                elif action == "mute":
                    state["mute"] = value
                elif action in {"next", "previous"}:
                    state["title"] = action + " song"
                route.fulfill(json=state)
            else:
                route.fulfill(status=404)

        page.route("http://sonos.test/**", api)
        try:
            page.goto("http://sonos.test/?room=office")
            expect(page.get_by_role("button", name="Pause", exact=True)).to_be_enabled()
            expect(page.locator("#volume-number")).to_have_text("40%")
            page.wait_for_function('document.querySelector("#frame").naturalWidth === 800')
            assert page.locator("#frame").evaluate("(image) => image.naturalHeight") == 480
            page.get_by_role("button", name="Next track", exact=True).click()
            expect(page.get_by_role("button", name="Next track", exact=True)).to_be_enabled()
            assert commands[-1] == {"room": "uid-office", "command": "next"}
            page.get_by_role("button", name="Previous track", exact=True).click()
            expect(page.get_by_role("button", name="Previous track", exact=True)).to_be_enabled()
            assert commands[-1]["command"] == "previous"
            page.get_by_role("button", name="Pause", exact=True).click()
            expect(page.get_by_role("button", name="Play", exact=True)).to_be_enabled()
            page.get_by_role("button", name="Play", exact=True).click()
            expect(page.get_by_role("button", name="Pause", exact=True)).to_be_enabled()
            page.get_by_role("button", name="Volume up", exact=True).click()
            expect(page.locator("#volume-number")).to_have_text("41%")
            page.get_by_role("button", name="Volume down", exact=True).click()
            expect(page.locator("#volume-number")).to_have_text("40%")
            expect(page.locator("#volume")).to_be_enabled()
            page.locator("#volume").focus()
            page.locator("#volume").press("ArrowLeft")
            expect(page.locator("#volume-number")).to_have_text("39%")
            assert commands[-1] == {"room": "uid-office", "command": "volume", "value": 39}
            page.get_by_role("button", name="Mute", exact=True).click()
            expect(page.get_by_role("button", name="Unmute", exact=True)).to_be_enabled()
            page.get_by_role("button", name="Unmute", exact=True).click()
            expect(page.get_by_role("button", name="Mute", exact=True)).to_be_enabled()
            page.wait_for_function("!refreshing && !pending")
            hold_state[0] = True
            page.evaluate("void refresh()")
            page.wait_for_timeout(100)
            assert held_state, "The poll must be held before sending a command"
            page.get_by_role("button", name="Mute", exact=True).click()
            expect(page.get_by_role("button", name="Unmute", exact=True)).to_be_enabled()
            old_request, old_state = held_state.pop()
            old_request.fulfill(json=old_state)
            page.wait_for_timeout(100)
            assert page.locator("#mute").get_attribute("aria-pressed") == "true", (
                "An older poll replaced a successful mute result"
            )
            page.get_by_role("button", name="Unmute", exact=True).click()
            expect(page.get_by_role("button", name="Mute", exact=True)).to_be_enabled()
            page.locator("#encoder").hover()
            page.mouse.wheel(0, -100)
            expect(page.locator("#volume-number")).to_have_text("40%")
            assert commands[-1] == {"room": "uid-office", "command": "volume_relative", "value": 1}
            expect(page.locator("#room")).to_be_enabled()
            page.locator("#room").select_option("uid-kitchen")
            expect(page.locator("#playback-status")).to_contain_text("Kitchen")
            expect(page.get_by_role("button", name="Next track", exact=True)).to_be_enabled()
            fail[0] = True
            count = len(commands)
            page.get_by_role("button", name="Next track", exact=True).click()
            expect(page.locator("#message")).to_contain_text("Command outcome uncertain")
            expect(page.get_by_role("button", name="Next track", exact=True)).to_be_enabled()
            assert commands[-1]["room"] == "uid-kitchen"
            page.wait_for_timeout(3500)
            assert len(commands) == count + 1, "An uncertain command must not be retried"
            page.set_viewport_size({"width": 390, "height": 844})
            assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
            assert (
                page.locator(".controls").bounding_box()["y"]
                >= page.locator(".screen").bounding_box()["y"] + page.locator(".screen").bounding_box()["height"]
            )
            print(
                json.dumps(
                    {
                        "mock_browser": "passed",
                        "commands": len(commands),
                        "frames": frames[0],
                        "mobile_width": 390,
                        "errors": errors,
                    }
                )
            )
            assert not errors

            live_url = os.getenv("SONOS_BROWSER_URL")
            if live_url:
                page.goto(live_url)
                expect(page.locator("#volume")).to_be_enabled(timeout=20000)
                page.wait_for_function('document.querySelector("#frame").naturalWidth === 800', timeout=20000)
                page.screenshot(path="/tmp/sonos-desk-work/browser-mobile.png", full_page=True)
                page.set_viewport_size({"width": 1100, "height": 950})
                page.screenshot(path="/tmp/sonos-desk-work/browser-desktop.png", full_page=True)
                print(json.dumps({"live_browser": "passed", "status": page.locator("#playback-status").inner_text()}))
        finally:
            context.close()


if __name__ == "__main__":
    main()
