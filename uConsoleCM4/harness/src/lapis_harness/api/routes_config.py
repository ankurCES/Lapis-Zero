"""lapis.toml, hosts.toml, write-only secrets and schedules."""

from __future__ import annotations

from typing import Any

from aiohttp import web

from lapis_harness.agent.runner import Busy
from lapis_harness.config import (
    DECK_HOST,
    SECRET_NAME_RE,
    CoreConfig,
    CronSchedule,
    HostsConfig,
    deck_host,
)


def _core(request: web.Request) -> Any:
    from lapis_harness.api.server import CORE

    return request.app[CORE]


async def get_config(request: web.Request) -> web.Response:
    # CoreConfig holds only the *names* of secret env vars, never their values.
    return web.json_response({"config": _core(request).config.model_dump()})


async def put_config(request: web.Request) -> web.Response:
    from lapis_harness.api.server import body

    core = _core(request)
    new = CoreConfig.model_validate(_keeping_config((await body(request)).get("config"), core))
    core.apply_config(new)
    return web.json_response({"config": core.config.model_dump()})


# The body of a PUT is the whole object, and an app from before a setting existed reads
# it, drops the keys it does not know, and writes the rest back. For the settings that
# came later a key that is not sent therefore means "as it is", not "back to the
# default": to clear one, send it empty.


def _keeping_config(data: Any, core: Any) -> Any:
    """`agents`, `kvm.agent_run_timeout_sec` and an agent's `tiers` stay when not sent."""
    if not isinstance(data, dict):
        return data
    stored = core.config.model_dump()
    data.setdefault("agents", stored["agents"])
    kvm = data.setdefault("kvm", {})
    if isinstance(kvm, dict):
        kvm.setdefault("agent_run_timeout_sec", stored["kvm"]["agent_run_timeout_sec"])
    if isinstance(data["agents"], dict):
        for agent_id, entry in data["agents"].items():
            was = stored["agents"].get(agent_id)
            # The owner vouched for that command line, not for another one.
            if isinstance(entry, dict) and was and entry.get("command") == was["command"]:
                entry.setdefault("tiers", was["tiers"])
    return data


def _keeping_hosts(data: Any, core: Any) -> Any:
    """A host's `ssh.password_env` stays when not sent."""
    if not isinstance(data, dict):
        return data
    for host_id, entry in data.items():
        was = core.hosts.hosts.get(host_id)
        ssh = entry.setdefault("ssh", {}) if isinstance(entry, dict) else None
        if was is not None and isinstance(ssh, dict):
            ssh.setdefault("password_env", was.ssh.password_env)
    return data


def _hosts_payload(core: Any) -> dict[str, Any]:
    """The fleet hosts, then the built-in `deck` (the uConsole itself; never stored)."""
    hosts = {name: h.model_dump() for name, h in core.hosts.hosts.items()}
    hosts[DECK_HOST] = {**deck_host().model_dump(), "builtin": True}
    return {"hosts": hosts, "active_host": core.active_host}


async def get_hosts(request: web.Request) -> web.Response:
    return web.json_response(_hosts_payload(_core(request)))


async def put_hosts(request: web.Request) -> web.Response:
    from lapis_harness.api.server import body

    core = _core(request)
    if core.runner.is_running:
        raise Busy("Agent busy: stop the running task before editing hosts")
    hosts = _keeping_hosts((await body(request)).get("hosts"), core)
    if isinstance(hosts, dict):
        hosts.pop(DECK_HOST, None)  # an app sending the list back: the deck stays built in
    new = HostsConfig.model_validate({"hosts": hosts})
    for name in new.hosts:
        if not SECRET_NAME_RE.match(name.replace("-", "_")):
            raise ValueError(f"Invalid host id: {name!r}")
    core.apply_hosts(new)
    return web.json_response(_hosts_payload(core))


async def probe(request: web.Request) -> web.Response:
    return web.json_response(await _core(request).probe_host(_known_host(request)))


def _known_host(request: web.Request) -> str:
    host_id = request.match_info["host_id"]
    if not _core(request).is_host(host_id):
        raise LookupError("Unknown host")
    return host_id


