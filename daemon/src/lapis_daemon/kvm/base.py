"""HostDriver interface, the MockHostDriver, and the real Option C composite (SPEC 7.2).

All coordinates a driver receives are in *screenshot space*: the pixel grid of the
most recent downscaled frame the model (or the phone) saw. Mapping to the host's
native resolution is the driver's job.
"""

from __future__ import annotations

import asyncio
import io
from abc import ABC, abstractmethod
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from lapis_daemon.agent import harness
from lapis_daemon.agent.harness import Answer, Harness


@dataclass
class Frame:
    """One captured, already-downscaled screenshot."""

    jpeg: bytes
    width: int
    height: int
    dhash: int | None = None
    fine: bytes | None = None  # small grayscale thumbnail for change detection
    note: str = ""  # how the capture went (native size, encoding, bytes, seconds)


class HostDriver(ABC):
    """Async KVM surface for one target host."""

    mock: bool = False
    # After a command on the host was stopped (a time limit, the kill switch): what of it
    # may still be running there, as far as the host confirmed: one of harness.LEFT_...
    left_running: str = harness.LEFT_NOTHING

    @abstractmethod
    async def connect(self) -> None: ...

    @abstractmethod
    async def disconnect(self) -> None: ...

    @abstractmethod
    async def run_ssh(self, command: str, timeout_s: float) -> str: ...

    @abstractmethod
    async def screenshot(self) -> Frame: ...

    @abstractmethod
    async def click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> None: ...

    @abstractmethod
    async def move(self, x: int, y: int) -> None: ...

    @abstractmethod
    async def drag(self, x1: int, y1: int, x2: int, y2: int) -> None: ...

    @abstractmethod
    async def type_text(self, text: str) -> None: ...

    @abstractmethod
    async def key_press(self, combo: str) -> None: ...

    @abstractmethod
    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None: ...

    def preview(self, x: int, y: int) -> str | None:
        """Base64 1-bit 120x68 crop of the last frame around (x, y), if there is one."""
        return None

    async def ssh_problem(self) -> str | None:
        """Why `run_ssh` cannot work on this host right now, or None if it can."""
        return None

    async def find_agents(self, tools: dict[str, Harness]) -> list[dict[str, str]]:
        """Which of `tools` are installed on the host: `{id, name, path}` each."""
        return []

    async def run_agent(
        self,
        tool: Harness,
        message: str,
        *,
        tier: str,
        session: str = "",
        workdir: str = "",
        timeout_s: float = 1800.0,
        on_progress: Callable[[str], None] | None = None,
    ) -> Answer:
        """Hand `message` to an agent tool installed on the host and wait for its answer.

        `tier` is the policy the tool is to be held to; it must be one of `tool.tiers`.
        """
        return Answer(f"This host cannot run {tool.name}.", is_error=True)


def ssh_setup_hint(name: str, os: str, reason: str) -> str:
    """What the owner is told when the device cannot log in to a host over SSH."""
    first = {
        "macos": "On the Mac, turn on Remote Login under System Settings > General > Sharing. ",
        "linux": "Make sure an SSH server is running on it. ",
    }.get(os, "")
    return (
        f"Cannot log in to {name} over SSH ({reason}). {first}Then enter that computer's "
        "user name and password in its host form and authorise this device there."
    )


# How long a task start waits for the host's SSH port before going on without it.
SSH_CHECK_TIMEOUT = 8.0
DETECT_TIMEOUT = 20.0  # a login shell with slow start-up files, then one `command -v` each
CANNED_SSH = {
    "uname": "Linux mock 6.12.0 aarch64 GNU/Linux",
    "df": "disk usage: 42%",
    "systemctl": "worker.service: active (running)",
}
SSH_OUTPUT_TRUNCATE = 4096
MOCK_W, MOCK_H = 1280, 720


