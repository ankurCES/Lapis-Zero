"""Health, pairing, paired devices and multi-turn chats."""

from __future__ import annotations

import socket
import time
from typing import Any

from aiohttp import web

from lapis_daemon import __version__
from lapis_daemon.agent.harness import LAPIS

# What `PATCH /v1/chats/{id}` can change; any other key of its body is ignored.
PATCHABLE = frozenset(
    ("title", "host_id", "policy_override", "model_override", "archived", "agent", "workdir")
)


def _core(request: web.Request) -> Any:
    from lapis_daemon.api.server import CORE

    return request.app[CORE]


def _device(request: web.Request) -> dict[str, Any]:
    from lapis_daemon.api.server import DEVICE

    return request[DEVICE]


async def _decorate(core: Any, chat: dict[str, Any]) -> dict[str, Any]:
    """Add live fields: is the agent running here, how many approvals wait."""
    task = core.runner.active_task
    chat["active"] = bool(task and task.get("chat_id") == chat["id"])
    chat["step"] = task["step"] if chat["active"] else None
    chat["pending_approvals"] = sum(
        1 for r in core.runner.pending.values() if r.get("chat_id") == chat["id"]
    )
    return chat


async def health(request: web.Request) -> web.Response:
    core = _core(request)
    return web.json_response(
        {
            "ok": True,
            "version": __version__,
            "hostname": socket.gethostname(),
            "uptime_sec": int(time.time() - core.started),
            "mock": core.mock,
            "device_id": _device(request)["id"],
        }
    )


