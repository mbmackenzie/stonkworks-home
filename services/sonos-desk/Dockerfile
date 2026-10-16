FROM python:3.13-slim-bookworm

COPY --from=ghcr.io/astral-sh/uv:0.13.1 /uv /uvx /bin/

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    UV_PYTHON_DOWNLOADS=never \
    PATH="/app/.venv/bin:$PATH"

RUN apt-get update \
    && apt-get install -y --no-install-recommends fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 sonos \
    && useradd --uid 10001 --gid sonos --no-create-home sonos

WORKDIR /app
COPY pyproject.toml uv.lock ./
COPY services/sonos-desk/pyproject.toml services/sonos-desk/README.md ./services/sonos-desk/
COPY services/sonos-desk/src ./services/sonos-desk/src
RUN uv sync --frozen --no-dev --no-editable --package sonos-desk

USER sonos
CMD ["uvicorn", "sonos_desk.app:app", "--host", "127.0.0.1", "--port", "8008", "--workers", "1", "--no-proxy-headers"]
