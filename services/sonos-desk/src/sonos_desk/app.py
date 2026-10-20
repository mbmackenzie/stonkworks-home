"""HTTP contract for the thin e-ink client."""

import logging
import os
from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.responses import JSONResponse
from starlette.concurrency import run_in_threadpool

from sonos_desk.mqtt.config import load_mqtt_config
from sonos_desk.mqtt.runtime import MqttAdapter, disabled_status
from sonos_desk.render import Renderer
from sonos_desk.routes import create_router
from sonos_desk.sonos import BackendError, SonosBackend


def create_app(backend=None, renderer=None, *, mqtt_config=None):
    backend = backend or SonosBackend()
    renderer = renderer or Renderer()
    mqtt = MqttAdapter(backend, mqtt_config) if mqtt_config is not None else None

    @asynccontextmanager
    async def lifespan(app):
        try:
            try:
                await run_in_threadpool(backend.rooms)
            except BackendError:
                logging.getLogger(__name__).warning("Startup speaker inventory unavailable")
            if mqtt is not None:
                mqtt.start()
            yield
        finally:
            try:
                if mqtt is not None:
                    await run_in_threadpool(mqtt.close)
            finally:
                await run_in_threadpool(renderer.close)

    application = FastAPI(title="Sonos Desk", lifespan=lifespan, docs_url=None, redoc_url=None)

    @application.exception_handler(BackendError)
    async def backend_error(request, exc):
        return JSONResponse(status_code=exc.status, content={"detail": str(exc)}, headers={"Cache-Control": "no-store"})

    application.include_router(create_router(backend, renderer, mqtt.status if mqtt is not None else disabled_status))

    return application


app = create_app(mqtt_config=load_mqtt_config(os.environ, lambda path: path.read_text(encoding="utf-8")))