async def host_agents(request: web.Request) -> web.Response:
    """The agent tools (Claude Code, opencode, ...) installed on a host."""
    from lapis_harness.api.server import error
    from lapis_harness.core import SshUnavailable

    try:
        agents = await _core(request).host_agents(_known_host(request))
    except SshUnavailable as exc:
        return error(409, "ssh_unavailable", str(exc))
    return web.json_response({"agents": agents})


async def authorize_ssh(request: web.Request) -> web.Response:
    """Add the device's own key to the host's authorized_keys and prove that it works."""
    from lapis_harness.api.server import error
    from lapis_harness.core import SshUnavailable

    try:
        return web.json_response(await _core(request).authorize_ssh(_known_host(request)))
    except SshUnavailable as exc:
        return error(409, "ssh_unavailable", str(exc))


async def secrets_status(request: web.Request) -> web.Response:
    return web.json_response({"secrets": _core(request).secrets_status()})


async def patch_secrets(request: web.Request) -> web.Response:
    from lapis_harness.api.server import body

    core = _core(request)
    data = await body(request)
    to_set, to_unset = data.get("set") or {}, data.get("unset") or []
    if not isinstance(to_set, dict) or not isinstance(to_unset, list):
        raise ValueError("`set` must be an object and `unset` a list")
    for name, value in to_set.items():
        # Neither refusal repeats the name: it may be the key or password itself,
        # swapped with its name.
        if not SECRET_NAME_RE.fullmatch(str(name)):
            raise ValueError(
                "Invalid secret name: a name is letters, digits and _ and does not "
                "start with a digit"
            )
        if not isinstance(value, str) or not value:
            raise ValueError("Invalid secret: its value must be a non-empty string")
    core.patch_secrets({str(k): v for k, v in to_set.items()}, [str(n) for n in to_unset])
    # The response carries presence and masked tails only, like GET.
    return web.json_response({"secrets": core.secrets_status()})


async def list_schedules(request: web.Request) -> web.Response:
    return web.json_response({"schedules": await _core(request).schedules()})


async def create_schedule(request: web.Request) -> web.Response:
    from lapis_harness.api.server import body

    core = _core(request)
    schedule = CronSchedule.model_validate(await body(request))
    if any(s.name == schedule.name for s in core.config.cron.schedules):
        raise ValueError(f"A schedule named {schedule.name!r} already exists")
    core.save_schedule(schedule)
    return web.json_response({"schedules": await core.schedules()}, status=201)


async def patch_schedule(request: web.Request) -> web.Response:
    from lapis_harness.api.server import body

    core = _core(request)
    name = request.match_info["name"]
    current = next((s for s in core.config.cron.schedules if s.name == name), None)
    if current is None:
        raise LookupError("Unknown schedule")
    merged = CronSchedule.model_validate({**current.model_dump(), **await body(request)})
    core.save_schedule(merged, replace=name)
    return web.json_response({"schedules": await core.schedules()})


async def delete_schedule(request: web.Request) -> web.Response:
    if not _core(request).delete_schedule(request.match_info["name"]):
        raise LookupError("Unknown schedule")
    return web.Response(status=204)


async def run_schedule(request: web.Request) -> web.Response:
    chat_id, task_id = await _core(request).run_schedule(request.match_info["name"])
    return web.json_response({"chat_id": chat_id, "task_id": task_id}, status=202)


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/config", get_config)
    r.add_put("/v1/config", put_config)
    r.add_get("/v1/hosts", get_hosts)
    r.add_put("/v1/hosts", put_hosts)
    r.add_post("/v1/hosts/{host_id}/probe", probe)
    r.add_get("/v1/hosts/{host_id}/agents", host_agents)
    r.add_post("/v1/hosts/{host_id}/ssh/authorize", authorize_ssh)
    r.add_get("/v1/secrets/status", secrets_status)
    r.add_patch("/v1/secrets", patch_secrets)
    r.add_get("/v1/schedules", list_schedules)
    r.add_post("/v1/schedules", create_schedule)
    r.add_patch("/v1/schedules/{name}", patch_schedule)
    r.add_delete("/v1/schedules/{name}", delete_schedule)
    r.add_post("/v1/schedules/{name}/run", run_schedule)
