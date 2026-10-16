def main() -> None:
    import uvicorn

    uvicorn.run("sonos_desk.app:app", host="127.0.0.1", port=8008, workers=1, proxy_headers=False)
