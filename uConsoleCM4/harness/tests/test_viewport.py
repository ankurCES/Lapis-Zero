"""The HUD's live view of a fleet host: `/v1/hosts/{id}/screen.jpg` and `/input`, gated."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from conftest import wait_for
from fakedaemon import FakeDaemon, closed_port_url
from test_api import HUD, PHONE, client_for, ws_until

from lapis_harness.daemon_client import DaemonClient
from lapis_harness.gate import DaemonGate
from lapis_harness.kvm.base import MockHostDriver


class Logged(MockHostDriver):
    """A mock desktop that keeps every driver ever made, for what was done on it."""

    made: list[Logged] = []

    def __init__(self, name: str) -> None:
        super().__init__(name)
        Logged.made.append(self)

    def done(self) -> list[str]:
        return [line for d in Logged.made for line in d.result_lines()]


class Unreachable(MockHostDriver):
    async def screenshot(self) -> Any:
        raise ConnectionError("No route to host")

    async def click(self, *args: Any) -> None:
        raise ConnectionError("No route to host")


async def test_a_fresh_frame_with_the_local_gate(short_tmp: Path) -> None:
    Logged.made = []
    core, client = await client_for(short_tmp)
    core.runner.driver_factory = Logged
    resp = await client.get("/v1/hosts/macbook-pro/screen.jpg", headers=HUD)
    assert resp.status == 200 and resp.content_type == "image/jpeg"
    assert (resp.headers["X-Lapis-Width"], resp.headers["X-Lapis-Height"]) == ("1280", "720")
    assert resp.headers["Cache-Control"] == "no-store"
    jpeg = await resp.read()
    assert jpeg[:3] == b"\xff\xd8\xff"
    # Kept in the frame ring like every other screenshot.
    shot = resp.headers["X-Lapis-Screenshot-Id"]
    stored = await client.get(f"/v1/screenshots/{shot}.jpg", headers=PHONE)
    assert stored.status == 200 and await stored.read() == jpeg
    # One driver for the view, kept for the next frame.
    await client.get("/v1/hosts/macbook-pro/screen.jpg", headers=HUD)
    assert len(Logged.made) == 1 and Logged.made[0].done().count("screenshot: screenshot()") == 2

    assert (await client.get("/v1/hosts/nope/screen.jpg", headers=HUD)).status == 404
    no_screen = await client.get("/v1/hosts/deck/screen.jpg", headers=HUD)
    assert no_screen.status == 409 and (await no_screen.json())["error"]["code"] == "no_screen"
    core.runner.driver_factory = Unreachable
    down = await client.get("/v1/hosts/arch-dev/screen.jpg", headers=HUD)
    body = await down.json()
    assert down.status == 503 and body["error"]["code"] == "host_unreachable"
    assert body["error"]["message"] == "Cannot reach arch-dev: No route to host"
    await client.close()
    await core.stop()


async def test_input_with_the_local_gate_runs_only_what_was_allowed(short_tmp: Path) -> None:
    Logged.made = []
    core, client = await client_for(short_tmp)
    core.runner.driver_factory = Logged
    dev = await client.ws_connect("/v1/ws", headers=HUD)
    path = "/v1/hosts/macbook-pro/input"
    # Balanced host: a plain click runs at once.
    resp = await client.post(path, json={"action": "click", "x": 10, "y": 20}, headers=HUD)
    assert (resp.status, await resp.json()) == (200, {"ok": True, "decision": "auto", "tier": 0})
    for body in (
        {"action": "move", "x": 1, "y": 2},
        {"action": "scroll", "x": 1, "y": 2, "dx": 0, "dy": 3},
        {"action": "key", "combo": "cmd+space"},
    ):
        assert (await (await client.post(path, json=body, headers=HUD)).json())["ok"] is True
    assert Logged.made[0].done() == [
        "vnc_click: click(10, 20) left",
        "mouse_move: move(1, 2)",
        "scroll: scroll(1, 2, 0, 3)",
        "key_press: key_press(cmd+space)",
    ]
    typed = await client.post(path, json={"action": "type", "text": "make test"}, headers=HUD)
    assert (await typed.json())["ok"] is True
    assert Logged.made[0].done()[-1] == "type_text: type_text('make test')"
    # Strict host: every input waits for the owner. Approved on the device, then typed...
    strict = "/v1/hosts/arch-dev/input"
    typing = asyncio.create_task(
        client.post(strict, json={"action": "type", "text": "ls"}, headers=HUD)
    )
    asked = (await ws_until(dev, "approval_request"))[-1]
    assert (asked["host"], asked["action"], asked["summary"]) == (
        "arch-dev",
        "vnc_type",
        "type (manual)",
    )
    assert core.decide(asked["request_id"], "approve_once", "device")
    assert (await (await typing).json())["decision"] == "approve_once"
    on_strict = next(d for d in Logged.made if d.name == "arch-dev")
    assert on_strict.result_lines() == ["type_text: type_text('ls')"]
    # ...or denied on the phone, and never clicked.
    clicking = asyncio.create_task(
        client.post(strict, json={"action": "click", "x": 5, "y": 5}, headers=PHONE)
    )
    asked = (await ws_until(dev, "approval_request"))[-1]
    assert core.decide(asked["request_id"], "deny", "android:Pixel", "not now")
    denied = await (await clicking).json()
    assert denied == {"ok": False, "decision": "deny", "tier": 1, "error": "not now"}
    assert on_strict.result_lines() == ["type_text: type_text('ls')"]
    # The hard denylist holds for typed text too.
    resp = await client.post(
        path, json={"action": "type", "text": "sudo mkfs /dev/sda"}, headers=HUD
    )
    assert (await resp.json())["decision"] == "policy_deny"
    for bad in ({"action": "paste"}, {"action": "click", "x": "1"}, {"action": "type", "text": 3},
                {"action": "click", "button": "thumb"}):  # fmt: skip
        assert (await client.post(path, json=bad, headers=HUD)).status == 422, bad
    assert (await client.post("/v1/hosts/nope/input", json={}, headers=HUD)).status == 404

    # It works while a task runs there too (the nudge needs the agent paused).
    chat = await (await client.post("/v1/chats", json={"host_id": "macbook-pro"},
                                    headers=PHONE)).json()  # fmt: skip
    await client.post(f"/v1/chats/{chat['id']}/messages", json={"content": "go"}, headers=PHONE)
    await wait_for(lambda: core.runner.is_running)
    resp = await client.post(path, json={"action": "move", "x": 7, "y": 7}, headers=HUD)
    assert (await resp.json())["ok"] is True
    await core.kill_all()
    await dev.close()
    await client.close()
    await core.stop()


async def test_frames_and_input_through_the_daemons_gate(short_tmp: Path) -> None:
    Logged.made = []
    async with FakeDaemon(short_tmp) as daemon:

        async def no_typing(body: dict[str, Any]) -> dict[str, Any]:
            if body["tool"] == "type_text":
                return {"allowed": False, "decision": "deny", "tier": 1, "reason": "owner: no",
                        "by": "device"}  # fmt: skip
            return {"allowed": True, "decision": "auto", "tier": 0, "reason": "", "by": "policy"}

        daemon.authorize = no_typing
        gate = DaemonGate(daemon.client())
        core, client = await client_for(short_tmp, gate=gate)
        core.runner.driver_factory = Logged
        assert (await client.get("/v1/hosts/arch-dev/screen.jpg", headers=HUD)).status == 200
        path = "/v1/hosts/arch-dev/input"
        ok = await (await client.post(path, json={"action": "click", "x": 3, "y": 4},
                                      headers=HUD)).json()  # fmt: skip
        assert ok == {"ok": True, "decision": "auto", "tier": 0}
        no = await (await client.post(path, json={"action": "type", "text": "rm -rf ~"},
                                      headers=PHONE)).json()  # fmt: skip
        assert no == {"ok": False, "decision": "deny", "tier": 1, "error": "owner: no"}
        # Asked with the runner's tool names, the host's policy, and who asked.
        assert [(b["tool"], b["host"], b["host_policy"], b["source"]) for b in daemon.asked] == [
            ("screenshot", "arch-dev", "strict", "hud"),
            ("mouse_click", "arch-dev", "strict", "hud"),
            ("type_text", "arch-dev", "strict", "android"),
        ]
        assert daemon.asked[1]["click_x"] == 3 and daemon.asked[2]["command"] == "rm -rf ~"
        assert Logged.made[0].done() == ["screenshot: screenshot()", "vnc_click: click(3, 4) left"]

        # Nobody to ask: no frame, no input (fail closed).
        core.gate = core.runner.gate = DaemonGate(
            DaemonClient(closed_port_url(), daemon.token_file)
        )
        refused = await client.get("/v1/hosts/arch-dev/screen.jpg", headers=HUD)
        assert refused.status == 403 and (await refused.json())["error"]["code"] == "denied"
        click = await (await client.post(path, json={"action": "click"}, headers=HUD)).json()
        assert click["ok"] is False and click["decision"] == "deny"
        assert len(Logged.made[0].done()) == 2
        await core.gate.client.close()  # type: ignore[attr-defined]
        await gate.client.close()
        await client.close()
        await core.stop()
