"""HTTP contract for the thin e-ink client."""

import logging
from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.responses import JSONResponse
from starlette.concurrency import run_in_threadpool

from sonos_desk.render import Renderer
from sonos_desk.routes import create_router
from sonos_desk.sonos import BackendError, SonosBackend


def create_app(backend=None, renderer=None):
    backend = backend or SonosBackend()
    renderer = renderer or Renderer()

    @asynccontextmanager
    async def lifespan(app):
        try:
            try:
                await run_in_threadpool(backend.rooms)
            except BackendError:
                logging.getLogger(__name__).warning("Startup speaker inventory unavailable")
            yield
        finally:
            await run_in_threadpool(renderer.close)

    application = FastAPI(title="Sonos Desk", lifespan=lifespan, docs_url=None, redoc_url=None)

    @application.exception_handler(BackendError)
    async def backend_error(request, exc):
        return JSONResponse(status_code=exc.status, content={"detail": str(exc)}, headers={"Cache-Control": "no-store"})

    application.include_router(create_router(backend, renderer))

    return application


app = create_app()