class MockHostDriver(HostDriver):
    """Records actions instead of touching a real host; draws a fake desktop."""

    mock = True

    def __init__(self, name: str) -> None:
        self.name = name
        self.is_connected = False
        self.log: list[dict[str, str]] = []
        self._image: Any = None

    def _record(self, kind: str, summary: str, detail: str = "") -> None:
        self.log.append({"kind": kind, "summary": summary, "detail": detail})

    async def connect(self) -> None:
        self.is_connected = True

    async def disconnect(self) -> None:
        self.is_connected = False
        self._image = None

    async def run_ssh(self, command: str, timeout_s: float) -> str:
        words = command.split()
        output = CANNED_SSH.get(words[0] if words else "", "ok")[:SSH_OUTPUT_TRUNCATE]
        self._record("ssh_exec", command, output)
        return output

    async def screenshot(self) -> Frame:
        from PIL import Image, ImageDraw

        from lapis_daemon.agent.dhash import dhash_from_image

        img = Image.new("RGB", (MOCK_W, MOCK_H), (28, 34, 48))
        draw = ImageDraw.Draw(img)
        draw.rectangle((0, 0, MOCK_W, 28), fill=(232, 232, 226))
        draw.text((12, 8), f"{self.name}  -  mock desktop", fill=(11, 11, 13))
        draw.rectangle((90, 80, 1190, 640), fill=(242, 242, 236), outline=(11, 11, 13), width=2)
        draw.rectangle((90, 80, 1190, 112), fill=(45, 107, 255))
        draw.text((104, 90), "Terminal", fill=(255, 255, 255))
        for i, action in enumerate(self.log[-22:]):
            draw.text((110, 128 + i * 22), f"$ {action['summary']}"[:120], fill=(11, 11, 13))
        draw.rectangle((780, 396, 900, 428), fill=(255, 107, 43))
        draw.text((796, 406), "Retry Build", fill=(255, 255, 255))
        buf = io.BytesIO()
        img.save(buf, format="JPEG", quality=75)
        self._image = img
        self._record("screenshot", "screenshot()")
        return Frame(buf.getvalue(), MOCK_W, MOCK_H, dhash_from_image(img))

    async def click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> None:
        self._record("vnc_click", f"click({x}, {y}) {button}")

    async def move(self, x: int, y: int) -> None:
        self._record("mouse_move", f"move({x}, {y})")

    async def drag(self, x1: int, y1: int, x2: int, y2: int) -> None:
        self._record("mouse_drag", f"drag({x1}, {y1} -> {x2}, {y2})")

    async def type_text(self, text: str) -> None:
        self._record("type_text", f"type_text({text!r})")

    async def key_press(self, combo: str) -> None:
        self._record("key_press", f"key_press({combo})")

    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        self._record("scroll", f"scroll({x}, {y}, {dx}, {dy})")

    def preview(self, x: int, y: int) -> str | None:
        if self._image is None:
            return None
        from lapis_daemon.kvm.vnc_driver import preview_from_image

        return preview_from_image(self._image, x, y)

    async def find_agents(self, tools: dict[str, Harness]) -> list[dict[str, str]]:
        """The mock host has Claude Code, opencode and Freya, so the picker has something."""
        return [
            {"id": t.id, "name": t.name, "path": f"/Users/mock/.local/bin/{t.binary}"}
            for t in tools.values()
            if t.id in ("claude", "opencode", "freya")
        ]

    async def run_agent(
        self,
        tool: Harness,
        message: str,
        *,
        tier: str,
        session: str = "",
        workdir: str = "",
        timeout_s: float = 1800.0,
        on_progress: Callable[[str], None] | None = None,
    ) -> Answer:
        """A canned answer: `--mock` and the simulator show the feature without a host."""
        self._record("agent_run", f"{tool.name}: {message}"[:120], workdir)
        if on_progress is not None:
            on_progress(f"(mock) {tool.name} is working")
        said = "continued" if session else "started"
        return Answer(
            f"(mock) {tool.name} {said} a session and would now do: {message[:200]}",
            session or "mock-session-1",
        )

    def result_lines(self) -> list[str]:
        """One `kind: summary` line per recorded action."""
        return [f"{a['kind']}: {a['summary']}" for a in self.log]


