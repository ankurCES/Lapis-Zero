"""Pairing-view recovery in the daemon after lapis-ble-prov reported a failure."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest
from conftest import make_core

import lapis_daemon.core as core_mod


def test_retry_is_not_stopped_by_the_stale_error_timer(
    short_tmp: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """ "Pair Android App" pressed while "Bluetooth unavailable" is still on screen.

    The error view's own expiry timer used to survive the request and stop the
    unit that had just been started.
    """
    ran: list[tuple[str, ...]] = []

    async def run_cmd(*argv: str, **_: object) -> tuple[int, str]:
        ran.append(argv)
        return 0, ""

    monkeypatch.setattr(core_mod, "run_cmd", run_cmd)
    monkeypatch.setattr(core_mod, "BLE_ERROR_TTL_SEC", 0.3)

    async def scenario() -> None:
        core = await make_core(short_tmp)
        core.mock = False  # take the real BLE branch; systemctl is the recorder above
        try:
            core.on_pairing_event({"type": "pairing_event", "mode": "off", "error": "no adapter"})
            assert core.pairing["status"] == "Bluetooth unavailable"
            assert core.pairing["error"] == "no adapter" and core.pairing["pin"] == ""
            await asyncio.sleep(0.2)
            ran.clear()
            await core.pairing_start("ble")
            await asyncio.sleep(0.25)  # the old 0.3 s error timer would come due here
            assert [c for c in ran if "stop" in c] == []
            assert ("systemctl", "reset-failed", "lapis-ble-prov.service") in ran
            assert core.pairing is not None and core.pairing["status"] == "Starting Bluetooth"

            # A real PIN on screen is never overwritten by a second request...
            core.on_pairing_event(
                {"type": "pairing_event", "mode": "pin", "pin": "482917", "status": "waiting"}
            )
            await core.pairing_start("ble")
            assert core.pairing["pin"] == "482917"
            # A PIN view always has an expiry behind it, in case its process dies silently.
            assert core._pairing_timer is not None and not core._pairing_timer.done()
            # ...and a plain "off" from the BLE process closes a BLE view only.
            core.pairing = {"type": "pairing", "mode": "qr", "payload": "lapis://pair?x"}
            core.on_pairing_event({"type": "pairing_event", "mode": "off"})
            assert core.pairing is not None and core.pairing["mode"] == "qr"
        finally:
            core.mock = True
            await core.stop()

    asyncio.run(scenario())
