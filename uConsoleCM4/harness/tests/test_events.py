"""Push-to-talk through the daemon, and redaction of every event, answer and log line."""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import pytest
from test_api import HUD, PHONE, client_for, ws_match, ws_until

from lapis_harness import log
from lapis_harness.redact import register_secret


class FakeVoice:
    def __init__(self) -> None:
        self.edges: list[str] = []

    async def begin_ptt(self) -> None:
        self.edges.append("begin")

    async def end_ptt(self) -> None:
        self.edges.append("end")
        raise RuntimeError("no speech detected")


async def test_push_to_talk_is_a_route_and_mic_levels_go_out_as_events(short_tmp: Path) -> None:
    core, client = await client_for(short_tmp)
    dev = await client.ws_connect("/v1/ws", headers=HUD)
    # --mock without audio: a fake level, so the HUD's waveform has something to draw.
    resp = await client.post("/v1/voice/ptt", json={"pressed": True}, headers=HUD)
    assert (resp.status, await resp.json()) == (200, {"ok": True, "pressed": True, "voice": "mock"})
    assert (await ws_until(dev, "mic_level"))[-1]["rms"] == 0.42
    await client.post("/v1/voice/ptt", json={"pressed": False}, headers=HUD)
    assert (await ws_until(dev, "mic_level"))[-1]["rms"] == 0.0
    for bad in ({}, {"pressed": "yes"}, {"pressed": 1}):
        assert (await client.post("/v1/voice/ptt", json=bad, headers=HUD)).status == 422
    # A real controller gets both edges; its failure is a toast, not a failed request.
    core.voice = voice = FakeVoice()
    for pressed in (True, False):
        resp = await client.post("/v1/voice/ptt", json={"pressed": pressed}, headers=HUD)
        assert (await resp.json())["voice"] == "on"
    assert voice.edges == ["begin", "end"]
    toast = (await ws_until(dev, "toast"))[-1]
    assert (toast["level"], toast["text"]) == ("error", "voice: no speech detected")
    # A real device without voice set up never invents a transcript: it says so.
    core.voice, core.mock = None, False
    resp = await client.post("/v1/voice/ptt", json={"pressed": True}, headers=HUD)
    assert (await resp.json())["voice"] == "off"
    assert (await ws_until(dev, "toast"))[-1]["text"] == "Voice input is not configured"
    core.mock = True
    await dev.close()
    await client.close()
    await core.stop()


async def test_no_secret_leaves_in_an_event_an_answer_or_a_log_line(
    short_tmp: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    core, client = await client_for(short_tmp)
    ws = await client.ws_connect("/v1/ws", headers=PHONE)
    seen: list[dict[str, Any]] = []
    core.add_sink(seen.append)
    # A loaded secret is scrubbed wherever its value shows up, not only in known formats.
    value = "plain-words-but-a-secret-7781"
    resp = await client.patch("/v1/secrets", json={"set": {"HOME_ASSISTANT_TOKEN": value}},
                              headers=PHONE)  # fmt: skip
    assert resp.status == 200
    chat = await (await client.post("/v1/chats", json={"host_id": "macbook-pro"},
                                    headers=PHONE)).json()  # fmt: skip
    leaky = (
        f"key sk-ant-api03-{'a' * 30} and {value}, auth: Bearer abcdefghijklmnop, password=hunter22"
    )
    await core.chats.add_message(chat["id"], "assistant", leaky)
    frame = await ws_match(ws, "chat_message_created", lambda m: m["chat_id"] == chat["id"])
    answer = await (await client.get(f"/v1/chats/{chat['id']}", headers=PHONE)).text()
    core.toast("info", f"token={value}")
    toast = (await ws_until(ws, "toast"))[-1]
    log.log(f"connected with {value} and sk-ant-api03-{'b' * 30}")
    printed = capsys.readouterr().out
    for text in (json.dumps(frame), answer, json.dumps(toast), json.dumps(seen), printed):
        assert value not in text and "sk-ant-api03" not in text and "hunter22" not in text, text
        assert "abcdefghijklmnop" not in text and "[REDACTED]" in text
    # What is stored stays the owner's own: only what goes out is scrubbed.
    rows = await core.db.run("SELECT content FROM chat_messages WHERE chat_id = ?", (chat["id"],))
    assert rows[-1]["content"] == leaky
    os.environ.pop("HOME_ASSISTANT_TOKEN", None)
    await ws.close()
    await client.close()
    await core.stop()


def test_library_logging_is_redacted_too(capsys: pytest.CaptureFixture[str]) -> None:
    import logging

    log.install()
    log.install()  # idempotent
    register_secret("library-logged-secret-0042")
    logging.getLogger("aiohttp.access").warning("GET /x?k=library-logged-secret-0042")
    err = capsys.readouterr().err
    root = logging.getLogger()
    for handler in [h for h in root.handlers if isinstance(h.formatter, log.RedactingFormatter)]:
        root.removeHandler(handler)  # it writes to this test's captured stderr
    assert "library-logged-secret-0042" not in err and "[REDACTED]" in err
    assert err.count("aiohttp.access") == 1