async def pair_exchange(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body, error

    core = _core(request)
    if core.auth.locked_out:
        return error(429, "locked_out", "Too many failed pairing attempts; wait a minute")
    data = await body(request)
    paired = await core.pair_exchange(
        str(data.get("nonce", "")),
        str(data.get("device_name", "Android")),
        str(data.get("device_model", "")),
    )
    if paired is None:
        return error(403, "bad_nonce", "Pairing code is invalid, already used, or expired")
    return web.json_response(
        {"device_id": paired[0], "api_token": paired[1], "hostname": socket.gethostname()}
    )


async def devices(request: web.Request) -> web.Response:
    me = _device(request)["id"]
    rows = await _core(request).auth.devices()
    return web.json_response({"devices": [{**d, "current": d["id"] == me} for d in rows]})


async def revoke_device(request: web.Request) -> web.Response:
    if not await _core(request).auth.revoke(request.match_info["id"]):
        raise LookupError("Unknown device")
    return web.Response(status=204)


async def list_chats(request: web.Request) -> web.Response:
    core = _core(request)
    chats = await core.chats.list(
        request.query.get("host_id") or None, request.query.get("archived") == "1"
    )
    return web.json_response({"chats": [await _decorate(core, c) for c in chats]})


def _agent_fields(core: Any, data: dict[str, Any]) -> dict[str, str]:
    """`agent` and `workdir` of a chat body, checked; only the ones that were sent."""
    out = {}
    if "agent" in data:
        valid = [LAPIS, *core.agents()]
        if data["agent"] not in valid:
            raise ValueError(f"Unknown agent: use one of {', '.join(valid)}")
        out["agent"] = data["agent"]
    if "workdir" in data:
        workdir = data["workdir"]
        if not isinstance(workdir, str) or len(workdir) > 500 or not workdir.isprintable():
            raise ValueError("workdir must be a folder path of at most 500 printable characters")
        out["workdir"] = workdir.strip()
    return out


async def create_chat(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    core = _core(request)
    data = await body(request)
    host_id = str(data.get("host_id") or core.active_host)
    if host_id not in core.hosts.hosts:
        raise ValueError(f"Unknown host: {host_id or '(none configured)'}")
    policy = data.get("policy_override")
    if policy not in (None, "strict", "balanced", "autonomous"):
        raise ValueError("policy_override must be strict, balanced or autonomous")
    chat = await core.chats.create(host_id, data.get("title"), policy, **_agent_fields(core, data))
    return web.json_response(await _decorate(core, chat), status=201)


async def _chat_or_404(request: web.Request) -> dict[str, Any]:
    chat = await _core(request).chats.get(request.match_info["chat_id"])
    if chat is None:
        raise LookupError("Unknown chat")
    return chat


async def get_chat(request: web.Request) -> web.Response:
    core = _core(request)
    chat = await _chat_or_404(request)
    before = request.query.get("before")
    messages, has_more = await core.chats.messages(
        chat["id"], int(before) if before else None, int(request.query.get("limit", 50))
    )
    return web.json_response(
        {"chat": await _decorate(core, chat), "messages": messages, "has_more": has_more}
    )


async def patch_chat(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    core = _core(request)
    chat = await _chat_or_404(request)
    data = await body(request)
    if "host_id" in data and data["host_id"] not in core.hosts.hosts:
        raise ValueError(f"Unknown host: {data['host_id']}")
    if data.get("policy_override") not in (None, "strict", "balanced", "autonomous"):
        raise ValueError("policy_override must be strict, balanced or autonomous")
    if "title" in data:
        if not (isinstance(data["title"], str) and data["title"].strip()):
            raise ValueError("title must be a non-empty string")
        data["title"] = data["title"].strip()[:120]
    data.update(_agent_fields(core, data))
    fields = {k: v for k, v in data.items() if k in PATCHABLE}
    updated = await core.chats.update(chat["id"], **fields)
    return web.json_response(await _decorate(core, updated))


async def delete_chat(request: web.Request) -> web.Response:
    core = _core(request)
    chat = await _chat_or_404(request)
    task = core.runner.active_task
    if task and task.get("chat_id") == chat["id"]:
        await core.kill_all(f"android:{_device(request)['name']}")
    await core.chats.delete(chat["id"])
    return web.Response(status=204)


async def post_message(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    core = _core(request)
    chat = await _chat_or_404(request)
    if request.content_type.startswith("multipart/"):
        content = await _transcribe_upload(core, request)
    else:
        content = str((await body(request)).get("content", ""))
    by = f"android:{_device(request)['name']}"
    _, message, task_id = await core.submit_prompt(content, chat_id=chat["id"], source=by)
    return web.json_response({"message": message, "task_id": task_id}, status=202)


async def _transcribe_upload(core: Any, request: web.Request) -> str:
    """`audio` part (16 kHz mono WAV/PCM) -> text via the configured STT endpoint."""
    import asyncio

    from lapis_daemon.voice import FakeTranscriber, HttpTranscriber, Transcriber

    voice = core.config.voice
    transcriber: Transcriber
    if core.mock:
        transcriber = FakeTranscriber()
    elif voice.stt_api_url:
        transcriber = HttpTranscriber(
            voice.stt_api_url,
            voice.stt_model,
            voice.sample_rate,
            voice.channels,
            voice.stt_api_key_env,
            voice.stt_timeout_sec,
        )
    else:
        raise ValueError("Voice transcription is not configured on this device")
    audio = b""
    async for part in await request.multipart():
        if part.name == "audio":
            audio = bytes(await part.read(decode=False))
    if not audio:
        raise ValueError("Missing `audio` part")
    return await asyncio.to_thread(transcriber.transcribe, audio)


async def stop_chat(request: web.Request) -> web.Response:
    core = _core(request)
    chat = await _chat_or_404(request)
    task = core.runner.active_task
    running = bool(task and task.get("chat_id") == chat["id"])
    if running:
        await core.kill_all(f"android:{_device(request)['name']}")
    return web.json_response({"stopped": running})


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/health", health)
    r.add_post("/v1/pair/exchange", pair_exchange)
    r.add_get("/v1/devices", devices)
    r.add_delete("/v1/devices/{id}", revoke_device)
    r.add_get("/v1/chats", list_chats)
    r.add_post("/v1/chats", create_chat)
    r.add_get("/v1/chats/{chat_id}", get_chat)
    r.add_patch("/v1/chats/{chat_id}", patch_chat)
    r.add_delete("/v1/chats/{chat_id}", delete_chat)
    r.add_post("/v1/chats/{chat_id}/messages", post_message)
    r.add_post("/v1/chats/{chat_id}/stop", stop_chat)
