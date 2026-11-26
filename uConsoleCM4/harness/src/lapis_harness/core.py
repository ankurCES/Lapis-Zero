"""The harness's single owner of agent state: chats, the task runner, hosts, schedules.

The harness serves the Lapis-Zero REST/WS API on a Unix socket behind the Rust
`lapis-daemon`, which authenticates every caller (the HUD, paired phones), owns the
Policy Gate and the device itself, and lays its own fields (Wi-Fi, Tailscale, battery,
pairing, UI, device) over this object's `state_sync`. Every event reaches the daemon
through `publish`, redacted, and the daemon fans it out.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import socket
import time
from collections.abc import Callable
from datetime import datetime
from typing import Any
from zoneinfo import ZoneInfo

from lapis_harness import __version__, deck
from lapis_harness.agent import harness
from lapis_harness.agent.runner import AgentJob, MockPlanner, TaskRunner
from lapis_harness.agent.scheduler import CronError, CronScheduler, Schedule, next_run, parse_cron
from lapis_harness.chat_manager import ChatManager
from lapis_harness.config import (
    DECK_HOST,
    CoreConfig,
    CronSchedule,
    EffectivePaths,
    HostConfig,
    HostsConfig,
    deck_host,
    mask_secret,
    read_env_file,
    save_toml,
    write_env_file,
)
from lapis_harness.db import LapisDb
from lapis_harness.frames import FrameStore
from lapis_harness.gate import ActionRequest, Gate, LocalGate, Outcome
from lapis_harness.kvm import device_key
from lapis_harness.kvm.base import (
    Frame,
    HostDriver,
    MockHostDriver,
    build_host_driver,
    ssh_setup_hint,
)
from lapis_harness.llm.base import Planner, build_planner
from lapis_harness.proc import run_cmd
from lapis_harness.redact import redact_json, register_secret

Sink = Callable[[dict[str, Any]], None]
VIEWPORT_TIMEOUT_SEC = 30.0  # one live-view frame or input on a fleet host
MOCK_STEP_DELAY_SEC = 0.8  # how long a mock step "takes", so ticker and screens are watchable


def system_timezone() -> str:
    """The OS timezone name, from where /etc/localtime points ('' if unknown)."""
    try:
        target = os.path.realpath("/etc/localtime")
    except OSError:
        return ""
    return target.split("zoneinfo/", 1)[1] if "zoneinfo/" in target else ""


def utc_offset_minutes(tz: str) -> int:
    """Current offset of `tz` from UTC (DST-aware); 0 for an unknown or empty zone.

    Computed from the zone database rather than the process's own local time,
    which glibc caches at start-up and would not notice a timezone change.
    """
    try:
        offset = datetime.now(ZoneInfo(tz)).utcoffset() if tz else None
    except Exception:  # noqa: BLE001 - unknown zone, missing tzdata
        offset = None
    return int(offset.total_seconds() // 60) if offset is not None else 0


class SshUnavailable(Exception):
    """The device cannot log in to a host over SSH; the message tells the owner what to do."""


class HostUnreachable(Exception):
    """A fleet host did not answer a live-view frame or an input (the API answers 503)."""


class NoScreen(Exception):
    """The host has no remote screen (the deck itself)."""


class Core:
    def __init__(
        self,
        paths: EffectivePaths,
        config: CoreConfig,
        hosts: HostsConfig,
        db: LapisDb,
        *,
        mock: bool = False,
        step_delay: float | None = None,
        gate: Gate | None = None,
        deck_tools: deck.DeckTools | None = None,
    ) -> None:
        self.paths = paths
        self.config = config
        hosts.hosts.pop(DECK_HOST, None)  # built in, never stored
        self.hosts = hosts
        self.db = db
        self.mock = mock
        self.started = time.time()
        self.chats = ChatManager(db, self.publish)
        self.frames = FrameStore(db, paths.frames_dir, config.api.frames_cap_mb * 1024 * 1024)
        self.sinks: list[tuple[Sink, frozenset[str]]] = []
        self.active_host = next(iter(hosts.hosts), DECK_HOST)
        self.voice: Any = None
        # Production passes the daemon's gate; without one, Lapis-Zero's in-process gate
        # (tests), whose approval requests go out as events like any other.
        self.gate: Gate = gate if gate is not None else LocalGate(self.publish, self.publish)
        self.deck_tools = deck_tools
        self.runner = TaskRunner(
            db,
            hosts,
            emit_state=self.broadcast_state,
            emit_event=self._on_event,
            emit_toast=self.toast,
            emit_finish=self._on_finish,
            driver_factory=self._driver,
            planner_factory=self._planner,
            deck_planner_factory=self._deck_planner,
            gate=self.gate,
            deck_tools=deck_tools,
            frames=self.frames,
            max_steps=config.llm.max_steps,
            step_delay=(MOCK_STEP_DELAY_SEC if mock else 0.0) if step_delay is None else step_delay,
            settle_delay=0.0 if mock else 0.7,
            stuck_threshold=config.kvm.stuck_threshold,
            stuck_hamming_max=config.kvm.stuck_hamming_max,
        )
        self.scheduler = CronScheduler(self._run_scheduled)
        self._chat_q: asyncio.Queue[tuple[Any, ...]] = asyncio.Queue()
        self._tasks: list[asyncio.Task[Any]] = []
        # The HUD's live view of a fleet host: one driver per host, its own session
        # (the task's driver is busy with the task), one frame or input at a time.
        self._viewports: dict[str, tuple[HostDriver, asyncio.Lock]] = {}
        self._viewport_idle: dict[str, asyncio.Task[None]] = {}
        # One authorisation at a time: two at once would both look for the key on the
        # host, both miss it, and both add it.
        self._authorizing = asyncio.Lock()
        self._file_secrets: set[str] = set()

    # -- lifecycle -------------------------------------------------------------

    async def start(self) -> None:
        self.reload_secrets(broadcast=False)
        self._tasks = [asyncio.create_task(self._chat_writer())]
        self._reschedule()
        self.scheduler.start()

    async def stop(self) -> None:
        self.scheduler.stop()
        if self.runner.is_running:
            await self.runner.kill_all()
        await self._chat_q.join()
        for task in [*self._tasks, *self._viewport_idle.values()]:
            task.cancel()
        for driver, _lock in list(self._viewports.values()):
            with contextlib.suppress(Exception):
                await driver.disconnect()
        self._viewports.clear()

    # -- event fan-out ---------------------------------------------------------

    def add_sink(self, sink: Sink, skip: frozenset[str] = frozenset()) -> None:
        self.sinks.append((sink, skip))

    def publish(self, msg: dict[str, Any]) -> None:
        """Every event the harness emits, redacted once here (AGENTS.md invariant 4)."""
        msg = redact_json(msg)
        for sink, skip in self.sinks:
            if msg.get("type") not in skip:
                with contextlib.suppress(Exception):  # one bad client must not starve the rest
                    sink(msg)

    def toast(self, level: str, text: str) -> None:
        self.publish({"type": "toast", "level": level, "text": text})

    def broadcast_state(self) -> None:
        self.publish(self.state())

    def state(self) -> dict[str, Any]:
        """The agent half of `state_sync`; the daemon adds Wi-Fi, Tailscale, battery,
        pairing, UI and device fields over it."""
        task = self.runner.active_task
        return {
            "type": "state_sync",
            "agent_state": self.runner.agent_state,
            # The activity kind of the step being executed right now; "" when none is.
            "action": self.runner.action,
            "active_host": self.active_host,
            "hosts": self.host_ids(),
            "active_task": (
                {k: task[k] for k in ("id", "title", "step", "max_steps")} if task else None
            ),
            "active_chat_id": task.get("chat_id") if task else None,
            "next_cron": self._next_cron(),
            "api_key_ready": self.api_key_ready,
            "utc_offset_min": utc_offset_minutes(self.config.ui.timezone or system_timezone()),
            "pending_approvals": list(self.runner.pending.values()),
            "device": {
                "hostname": socket.gethostname(),
                "version": __version__,
                "mock": self.mock,
            },
        }

    @property
    def api_key_ready(self) -> bool:
        return self.mock or bool(os.environ.get(self.config.llm.api_key_env))

    # -- hosts -----------------------------------------------------------------

    def host_ids(self) -> list[str]:
        """The fleet hosts, then the built-in `deck` (the uConsole itself)."""
        return [*self.hosts.hosts, DECK_HOST]

    def is_host(self, host_id: str) -> bool:
        return host_id == DECK_HOST or host_id in self.hosts.hosts

    def host_config(self, host_id: str) -> HostConfig:
        return deck_host() if host_id == DECK_HOST else self.hosts.hosts[host_id]

    # -- agent -----------------------------------------------------------------

    def _driver(self, name: str, ssh_methods: tuple[str, ...] | None = None) -> HostDriver:
        if self.mock:
            return MockHostDriver(name)
        return build_host_driver(
            name, self.hosts.hosts[name], self.config, self.paths.ssh_key, ssh_methods
        )

    def agents(self) -> dict[str, harness.Harness]:
        """Every agent tool a chat can be bound to, by id (`lapis` itself is not one)."""
        return harness.all_tools(self.config.agents)

    def _planner(self) -> Planner:
        return MockPlanner() if self.mock else build_planner(self.config.llm)

    def _deck_planner(self) -> Planner:
        return MockPlanner(deck.DECK_SCRIPT) if self.mock else build_planner(self.config.llm)

    def select_host(self, host: str) -> bool:
        if not self.is_host(host):
            return False
        self.active_host = host
        self.broadcast_state()
        return True

    async def submit_prompt(
        self, prompt: str, *, host: str | None = None, chat_id: str | None = None, source: str
    ) -> tuple[dict[str, Any], dict[str, Any], str]:
        """Run `prompt` in a chat; returns (chat, user message, task id)."""
        prompt = prompt.strip()
        if not prompt:
            raise ValueError("Prompt is empty")
        self.runner.refuse_if_busy()
        if chat_id is not None:
            chat = await self.chats.get(chat_id)
            if chat is None:
                raise LookupError("Unknown chat")
        else:
            target = host or self.active_host
            if not self.is_host(target):
                raise ValueError(
                    "No target host is configured yet" if not target else f"Unknown host: {target}"
                )
            chat = await self.chats.device_chat(target)
        if not self.is_host(chat["host_id"]):
            raise ValueError(f"Unknown host: {chat['host_id']}")
        # A chat on the phone may be bound to an agent tool on its host. Prompts typed or
        # spoken on the handheld and schedules always go to the device's own agent.
        job = None
        if chat["host_id"] == DECK_HOST and chat["agent"] != harness.LAPIS:
            raise ValueError(
                "Agent tools run on fleet hosts: pick the device's own agent for the deck chat"
            )
        if chat_id is not None and source != "schedule" and chat["agent"] != harness.LAPIS:
            tool = self.agents().get(chat["agent"])
            if tool is None:
                raise ValueError(
                    f"This chat's agent {chat['agent']!r} is no longer configured: "
                    "pick another one for the chat"
                )
            # The session the tool names belongs to this tool, folder and host: if the
            # chat is bound to others by the time the run ends, it is not kept.
            bound = {k: chat[k] for k in ("agent", "workdir", "host_id")}
            bound_chat = chat["id"]

            async def remember(session: str) -> None:
                await self.chats.set_agent_session(bound_chat, session, **bound)

            job = AgentJob(
                tool,
                await self.chats.agent_session(chat["id"]),
                chat["workdir"],
                self.config.kvm.agent_run_timeout_sec,
                remember,
                self.agents(),
            )
        history = await self.chats.history_for_llm(chat["id"])
        if chat["title"] == "New chat":
            await self.chats.update(chat["id"], title=prompt[:60])
        message = await self.chats.add_message(
            chat["id"], "user", prompt, metadata={"source": source}
        )
        task_id = await self.runner.submit_task(
            chat["host_id"],
            prompt,
            chat["policy_override"],
            chat_id=chat["id"],
            history=history,
            agent=job,
        )
        await self.db.run(
            "UPDATE chat_messages SET task_id = ? WHERE id = ?", (task_id, message["id"])
        )
        message["task_id"] = task_id
        return chat, message, task_id

    def _on_event(self, event: dict[str, Any]) -> None:
        self.publish(event)
        # A tool's progress lines ("running") are for watching, not for the thread: two
        # a second for half an hour would bury the chat and wear the SD card.
        if event.get("chat_id") and event["kind"] != "prompt" and event["status"] != "running":
            self._chat_q.put_nowait(
                (
                    event["chat_id"],
                    "tool",
                    event["summary"],
                    event["task_id"],
                    {
                        k: event[k]
                        for k in ("kind", "status", "detail", "screenshot_id", "step", "ts")
                    },
                )
            )

    def _on_finish(self, done: dict[str, Any]) -> None:
        self.publish(done)
        if done.get("chat_id"):
            text = done["summary"] or {"done": "Done.", "cancelled": "Cancelled."}.get(
                done["status"], done["status"]
            )
            self._chat_q.put_nowait(
                (done["chat_id"], "assistant", text, done["task_id"], {"status": done["status"]})
            )

    async def _chat_writer(self) -> None:
        """Single consumer: chat rows land in the order the events happened."""
        while True:
            chat_id, role, content, task_id, meta = await self._chat_q.get()
            try:
                await self.chats.add_message(chat_id, role, content, task_id, meta)
            except Exception as exc:  # noqa: BLE001
                self.toast("error", f"chat store: {exc}")
            finally:
                self._chat_q.task_done()

    def decide(self, request_id: str, decision: str, by: str = "device", reason: str = "") -> bool:
        return self.runner.resolve_approval(request_id, decision, by, reason)

    async def kill_all(self, by: str = "device") -> None:
        await self.runner.kill_all(by)

    def reload_secrets(self, broadcast: bool = True) -> None:
        """Mirror secrets.env into this process's environment (drivers read os.environ)."""
        values = read_env_file(self.paths.secrets)
        for value in values.values():
            register_secret(value)  # never in a log line or an event from here on
        for gone in self._file_secrets - set(values):
            os.environ.pop(gone, None)
        os.environ.update(values)
        self._file_secrets = set(values)
        if broadcast:
            self.broadcast_state()

    def secrets_status(self) -> list[dict[str, Any]]:
        """Presence flags and masked tails only: plaintext never leaves this process."""
        values = read_env_file(self.paths.secrets)
        names = {self.config.llm.api_key_env, self.config.voice.stt_api_key_env, *values}
        for host in self.hosts.hosts.values():
            names |= {host.vnc.password_env, host.ssh.password_env}
        return [
            {
                "name": name,
                "is_set": bool(values.get(name) or os.environ.get(name)),
                "masked": mask_secret(values.get(name) or os.environ.get(name, ""))
                if (values.get(name) or os.environ.get(name))
                else None,
            }
            for name in sorted(n for n in names if n)
        ]

    def patch_secrets(self, to_set: dict[str, str], to_unset: list[str]) -> None:
        values = read_env_file(self.paths.secrets)
        values.update(to_set)
        for name in to_unset:
            values.pop(name, None)
        write_env_file(self.paths.secrets, values)
        self.reload_secrets()

    def apply_config(self, new: CoreConfig) -> None:
        save_toml(self.paths.config, new)
        self.config = new
        self.runner.max_steps = new.llm.max_steps
        self.frames.cap_bytes = new.api.frames_cap_mb * 1024 * 1024
        self._reschedule()
        self.broadcast_state()

    def apply_hosts(self, new: HostsConfig) -> None:
        new.hosts.pop(DECK_HOST, None)  # built in, never stored
        save_toml(self.paths.hosts, new)
        # Mutate in place: the runner and the transports hold this same object.
        self.hosts.hosts.clear()
        self.hosts.hosts.update(new.hosts)
        if not self.is_host(self.active_host):
            self.active_host = next(iter(self.hosts.hosts), DECK_HOST)
        for host_id in [h for h in self._viewports if h not in self.hosts.hosts]:
            self._drop_viewport(host_id)
        self._reschedule()
        self.broadcast_state()

    # -- schedules -------------------------------------------------------------

    def _reschedule(self) -> None:
        cron = self.config.cron
        self.scheduler.set_schedules(
            [
                Schedule(s.name, s.schedule, s.host, s.prompt)
                for s in cron.schedules
                if cron.enabled and s.enabled and self.is_host(s.host)
            ]
        )

    def _next_cron(self) -> str:
        upcoming = []
        for s in self.config.cron.schedules:
            if self.config.cron.enabled and s.enabled and self.is_host(s.host):
                with contextlib.suppress(CronError):
                    upcoming.append((next_run(s.schedule), s.name))
        if not upcoming:
            return ""
        when, name = min(upcoming)
        return f"{time.strftime('%H:%M', time.localtime(when))} {name}"

    async def schedules(self) -> list[dict[str, Any]]:
        runs = {r["name"]: r for r in await self.db.run("SELECT * FROM schedule_runs")}
        out = []
        for s in self.config.cron.schedules:
            run = runs.get(s.name, {})
            upcoming = None
            if s.enabled:
                with contextlib.suppress(CronError):
                    upcoming = int(next_run(s.schedule))
            out.append(
                {
                    **s.model_dump(),
                    "next_run": upcoming,
                    "last_run_at": int(run["last_run_at"]) if run.get("last_run_at") else None,
                    "last_chat_id": run.get("last_chat_id"),
                }
            )
        return out

    def save_schedule(self, schedule: CronSchedule, replace: str | None = None) -> None:
        parse_cron(schedule.schedule)  # CronError (a ValueError) on a bad expression
        if not self.is_host(schedule.host):
            raise ValueError(f"Unknown host: {schedule.host}")
        items = [s for s in self.config.cron.schedules if s.name not in (replace, schedule.name)]
        new = self.config.model_copy(deep=True)
        new.cron.schedules = [*items, schedule]
        self.apply_config(new)

    def delete_schedule(self, name: str) -> bool:
        items = [s for s in self.config.cron.schedules if s.name != name]
        if len(items) == len(self.config.cron.schedules):
            return False
        new = self.config.model_copy(deep=True)
        new.cron.schedules = items
        self.apply_config(new)
        return True

    async def run_schedule(self, name: str) -> tuple[str, str]:
        """Run a schedule now; returns (chat id, task id)."""
        schedule = next((s for s in self.config.cron.schedules if s.name == name), None)
        if schedule is None:
            raise LookupError("Unknown schedule")
        title = f"Scheduled: {name}"
        rows = await self.db.run(
            "SELECT id FROM chats WHERE title = ? AND archived = 0"
            " ORDER BY updated_at DESC LIMIT 1",
            (title,),
        )
        chat_id = rows[0]["id"] if rows else (await self.chats.create(schedule.host, title))["id"]
        _, _, task_id = await self.submit_prompt(
            schedule.prompt, chat_id=chat_id, source="schedule"
        )
        await self.db.run(
            "INSERT OR REPLACE INTO schedule_runs(name, last_run_at, last_chat_id)"
            " VALUES (?, ?, ?)",
            (name, time.time(), chat_id),
        )
        return chat_id, task_id

    async def _run_scheduled(self, name: str, host: str, prompt: str) -> None:
        try:
            await self.run_schedule(name)
        except (ValueError, LookupError) as exc:
            self.toast("warn", f"Schedule {name}: {exc}")

    # -- host probe ------------------------------------------------------------

    async def probe_host(self, host_id: str) -> dict[str, Any]:
        """Tailscale reachability, VNC auth and SSH `echo ok`, each timed; the agent tools."""
        if host_id == DECK_HOST:
            return await self._probe_deck()
        host = self.hosts.hosts[host_id]
        if self.mock:
            ok = {"ok": True, "detail": "mock"}
            return {
                "host_id": host_id,
                "tailscale": {**ok, "latency_ms": 12},
                "vnc": {**ok, "latency_ms": 85},
                "ssh": {**ok, "latency_ms": 140},
                "agents": await self._driver(host_id).find_agents(self.agents()),
            }

        async def timed(check: Any, limit: float = 20) -> dict[str, Any]:
            start = time.monotonic()
            try:
                detail = await asyncio.wait_for(check(), limit)
                return {
                    "ok": True,
                    "latency_ms": int((time.monotonic() - start) * 1000),
                    "detail": detail,
                }
            except TimeoutError as exc:  # a bare one has no text: say what ran out
                took = str(exc) or f"did not finish within {limit:.0f} s"
                return {"ok": False, "latency_ms": None, "detail": took[:200]}
            except Exception as exc:  # noqa: BLE001 - the message is the result
                return {
                    "ok": False,
                    "latency_ms": None,
                    "detail": f"{type(exc).__name__}: {exc}"[:200],
                }

        async def ping() -> str:
            rc, out = await run_cmd(
                "tailscale", "ping", "--c", "1", "--timeout", "4s", host.tailscale_ip, timeout=8
            )
            if rc != 0:
                raise ConnectionError(out.strip().splitlines()[-1] if out.strip() else "no reply")
            return out.strip().splitlines()[-1][:120]

        async def vnc() -> str:
            driver = self._driver(host_id)
            try:
                frame = await driver.screenshot()
                return frame.note or f"{frame.width}x{frame.height}"
            finally:
                await driver.disconnect()

        shell: Any = self._driver(host_id)

        async def ssh() -> str:
            said = (await shell.run_ssh("echo ok", 10)).strip()[:80]
            return f"{said} ({shell.ssh_login})"  # how the login worked: "ok (device key)"

        try:
            tailscale, vnc_result, ssh_result = await asyncio.gather(
                # The first frame of a large desktop takes a while over the device's Wi-Fi;
                # the phone waits 60 s for this call.
                timed(ping),
                timed(vnc, 45),
                timed(ssh),
            )
            agents: list[dict[str, str]] = []
            if ssh_result["ok"]:
                # Not timed and never the probe's failure: a host whose login shell
                # misbehaves still has a working SSH.
                with contextlib.suppress(Exception):
                    agents = await asyncio.wait_for(shell.find_agents(self.agents()), 25)
        finally:
            await shell.disconnect()
        return {
            "host_id": host_id,
            "tailscale": tailscale,
            "vnc": vnc_result,
            "ssh": ssh_result,
            "agents": agents,
        }

    async def _probe_deck(self) -> dict[str, Any]:
        """The deck is this device: its tools are the daemon's, not VNC or SSH."""
        here = {"ok": True, "latency_ms": 0, "detail": "this device"}
        none = {
            "ok": False,
            "latency_ms": None,
            "detail": "the deck is driven through its own tools",
        }
        start = time.monotonic()
        try:
            if self.deck_tools is None:
                raise ConnectionError("lapis-daemon is not set up")
            tools = await asyncio.wait_for(self.deck_tools.list_tools(), 10)
            tools_result = {
                "ok": True,
                "latency_ms": int((time.monotonic() - start) * 1000),
                "detail": f"{len(tools)} cyberdeck tools",
            }
        except Exception as exc:  # noqa: BLE001 - the message is the result
            tools_result = {"ok": False, "latency_ms": None, "detail": str(exc)[:200]}
        return {
            "host_id": DECK_HOST,
            "tailscale": here,
            "vnc": none,
            "ssh": none,
            "deck": tools_result,
            "agents": [],
        }

    # -- the HUD's live view of a fleet host ------------------------------------

    def _viewport(self, host_id: str) -> tuple[HostDriver, asyncio.Lock]:
        if host_id == DECK_HOST:
            raise NoScreen("The deck has no remote screen: it is this device")
        if host_id not in self.hosts.hosts:
            raise LookupError("Unknown host")
        task = self.runner.active_task
        if self.mock and task is not None and task["host"] == host_id:
            # The mock desktop draws what the task did: show that one (it does no I/O).
            driver = self.runner.drivers.get(host_id)
            if driver is not None:
                return driver, self._viewports.setdefault(host_id, (driver, asyncio.Lock()))[1]
        if host_id not in self._viewports:
            self._viewports[host_id] = (self.runner.driver_factory(host_id), asyncio.Lock())
        return self._viewports[host_id]

    def _drop_viewport(self, host_id: str) -> None:
        held = self._viewports.pop(host_id, None)
        timer = self._viewport_idle.pop(host_id, None)
        if timer is not None:
            timer.cancel()
        if held is not None:
            asyncio.ensure_future(held[0].disconnect())

    def _touch_viewport(self, host_id: str) -> None:
        """Disconnect the live view after `kvm.vnc_idle_disconnect_sec` without use."""
        timer = self._viewport_idle.pop(host_id, None)
        if timer is not None:
            timer.cancel()

        async def idle() -> None:
            await asyncio.sleep(self.config.kvm.vnc_idle_disconnect_sec)
            self._viewport_idle.pop(host_id, None)
            self._drop_viewport(host_id)

        self._viewport_idle[host_id] = asyncio.create_task(idle())

    async def host_screen(self, host_id: str, source: str = "hud") -> tuple[int | None, Frame]:
        """A fresh frame of a fleet host, put to the gate first; (screenshot id, frame).

        PermissionError when the gate refused it, HostUnreachable when the host did not
        answer, LookupError for an unknown host, NoScreen for the deck.
        """
        driver, lock = self._viewport(host_id)
        outcome = await self.gate.authorize(
            ActionRequest(
                "screenshot",
                host_id,
                summary=f"Live view of {host_id}",
                host_policy=self.hosts.hosts[host_id].policy,
                source=source,
            )
        )
        if not outcome.allowed:
            raise PermissionError(outcome.reason or outcome.decision)
        async with lock:
            try:
                frame = await asyncio.wait_for(driver.screenshot(), VIEWPORT_TIMEOUT_SEC)
            except Exception as exc:  # noqa: BLE001 - whatever broke, the host is not there
                self._drop_viewport(host_id)
                raise HostUnreachable(_why(host_id, exc)) from exc
        self._touch_viewport(host_id)
        shot = await self.frames.save(f"live_{host_id}", 0, frame)
        return shot, frame

    async def host_input(
        self, host_id: str, action: str, args: dict[str, Any], source: str = "hud"
    ) -> Outcome:
        """One input on a fleet host's live view, whether or not a task runs there.

        Put to the gate with the runner's tool names; executed only if allowed.
        """
        driver, lock = self._viewport(host_id)
        try:
            outcome = await self.runner.manual_input(
                driver, host_id, action, args, source=source, lock=lock,
                timeout_s=VIEWPORT_TIMEOUT_SEC,
            )  # fmt: skip
        except ValueError:
            raise
        except Exception as exc:  # noqa: BLE001 - whatever broke, the host is not there
            self._drop_viewport(host_id)
            raise HostUnreachable(_why(host_id, exc)) from exc
        self._touch_viewport(host_id)
        return outcome

    # -- voice -------------------------------------------------------------------

    def send_mic(self, rms: float) -> None:
        """A live `mic_level` frame (the daemon routes it to the HUD's waveform only)."""
        self.publish({"type": "mic_level", "rms": round(rms, 4)})

    async def ptt(self, pressed: bool) -> str:
        """A push-to-talk edge from the HUD (through the daemon); returns the voice mode."""
        voice = self.voice
        if voice is None:
            if self.mock:
                self.send_mic(0.42 if pressed else 0.0)
                return "mock"
            if pressed:
                # Never invent a transcript on a real device.
                self.toast("warn", "Voice input is not configured")
            return "off"
        try:
            if pressed:
                await voice.begin_ptt()
            else:
                await voice.end_ptt()
        except Exception as exc:  # noqa: BLE001 - the owner gets a toast, the call an answer
            self.toast("error", f"voice: {exc}")
        return "on"

    # -- SSH set-up and the agent tools on a host ------------------------------

    async def device_ssh_key(self) -> dict[str, str]:
        """The public half of the device's own SSH key (made on first use)."""
        return await asyncio.to_thread(device_key.public_key, self.paths.ssh_key)

    async def _logged_in(self, host_id: str) -> Any:
        """A driver with an open SSH session, or SshUnavailable saying what to do."""
        if host_id == DECK_HOST:
            raise SshUnavailable("The deck is this device: its agent is the deck chat, not SSH")
        driver = self._driver(host_id)
        problem = await driver.ssh_problem()
        if problem:
            await driver.disconnect()
            raise SshUnavailable(ssh_setup_hint(host_id, self.hosts.hosts[host_id].os, problem))
        return driver

    async def host_agents(self, host_id: str) -> list[dict[str, str]]:
        """The agent tools installed on a host (`GET /v1/hosts/{id}/agents`)."""
        driver = await self._logged_in(host_id)
        try:
            found: list[dict[str, str]] = await driver.find_agents(self.agents())
            return found
        except Exception as exc:  # noqa: BLE001 - whatever broke, the owner gets a sentence
            why = str(exc) or type(exc).__name__
            raise SshUnavailable(f"Could not look for agent tools on {host_id}: {why}") from exc
        finally:
            await driver.disconnect()

    async def authorize_ssh(self, host_id: str) -> dict[str, Any]:
        """Put the device's key on a host, then prove it by logging in with that key alone."""
        if host_id == DECK_HOST:
            raise ValueError("The deck is this device: there is no host to authorise it on")
        if self.hosts.hosts[host_id].os == "windows":
            raise ValueError(
                "Authorising this device is only done for macOS and Linux hosts: on Windows, "
                "add the device's key to the account's authorized_keys file by hand"
            )
        if self.mock:
            return {"ok": True, "already": False, "detail": "mock: nothing was changed"}
        async with self._authorizing:
            return await self._authorize_ssh(host_id)

    async def _authorize_ssh(self, host_id: str) -> dict[str, Any]:
        key = await self.device_ssh_key()
        driver = await self._logged_in(host_id)
        how = driver.ssh_login
        try:
            # Logged in with the key itself: it is there. Otherwise add its line.
            already = how == "device key" or await driver.authorize(key["public_key"])
        except Exception as exc:  # noqa: BLE001 - whatever broke, the owner gets a sentence
            why = str(exc) or type(exc).__name__
            raise SshUnavailable(f"Logged in to {host_id} ({how}), but {why}") from exc
        finally:
            await driver.disconnect()
        proof = self._driver(host_id, ssh_methods=("device key",))
        problem = await proof.ssh_problem()
        await proof.disconnect()
        if problem:
            raise SshUnavailable(
                f"This device's key is in ~/.ssh/authorized_keys on {host_id}, but the host "
                f"still refuses it ({problem}). The home folder, ~/.ssh or the file may be "
                "writable by others, or the SSH server reads its keys from another file."
            )
        detail = (
            f"This device's key was already authorised on {host_id}."
            if already
            else f"Added this device's key on {host_id} (logged in by {how}). "
            "The key now logs in by itself."
        )
        return {"ok": True, "already": already, "detail": detail}


def _why(host_id: str, exc: BaseException) -> str:
    why = str(exc) or type(exc).__name__
    if isinstance(exc, TimeoutError):
        why = f"no answer within {VIEWPORT_TIMEOUT_SEC:g} s"
    return f"Cannot reach {host_id}: {why}"[:300]
