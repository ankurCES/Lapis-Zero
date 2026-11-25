"""The daemon's single owner of canonical state.

Both transports, the on-device Unix socket (lapis-ui) and the remote REST/WS
control plane (the Android app), are thin adapters over this object and receive
every event through `publish`. That is what makes an approval, a kill or a host
switch on one side show up on the other immediately: there is only one state.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import secrets
import socket
import time
from collections.abc import Callable
from datetime import datetime
from typing import Any
from zoneinfo import ZoneInfo

from lapis_daemon import __version__
from lapis_daemon.agent import harness
from lapis_daemon.agent.runner import AgentJob, MockPlanner, TaskRunner
from lapis_daemon.agent.scheduler import CronError, CronScheduler, Schedule, next_run, parse_cron
from lapis_daemon.api.auth import Auth
from lapis_daemon.ble_prov.network_ctl import NetworkCtl
from lapis_daemon.chat_manager import ChatManager
from lapis_daemon.config import (
    CoreConfig,
    CronSchedule,
    EffectivePaths,
    HostsConfig,
    mask_secret,
    read_env_file,
    save_toml,
    write_env_file,
)
from lapis_daemon.db import LapisDb
from lapis_daemon.frames import FrameStore
from lapis_daemon.kvm import device_key
from lapis_daemon.kvm.base import HostDriver, MockHostDriver, build_host_driver, ssh_setup_hint
from lapis_daemon.llm.base import build_planner
from lapis_daemon.system_status import SystemStatus, run_cmd

Sink = Callable[[dict[str, Any]], None]
# Each refresh spawns `nmcli` and `tailscale`: poll quickly only while the
# device is still getting online, then back off to spare the battery.
STATUS_REFRESH_SETUP_SEC = 15.0
STATUS_REFRESH_STEADY_SEC = 60.0
QR_TTL_SEC = 180
BLE_WINDOW_SEC = 300
BLE_ERROR_TTL_SEC = 120  # how long "Bluetooth unavailable" stays on the pairing screen
# Backstop for a PIN view whose BLE process died without saying "off" (it lives at
# most RuntimeMaxSec=960 s and every status event re-arms this): the LCD must not
# show a dead PIN, with the backlight held on, for good.
BLE_VIEW_BACKSTOP_SEC = 1020
SCREEN_TIMEOUT_SEC = 2.5
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


def _log(message: str) -> None:
    """One journal line per pairing / network transition (no PIN, key or password)."""
    print(f"lapis-daemon: {message}", flush=True)


def _ble_view(status: str) -> dict[str, Any]:
    """PIN-mode pairing frame with no PIN yet: lapis-ui draws `--- ---` and `status` as sent."""
    return {"type": "pairing", "mode": "pin", "pin": "", "device_name": "", "status": status}


class Core:
    def __init__(
        self,
        paths: EffectivePaths,
        config: CoreConfig,
        hosts: HostsConfig,
        db: LapisDb,
        *,
        mock: bool = False,
        dev_advertise: str | None = None,
        step_delay: float | None = None,
    ) -> None:
        self.paths = paths
        self.config = config
        self.hosts = hosts
        self.db = db
        self.mock = mock
        self.dev_advertise = dev_advertise
        self.started = time.time()
        self.status = SystemStatus(mock=mock, api_key_env=config.llm.api_key_env)
        self.net = NetworkCtl(port=config.api.port)
        self.auth = Auth(db)
        self.chats = ChatManager(db, self.publish)
        self.frames = FrameStore(db, paths.frames_dir, config.api.frames_cap_mb * 1024 * 1024)
        self.sinks: list[tuple[Sink, frozenset[str]]] = []
        self.active_host = next(iter(hosts.hosts), "")
        self.pairing: dict[str, Any] | None = None
        self.voice: Any = None
        self.ui = {
            "theme": config.ui.theme,
            "brightness": config.ui.brightness,
            "mute_audio": config.ui.mute_audio,
            "timezone": config.ui.timezone or system_timezone(),
        }
        self.runner = TaskRunner(
            db,
            hosts,
            emit_state=self.broadcast_state,
            emit_event=self._on_event,
            emit_approval=self.publish,
            emit_toast=self.toast,
            emit_resolved=self.publish,
            emit_finish=self._on_finish,
            driver_factory=self._driver,
            planner_factory=self._planner,
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
        self._screen_waiters: dict[str, asyncio.Future[bytes]] = {}
        self._pairing_timer: asyncio.Task[Any] | None = None
        self._served = False  # tailscale serve confirmed for the current tailnet session
        # One authorisation at a time: two at once would both look for the key on the
        # host, both miss it, and both add it.
        self._authorizing = asyncio.Lock()
        self._file_secrets: set[str] = set()

    # -- lifecycle -------------------------------------------------------------

    async def start(self) -> None:
        self.reload_secrets(broadcast=False)
        self._tasks = [
            asyncio.create_task(self._chat_writer()),
            asyncio.create_task(self._status_loop()),
        ]
        self._reschedule()
        self.scheduler.start()

    async def stop(self) -> None:
        self.scheduler.stop()
        if self.runner.is_running:
            await self.runner.kill_all()
        await self._chat_q.join()
        for task in [*self._tasks, self._pairing_timer]:
            if task is not None:
                task.cancel()
        await self.net.stop()

    # -- event fan-out ---------------------------------------------------------

    def add_sink(self, sink: Sink, skip: frozenset[str] = frozenset()) -> None:
        self.sinks.append((sink, skip))

    def publish(self, msg: dict[str, Any]) -> None:
        for sink, skip in self.sinks:
            if msg.get("type") not in skip:
                with contextlib.suppress(Exception):  # one bad client must not starve the rest
                    sink(msg)

    def toast(self, level: str, text: str) -> None:
        self.publish({"type": "toast", "level": level, "text": text})

    def broadcast_state(self) -> None:
        self.publish(self.state())

    def state(self) -> dict[str, Any]:
        """Canonical snapshot: SPEC 6.1 `state_sync` plus the control-plane superset."""
        task = self.runner.active_task
        return {
            "type": "state_sync",
            "agent_state": self.runner.agent_state,
            # The activity kind of the step being executed right now; "" when none is.
            "action": self.runner.action,
            "active_host": self.active_host,
            "hosts": list(self.hosts.hosts),
            "active_task": (
                {k: task[k] for k in ("id", "title", "step", "max_steps")} if task else None
            ),
            "active_chat_id": task.get("chat_id") if task else None,
            "next_cron": self._next_cron(),
            **self.status.snapshot(),
            "utc_offset_min": utc_offset_minutes(self.ui["timezone"]),
            "pending_approvals": list(self.runner.pending.values()),
            "pairing": self.pairing,
            "ui": dict(self.ui),
            "device": {
                "hostname": socket.gethostname(),
                "version": __version__,
                "mock": self.mock,
            },
        }

    async def _status_loop(self) -> None:
        while True:
            before = self.status.snapshot()
            with contextlib.suppress(Exception):
                await self.status.refresh()
            if self.status.snapshot() != before:
                self.broadcast_state()
            state = self.status.tailscale["state"]
            if state != before["tailscale_state"] and not self.mock:
                _log(f"tailscale {before['tailscale_state']} -> {state}")
            online = state == "connected"
            if not online:
                self._served = False
            elif not self._served and not self.mock:
                # Whoever brought the node up (the BLE wizard, a seeded key, a login
                # finished late): publishing the control plane must not depend on
                # one of their short-lived watchers still being alive.
                with contextlib.suppress(Exception):
                    self._served = await self.net.tailscale_serve()
                    _log(f"tailscale serve: {'active' if self._served else 'nothing stored'}")
            await asyncio.sleep(STATUS_REFRESH_STEADY_SEC if online else STATUS_REFRESH_SETUP_SEC)

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

    def _planner(self) -> Any:
        return MockPlanner() if self.mock else build_planner(self.config.llm)

    def select_host(self, host: str) -> bool:
        if host not in self.hosts.hosts:
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
            if target not in self.hosts.hosts:
                raise ValueError(
                    "No target host is configured yet" if not target else f"Unknown host: {target}"
                )
            chat = await self.chats.device_chat(target)
        if chat["host_id"] not in self.hosts.hosts:
            raise ValueError(f"Unknown host: {chat['host_id']}")
        # A chat on the phone may be bound to an agent tool on its host. Prompts typed or
        # spoken on the handheld and schedules always go to the device's own agent.
        job = None
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
            job = AgentJob(
                tool,
                await self.chats.agent_session(chat["id"]),
                chat["workdir"],
                self.config.kvm.agent_run_timeout_sec,
                lambda session, chat_id=chat["id"]: self.chats.set_agent_session(
                    chat_id, session, **bound
                ),
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

    # -- system ----------------------------------------------------------------

    async def system_control(self, command: str) -> None:
        if self.mock:
            self.toast("info", f"(mock) {command} scheduled")
            return
        if command == "retry_tailscale":
            self.net.tailscale_up()
            self.toast("info", "Tailscale: connecting...")
            asyncio.create_task(self._watch_tailscale())
        elif command in ("reboot", "shutdown"):
            self.toast("warn", "Rebooting..." if command == "reboot" else "Shutting down...")
            await asyncio.sleep(0.5)
            await run_cmd("systemctl", "reboot" if command == "reboot" else "poweroff")
        elif command == "restart_daemon":
            await run_cmd("systemctl", "--no-block", "restart", "lapis-daemon.service")
        else:
            raise ValueError(f"Unknown system command: {command}")

    async def _watch_tailscale(self) -> None:
        """Surface the login URL quickly, then the connected state."""
        for _ in range(100):
            await asyncio.sleep(3)
            before = self.status.snapshot()
            await self.status.refresh()
            if self.status.snapshot() != before:
                self.broadcast_state()
            if self.status.tailscale["state"] == "connected":
                await self.net.tailscale_serve()
                return

    def reload_secrets(self, broadcast: bool = True) -> None:
        """Mirror secrets.env into this process's environment (drivers read os.environ)."""
        values = read_env_file(self.paths.secrets)
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
        self.status.api_key_env = new.llm.api_key_env
        self.runner.max_steps = new.llm.max_steps
        self.frames.cap_bytes = new.api.frames_cap_mb * 1024 * 1024
        self.ui.update(
            theme=new.ui.theme,
            brightness=new.ui.brightness,
            mute_audio=new.ui.mute_audio,
            timezone=new.ui.timezone or system_timezone(),
        )
        self._reschedule()
        self.publish({"type": "ui_settings", **self.ui})
        self.broadcast_state()

    def apply_hosts(self, new: HostsConfig) -> None:
        save_toml(self.paths.hosts, new)
        # Mutate in place: the runner and the transports hold this same object.
        self.hosts.hosts.clear()
        self.hosts.hosts.update(new.hosts)
        if self.active_host not in self.hosts.hosts:
            self.active_host = next(iter(self.hosts.hosts), "")
        self._reschedule()
        self.broadcast_state()

    async def set_ui(
        self,
        theme: str | None = None,
        brightness: int | None = None,
        mute_audio: bool | None = None,
        wake: bool = False,
        timezone: str | None = None,
    ) -> dict[str, Any]:
        before = dict(self.ui)
        if theme is not None:
            self.config.ui.theme = self.ui["theme"] = theme  # type: ignore[assignment]
        if brightness is not None:
            self.config.ui.brightness = self.ui["brightness"] = max(0, min(int(brightness), 100))
        if mute_audio is not None:
            self.config.ui.mute_audio = self.ui["mute_audio"] = bool(mute_audio)
        if timezone is not None and timezone != self.ui["timezone"]:
            ZoneInfo(timezone)  # ZoneInfoNotFoundError (a KeyError) for an unknown zone
            if not self.mock:
                await run_cmd("timedatectl", "set-timezone", timezone)
            self.config.ui.timezone = self.ui["timezone"] = timezone
        if self.ui != before:  # a slider drag must not rewrite the SD card per event
            with contextlib.suppress(OSError):  # a read-only config dir must not block it
                save_toml(self.paths.config, self.config)
        self.publish({"type": "ui_settings", **self.ui, "wake": bool(wake)})
        self.broadcast_state()
        return dict(self.ui)

    async def screen_png(self) -> bytes:
        """Ask lapis-ui for its current 320x170 frame (ANDROID_COMPANION_SPEC 5.2)."""
        request_id = secrets.token_hex(4)
        fut: asyncio.Future[bytes] = asyncio.get_running_loop().create_future()
        self._screen_waiters[request_id] = fut
        self.publish({"type": "screen_request", "id": request_id})
        try:
            return await asyncio.wait_for(fut, SCREEN_TIMEOUT_SEC)
        finally:
            self._screen_waiters.pop(request_id, None)

    def on_screen_snapshot(self, request_id: str, png: bytes) -> None:
        fut = self._screen_waiters.get(request_id)
        if fut is not None and not fut.done():
            fut.set_result(png)

    # -- pairing ---------------------------------------------------------------

    def _set_pairing(self, pairing: dict[str, Any] | None, ttl: float = 0) -> None:
        self.pairing = pairing
        if self._pairing_timer is not None:
            self._pairing_timer.cancel()
            self._pairing_timer = None
        if pairing is not None and ttl:
            self._pairing_timer = asyncio.create_task(self._expire_pairing(ttl))
        self.publish(pairing or {"type": "pairing", "mode": "off"})
        self.broadcast_state()

    async def _expire_pairing(self, ttl: float) -> None:
        await asyncio.sleep(ttl)
        self._pairing_timer = None
        await self.pairing_stop()

    async def pairing_start(self, mode: str = "auto") -> dict[str, Any]:
        """QR when the tailnet is up (instant), otherwise the BLE PIN window."""
        fqdn = self.dev_advertise or self.status.tailscale["fqdn"]
        online = self.status.tailscale["state"] == "connected" and bool(fqdn)
        # A QR code promises https://<fqdn>/ works: only once serve is confirmed.
        served = self._served or self.mock or bool(self.dev_advertise)
        if mode == "auto":
            mode = "qr" if online and served else "ble"
        if mode == "qr":
            if not online:
                raise ValueError("Tailscale is not connected; pair over Bluetooth instead")
            if not served:
                raise ValueError("Tailscale HTTPS is not active yet; pair over Bluetooth instead")
            nonce = self.auth.new_nonce()
            pin = f"{secrets.randbelow(10**6):06d}"
            payload = f"lapis://pair?host={fqdn}&nonce={nonce}&pin={pin}"
            if self.dev_advertise:
                payload += "&tls=0"  # development daemon: plain HTTP, no tailscale serve
            self._set_pairing(
                {
                    "type": "pairing",
                    "mode": "qr",
                    "payload": payload,
                    "host": fqdn,
                    "pin": pin,
                    "expires_in": QR_TTL_SEC,
                    "expires_at": int(time.time()) + QR_TTL_SEC,
                },
                QR_TTL_SEC,
            )
        elif self.mock:
            self._set_pairing(
                {
                    "type": "pairing",
                    "mode": "pin",
                    "pin": f"{secrets.randbelow(10**6):06d}",
                    "device_name": "LapisZero-MOCK",
                    "status": "waiting",
                    "expires_in": BLE_WINDOW_SEC,
                    "expires_at": int(time.time()) + BLE_WINDOW_SEC,
                },
                BLE_WINDOW_SEC,
            )
        else:
            # The BLE service owns the PIN; it reports in over the Unix socket.
            # reset-failed: an explicit request must not be refused by an old start-limit hit.
            await run_cmd("systemctl", "reset-failed", "lapis-ble-prov.service")
            rc, _ = await run_cmd("systemctl", "--no-block", "start", "lapis-ble-prov.service")
            _log(f"pairing: Bluetooth window requested (systemctl exit {rc})")
            if rc:
                self._ble_unavailable(f"cannot start lapis-ble-prov (systemctl exit {rc})")
            elif not (self.pairing and self.pairing.get("mode") == "pin" and self.pairing["pin"]):
                # Only a view that shows a real PIN is left alone. A stale "Bluetooth
                # unavailable" view is replaced, which also re-arms its timer: left in
                # place, that timer would stop the unit this request just started.
                self._set_pairing(_ble_view("Starting Bluetooth"), BLE_WINDOW_SEC)
        return self.pairing or {"type": "pairing", "mode": "starting"}

    async def pairing_stop(self) -> None:
        self.auth.cancel_nonces()
        if not self.mock and self.pairing is not None and self.pairing.get("mode") == "pin":
            await run_cmd("systemctl", "--no-block", "stop", "lapis-ble-prov.service")
        self._set_pairing(None)

    def on_pairing_event(self, msg: dict[str, Any]) -> None:
        """Status from the lapis-ble-prov process (PIN to display, progress, teardown)."""
        if msg.get("mode") in (None, "off"):
            if msg.get("error"):
                self._ble_unavailable(str(msg["error"]))
            elif self.pairing is not None and self.pairing.get("mode") == "pin":
                # (a late "off" from a process being stopped must not close a QR view)
                self._set_pairing(None)
            return
        allowed = ("mode", "pin", "device_name", "status", "expires_in", "expires_at")
        self._set_pairing(
            {"type": "pairing", **{k: msg[k] for k in allowed if k in msg}}, BLE_VIEW_BACKSTOP_SEC
        )

    def _ble_unavailable(self, reason: str) -> None:
        """Say why on the pairing screen: never a PIN that cannot work, never silence."""
        _log(f"pairing: Bluetooth unavailable: {reason[:120]}")
        self.toast("warn", f"Bluetooth unavailable: {reason}"[:120])
        view = {**_ble_view("Bluetooth unavailable"), "error": reason[:120]}
        self._set_pairing(view, BLE_ERROR_TTL_SEC)

    async def pair_exchange(self, nonce: str, name: str, model: str) -> tuple[str, str] | None:
        if not self.auth.consume_nonce(nonce):
            return None
        device_id, token = await self.auth.register(name, model)
        await self.pairing_stop()
        self.toast("info", f"Paired: {name[:24] or 'Android'}")
        return device_id, token

    # -- schedules -------------------------------------------------------------

    def _reschedule(self) -> None:
        cron = self.config.cron
        self.scheduler.set_schedules(
            [
                Schedule(s.name, s.schedule, s.host, s.prompt)
                for s in cron.schedules
                if cron.enabled and s.enabled and s.host in self.hosts.hosts
            ]
        )

    def _next_cron(self) -> str:
        upcoming = []
        for s in self.config.cron.schedules:
            if self.config.cron.enabled and s.enabled and s.host in self.hosts.hosts:
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
        if schedule.host not in self.hosts.hosts:
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

    # -- SSH set-up and the agent tools on a host ------------------------------

    async def device_ssh_key(self) -> dict[str, str]:
        """The public half of the device's own SSH key (made on first use)."""
        return await asyncio.to_thread(device_key.public_key, self.paths.ssh_key)

    async def _logged_in(self, host_id: str) -> Any:
        """A driver with an open SSH session, or SshUnavailable saying what to do."""
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
            return await driver.find_agents(self.agents())
        except Exception as exc:  # noqa: BLE001 - whatever broke, the owner gets a sentence
            why = str(exc) or type(exc).__name__
            raise SshUnavailable(f"Could not look for agent tools on {host_id}: {why}") from exc
        finally:
            await driver.disconnect()

    async def authorize_ssh(self, host_id: str) -> dict[str, Any]:
        """Put the device's key on a host, then prove it by logging in with that key alone."""
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
