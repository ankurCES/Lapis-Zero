"""Tailscale SSH command execution with timeout and 4 KB truncation (SPEC 7.2)."""

from __future__ import annotations

from typing import Any

from lapis_daemon.config import HostConfig

TRUNCATE_MARKER = "\n… (truncated to {limit} bytes)"


def truncate_output(text: str, limit: int = 4096) -> str:
    """Combined stdout/stderr frame; keep the head and append a truncation marker."""
    if len(text) <= limit:
        return text
    return text[:limit] + TRUNCATE_MARKER.format(limit=limit)


class SshDriver:
    """Non-interactive command execution over `asyncssh`, lazily imported."""

    def __init__(
        self,
        host: HostConfig,
        timeout_s: int = 30,
        truncate: int = 4096,
        emit: Any = None,
    ) -> None:
        self.host = host
        self.timeout_s = timeout_s
        self.truncate = truncate
        self.emit = emit
        self._conn: Any = None

    async def connect(self) -> None:
        import asyncssh  # lazy: only the live driver needs the SDK

        kwargs: dict[str, Any] = {
            "host": self.host.tailscale_ip,
            "port": self.host.ssh.port,
            "username": self.host.ssh.user or None,
            "known_hosts": None,
        }
        if self.host.ssh.identity_file:
            kwargs["client_keys"] = [self.host.ssh.identity_file]
        self._conn = await asyncssh.connect(**kwargs)

    async def disconnect(self) -> None:
        if self._conn is not None:
            self._conn.close()
            await self._conn.wait_closed()
            self._conn = None

    async def run(self, command: str, timeout_s: float | None = None) -> str:
        if self._conn is None:
            raise RuntimeError("SSH not connected")
        import asyncssh

        timeout = timeout_s if timeout_s is not None else self.timeout_s
        try:
            result = await self._conn.run(command, timeout=timeout)
        except asyncssh.ProcessError as exc:
            # non-zero exit still carries what the host printed; surface it, not the traceback
            result = exc
        stdout = getattr(result, "stdout", "") or ""
        stderr = getattr(result, "stderr", "") or ""
        combined = stdout if stderr == "" else f"{stdout}\n{stderr}"
        return truncate_output(combined, self.truncate)
