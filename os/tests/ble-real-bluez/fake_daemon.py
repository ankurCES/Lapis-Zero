"""Stand-in for lapis-daemon's Unix socket: prints every NDJSON line and keeps the last PIN in /tmp/pin."""
import asyncio, json, os, time
T0 = time.monotonic()
async def handle(r, w):
    while line := await r.readline():
        print(f"[daemon-sock {time.monotonic()-T0:6.2f}]", line.decode().rstrip(), flush=True)
        try:
            m = json.loads(line)
            if m.get("pin"):
                open("/tmp/pin.tmp", "w").write(m["pin"]); os.replace("/tmp/pin.tmp", "/tmp/pin")
        except Exception:
            pass
async def main():
    os.makedirs("/run/lapis", exist_ok=True)
    await asyncio.start_unix_server(handle, "/run/lapis/lapisd.sock")
    await asyncio.Event().wait()
asyncio.run(main())
