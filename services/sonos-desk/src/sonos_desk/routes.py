"""HTTP routes and command validation for the e-ink and browser clients."""

from hashlib import sha256
from importlib.resources import files
from typing import Literal

from fastapi import APIRouter, Query, Request, Response
from fastapi.responses import HTMLResponse
from pydantic import Field

from sonos_desk.commands import CommandFields


class Command(CommandFields):
    room: str = Field(min_length=1, max_length=256)


def create_router(backend, renderer, mqtt_status):
    router = APIRouter()

    @router.get("/", response_class=HTMLResponse, include_in_schema=False)
    def browser():
        html = files("sonos_desk").joinpath("templates/browser.html").read_text(encoding="utf-8")
        return HTMLResponse(html, headers={"Cache-Control": "no-cache"})

    @router.get("/health")
    def health():
        return backend.health()

    @router.get("/mqtt/status")
    def mqtt_status_route(response: Response):
        response.headers["Cache-Control"] = "no-store"
        return mqtt_status()

    @router.get("/rooms")
    def rooms(response: Response):
        response.headers["Cache-Control"] = "no-store"
        return backend.rooms()

    @router.get("/state")
    def state(response: Response, room: str = Query(min_length=1, max_length=256)):
        response.headers["Cache-Control"] = "no-store"
        return backend.state(room)

    @router.post("/command")
    def command(payload: Command, response: Response):
        response.headers["Cache-Control"] = "no-store"
        return backend.command(payload.room, payload.command, payload.value)

    @router.get("/frame")
    def frame(
        request: Request, room: str = Query(min_length=1, max_length=256), mode: Literal["gray4", "mono"] = "gray4"
    ):
        png = renderer.frame(backend.state(room), mode)
        etag = '"' + sha256(png).hexdigest() + '"'
        headers = {"ETag": etag, "Cache-Control": "no-cache"}
        tags = [tag.strip().removeprefix("W/") for tag in request.headers.get("if-none-match", "").split(",")]
        if "*" in tags or etag in tags:
            return Response(status_code=304, headers=headers)
        return Response(png, media_type="image/png", headers=headers)

    return router
