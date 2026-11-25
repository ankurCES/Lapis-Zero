#!/usr/bin/env python3
"""Try the device's own screen-capture code against a real VNC host, from this computer.

    cd daemon && uv run python ../scripts/check_vnc_host.py --user NAME [HOST] [--port 5900]
    cd daemon && uv run python ../scripts/check_vnc_host.py --user NAME --device

It asks for the password with the terminal's hidden prompt. The password stays in this
process (and, with --device, goes to the container on its standard input): it is not
printed, not written to a file, and not put on a command line or in an environment
variable of another process.

--device runs the same code inside the device's own root filesystem (the image built by
`make build-os`, imported into Docker as `lapis-rootfs:final` by the image check) with
the device's Python and libraries, one slow CPU and a hard 160 MB of memory: the daemon
on the Cardputer has a 130 MB soft cap. HOST then defaults to this Mac as seen from the
container.

What it prints: how the login went, and for two captures the native size, the size sent
to the model, the encoding the host chose, megabytes received, seconds, and peak memory.
"""

from __future__ import annotations

import argparse
import asyncio
import getpass
import os
import pathlib
import subprocess
import sys
import time
import tracemalloc

ROOT = pathlib.Path(__file__).resolve().parent.parent
SECRET = "LAPIS_CHECK_VNC_PASSWORD"


async def check(host: str, port: int, user: str) -> int:
    from lapis_daemon.config import HostConfig, HostVnc
    from lapis_daemon.kvm.vnc_driver import VncDriver

    cfg = HostConfig(
        hostname=host,
        tailscale_ip=host,
        os="macos",
        vnc=HostVnc(port=port, username=user, password_env=SECRET),
    )
    driver = VncDriver(cfg)
    started = time.monotonic()
    try:
        await driver.connect()
    except Exception as exc:  # noqa: BLE001 - the message is the result
        took = time.monotonic() - started
        print(f"login: FAILED after {took:.1f} s: {type(exc).__name__}: {exc}")
        return 1
    print(f"login: ok in {time.monotonic() - started:.1f} s")
    try:
        for n in (1, 2):
            tracemalloc.start()
            try:
                frame = await driver.screenshot()
            except Exception as exc:  # noqa: BLE001
                print(f"capture {n}: FAILED: {type(exc).__name__}: {exc}")
                return 1
            finally:
                peak = tracemalloc.get_traced_memory()[1]
                tracemalloc.stop()
            print(
                f"capture {n}: {frame.note}; JPEG {len(frame.jpeg) / 1024:.0f} KB; "
                f"peak Python memory {peak / 1e6:.1f} MB"
            )
    finally:
        await driver.disconnect()
    try:
        import resource

        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        # Linux reports kilobytes, macOS bytes.
        per_mb = 1024 if sys.platform != "darwin" else 1024 * 1024
        print(f"process peak memory: {rss / per_mb:.0f} MB")
    except Exception:  # noqa: BLE001
        pass
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("host", nargs="?", default=None)
    ap.add_argument("--user", required=True, help="account name on the host (as in the host form)")
    ap.add_argument("--port", type=int, default=5900)
    ap.add_argument("--device", action="store_true", help="run inside the device image")
    ap.add_argument("--password-stdin", action="store_true", help=argparse.SUPPRESS)
    args = ap.parse_args()

    if args.password_stdin:  # inside the container
        password = sys.stdin.readline().rstrip("\n")
    else:
        password = getpass.getpass(f"Password for {args.user} (hidden; not stored): ")
    if not password:
        print("no password entered")
        return 2

    if args.device and not args.password_stdin:
        host = args.host or "host.docker.internal"
        print(f"running in the device image against {host}:{args.port} (1 slow CPU, 160 MB)")
        done = subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "-i",
                "--memory=160m",
                "--memory-swap=160m",
                "--cpus=0.25",
                "-v",
                f"{ROOT / 'daemon' / 'src'}:/lapis-src:ro",
                "-v",
                f"{pathlib.Path(__file__).resolve()}:/check_vnc_host.py:ro",
                "-e",
                "PYTHONPATH=/lapis-src",
                "lapis-rootfs:final",
                "/opt/lapis/venv/bin/python",
                "/check_vnc_host.py",
                host,
                "--user",
                args.user,
                "--port",
                str(args.port),
                "--password-stdin",
            ],  # fmt: skip
            input=password + "\n",
            text=True,
        )
        if done.returncode == 137:
            print("the container was killed for exceeding 160 MB")
        return done.returncode

    os.environ[SECRET] = password  # read by the driver in this process only
    del password
    try:
        return asyncio.run(check(args.host or "127.0.0.1", args.port, args.user))
    finally:
        os.environ.pop(SECRET, None)


if __name__ == "__main__":
    sys.exit(main())
