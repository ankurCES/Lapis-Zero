"""HostDriver interface and the Phase 2 MockHostDriver (SPEC 7.2)."""

from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import Awaitable, Callable
from typing import Any

EmitFn = Callable[[str, str, str], Awaitable[None]]


class HostDriver(ABC):
    """Async KVM surface for one target host."""

    mock: bool = False

    @abstractmethod
    async def connect(self) -> None: ...

    @abstractmethod
    async def disconnect(self) -> None: ...

    @abstractmethod
    async def run_ssh(self, command: str, timeout_s: float) -> str: ...

    @abstractmethod
    async def screenshot(self) -> None:
        """Placeholder until the Phase 3 VNC screenshot pipeline."""

    @abstractmethod
    async def click(self, x: int, y: int, button: str, clicks: int) -> None: ...

    @abstractmethod
    async def type_text(self, text: str) -> None: ...

    @abstractmethod
    async def key_press(self, combo: str) -> None: ...

    @abstractmethod
    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None: ...


CANNED_SSH = {
    "uname": "Linux mock 6.12.0 aarch64 GNU/Linux",
    "df": "disk usage: 42%",
    "systemctl": "worker.service: active (running)",
}
SSH_OUTPUT_TRUNCATE = 4096


class MockHostDriver(HostDriver):
    """Records actions instead of touching a real host."""

    mock = True

    def __init__(self, name: str, emit: EmitFn | None = None) -> None:
        self.name = name
        self.is_connected = False
        self.log: list[dict[str, str]] = []
        self.emit = emit

    async def _record(self, kind: str, summary: str, detail: str = "") -> None:
        self.log.append({"kind": kind, "summary": summary, "detail": detail})
        if self.emit is not None:
            await self.emit(kind, summary, detail)

    async def connect(self) -> None:
        self.is_connected = True

    async def disconnect(self) -> None:
        self.is_connected = False

    async def run_ssh(self, command: str, timeout_s: float) -> str:
        words = command.split()
        output = CANNED_SSH.get(words[0] if words else "", "ok")[:SSH_OUTPUT_TRUNCATE]
        await self._record("ssh_exec", command, output)
        return output

    async def screenshot(self) -> None:
        await self._record("screenshot", "screenshot()")

    async def click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> None:
        await self._record("vnc_click", f"click({x}, {y}) {button}")

    async def type_text(self, text: str) -> None:
        await self._record("type_text", f"type_text({text!r})")

    async def key_press(self, combo: str) -> None:
        await self._record("key_press", f"key_press({combo})")

    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        await self._record("scroll", f"scroll({x}, {y}, {dx}, {dy})")

    def result_lines(self) -> list[str]:
        """One `kind: summary` line per recorded action."""
        return [f"{a['kind']}: {a['summary']}" for a in self.log]


class CompositeHostDriver(HostDriver):
    """VNC for GUI actions + SSH for command execution on one host.

    Accepts factory callables so a missing credentials file never forces an import of
    `asyncvnc`/`asyncssh`; both drivers are built lazily on first use.
    """

    def __init__(
        self,
        name: str,
        host,
        *,
        vnc_factory,
        ssh_factory,
        emit: EmitFn | None = None,
    ) -> None:
        self.name = name
        self.host = host
        self.emit = emit
        self._vnc_factory = vnc_factory
        self._ssh_factory = ssh_factory
        self._vnc = None
        self._ssh = None

    @property
    def vnc(self):
        if self._vnc is None:
            self._vnc = self._vnc_factory()
        return self._vnc

    @property
    def ssh(self):
        if self._ssh is None:
            self._ssh = self._ssh_factory()
        return self._ssh

    async def connect(self) -> None:
        await self.vnc.connect()
        await self.ssh.connect()

    async def disconnect(self) -> None:
        if self._vnc is not None:
            await self._vnc.disconnect()
        if self._ssh is not None:
            await self._ssh.disconnect()

    async def run_ssh(self, command: str, timeout_s: float) -> str:
        return await self.ssh.run(command, timeout_s if timeout_s else None)

    async def screenshot(self) -> Any:
        return await self.vnc.screenshot()

    async def click(self, x: int, y: int, button: str, clicks: int) -> None:
        await self.vnc.click(x, y, button, clicks)

    async def type_text(self, text: str) -> None:
        await self.vnc.type_text(text)

    async def key_press(self, combo: str) -> None:
        await self.vnc.key_press(combo)

    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        await self.vnc.scroll(x, y, dx, dy)


def build_host_driver(name: str, host, config, emit: EmitFn | None = None) -> HostDriver:
    """Real Option C factory: VNC for GUI, SSH for commands (SPEC 7.2)."""
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
            idle_disconnect_sec=kvm.vnc_idle_disconnect_sec,
            emit=emit,
        ),
        ssh_factory=lambda: SshDriver(
            host,
            timeout_s=kvm.ssh_timeout_sec,
            truncate=kvm.ssh_output_truncate,
            emit=emit,
        ),
        emit=emit,
    )