class CompositeHostDriver(HostDriver):
    """VNC for GUI actions + SSH for command execution on one host.

    Both halves connect lazily, on first use: a task that only needs the shell
    never opens a VNC session (and the other way round), so a host with only one
    of the two reachable still works for the tasks that fit it.
    """

    def __init__(self, name: str, host: Any, *, vnc_factory: Any, ssh_factory: Any) -> None:
        self.name = name
        self.host = host
        self._vnc_factory = vnc_factory
        self._ssh_factory = ssh_factory
        self._vnc: Any = None
        self._ssh: Any = None

    async def _gui(self) -> Any:
        if self._vnc is None:
            self._vnc = self._vnc_factory()
        if not self._vnc.connected:
            await self._vnc.connect()
        return self._vnc

    async def connect(self) -> None:
        """Nothing eager: see the class docstring."""

    async def disconnect(self) -> None:
        if self._vnc is not None:
            await self._vnc.disconnect()
        if self._ssh is not None:
            await self._ssh.disconnect()

    async def _shell(self) -> Any:
        if self._ssh is None:
            self._ssh = self._ssh_factory()
        if not self._ssh.connected:
            await self._ssh.connect()
        return self._ssh

    async def run_ssh(self, command: str, timeout_s: float) -> str:
        return await (await self._shell()).run(command, timeout_s if timeout_s else None)

    @property
    def left_running(self) -> str:  # type: ignore[override]
        return self._ssh.left if self._ssh is not None else harness.LEFT_NOTHING

    @property
    def ssh_login(self) -> str:
        """How the open SSH session logged in ("device key", "password", ...); "" if none."""
        return self._ssh.login if self._ssh is not None and self._ssh.connected else ""

    async def authorize(self, public_key: str) -> bool:
        """Add the device's key to the host's authorized_keys; True if it was there."""
        return await (await self._shell()).authorize(public_key)

    async def find_agents(self, tools: dict[str, Harness]) -> list[dict[str, str]]:
        run = await (await self._shell()).login_shell(harness.detect_command(tools), DETECT_TIMEOUT)
        if not run.started:
            raise OSError("the login shell of the account there did not run the command")
        return harness.detected(tools, run.output)

    async def run_agent(
        self,
        tool: Harness,
        message: str,
        *,
        tier: str,
        session: str = "",
        workdir: str = "",
        timeout_s: float = 1800.0,
        on_progress: Callable[[str], None] | None = None,
    ) -> Answer:
        def progress(line: str) -> None:
            if on_progress is not None and (shown := tool.show(line)):
                on_progress(shown)

        script = harness.command(tool, message, tier=tier, session=session, workdir=workdir)
        run = await (await self._shell()).login_shell(script, timeout_s, progress)
        return harness.answer(tool, run, self.name, timeout_s)

    async def ssh_problem(self) -> str | None:
        """Log in over SSH now (the session is kept for `run_ssh`); the reason if that fails.

        ponytail: asked at every task start. A refused login answers within a second;
        a port that silently drops packets costs SSH_CHECK_TIMEOUT. Remember the
        answer per host if that wait ever matters.
        """
        try:
            if self._ssh is None:
                self._ssh = self._ssh_factory()
            if not self._ssh.connected:
                await asyncio.wait_for(self._ssh.connect(), SSH_CHECK_TIMEOUT)
        except TimeoutError:
            return "no answer on the SSH port"
        except Exception as exc:  # noqa: BLE001 - any failure means "no shell on this host"
            return f"{type(exc).__name__}: {exc}"[:200]
        return None

    async def screenshot(self) -> Frame:
        return await (await self._gui()).screenshot()

    async def click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> None:
        await (await self._gui()).click(x, y, button, clicks)

    async def move(self, x: int, y: int) -> None:
        await (await self._gui()).move(x, y)

    async def drag(self, x1: int, y1: int, x2: int, y2: int) -> None:
        await (await self._gui()).drag(x1, y1, x2, y2)

    async def type_text(self, text: str) -> None:
        await (await self._gui()).type_text(text)

    async def key_press(self, combo: str) -> None:
        await (await self._gui()).key_press(combo)

    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        await (await self._gui()).scroll(x, y, dx, dy)

    def preview(self, x: int, y: int) -> str | None:
        return self._vnc.preview(x, y) if self._vnc is not None else None


def build_host_driver(
    name: str,
    host: Any,
    config: Any,
    device_key: Path | None = None,
    ssh_methods: tuple[str, ...] | None = None,
) -> HostDriver:
    """Real Option C factory: VNC for GUI, SSH for commands (SPEC 7.2).

    `ssh_methods` narrows the SSH login to some of `ssh_driver.LOGIN_METHODS` (the
    proof after authorising uses the device key alone).
    """
    from lapis_daemon.kvm.ssh_driver import SshDriver
    from lapis_daemon.kvm.vnc_driver import VncDriver

    kvm = config.kvm
    return CompositeHostDriver(
        name,
        host,
        vnc_factory=lambda: VncDriver(
            host,
            llm_max_width=kvm.llm_max_width,
            llm_max_height=kvm.llm_max_height,
            jpeg_quality=kvm.jpeg_quality,
        ),
        ssh_factory=lambda: SshDriver(
            host,
            timeout_s=kvm.ssh_timeout_sec,
            truncate=kvm.ssh_output_truncate,
            device_key=device_key,
            **({"methods": ssh_methods} if ssh_methods else {}),
        ),
    )
