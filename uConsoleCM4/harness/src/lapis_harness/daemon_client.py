"""HTTP client for the Rust `lapis-daemon` on 127.0.0.1:8421.

The harness asks it before every fleet step (`/v1/policy/authorize`) and runs every
cyberdeck tool through it (`/v1/deck/tools/invoke`); the MCP server proxies the same two.
Both long-poll while the owner decides, so requests may take minutes. The bearer token is
the daemon's local token, read from its file on every call (the daemon may rotate it) and
registered with the redactor so it never reaches a log line.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from lapis_harness.redact import redact, register_secret

# The daemon's approvals time out after 600 s by default: a little longer than that.
LONG_POLL_TIMEOUT = 660.0
SHORT_TIMEOUT = 15.0
_LOOPBACK = {"127.0.0.1", "localhost", "::1"}


class DaemonUnavailable(Exception):
    """The daemon could not be asked (not running, no token, a broken answer)."""


def check_daemon_url(url: str) -> str:
    """`url` without a trailing slash; ValueError unless it is plain HTTP on loopback.

    The local token goes with every request, so it must never leave the device.
    """
    parts = urlsplit(url)
    if parts.scheme != "http" or parts.hostname not in _LOOPBACK or parts.path not in ("", "/"):
        raise ValueError("the daemon URL must be http://127.0.0.1:<port> (loopback only)")
    return url.rstrip("/")


class DaemonClient:
    def __init__(self, base_url: str, token_file: str | Path) -> None:
        self.base_url = check_daemon_url(base_url)
        self.token_file = Path(token_file)
        self._session: Any = None

    def _token(self) -> str:
        try:
            token = self.token_file.read_text().strip()
        except OSError as exc:
            raise DaemonUnavailable(f"cannot read the daemon token ({exc.strerror})") from None
        if not token:
            raise DaemonUnavailable("the daemon token file is empty")
        register_secret(token)
        return token

    async def request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        timeout: float = SHORT_TIMEOUT,
    ) -> tuple[int, Any]:
        """(status, JSON body); DaemonUnavailable when there is no answer to read."""
        import aiohttp

        headers = {"Authorization": f"Bearer {self._token()}"}
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None))
        try:
            async with (
                asyncio.timeout(timeout),
                self._session.request(
                    method, f"{self.base_url}{path}", json=body, headers=headers
                ) as resp,
            ):
                raw = await resp.read()
                status = resp.status
        except TimeoutError:
            raise DaemonUnavailable(f"lapis-daemon did not answer within {timeout:g} s") from None
        except (aiohttp.ClientError, OSError) as exc:
            raise DaemonUnavailable(redact(f"lapis-daemon unreachable: {exc}")[:200]) from None
        try:
            return status, json.loads(raw) if raw else None
        except (ValueError, RecursionError):
            raise DaemonUnavailable(f"lapis-daemon answered HTTP {status} without JSON") from None

    async def authorize(self, payload: dict[str, Any]) -> dict[str, Any]:
        """`POST /v1/policy/authorize`: blocks until the step is decided."""
        status, data = await self.request(
            "POST", "/v1/policy/authorize", payload, LONG_POLL_TIMEOUT
        )
        if status != 200 or not isinstance(data, dict):
            raise DaemonUnavailable(f"the policy gate answered HTTP {status}: {_message(data)}")
        return data

    async def deck_tools(self) -> list[dict[str, Any]]:
        """`GET /v1/deck/tools`: `[{name, description, tier, input_schema}]`."""
        status, data = await self.request("GET", "/v1/deck/tools")
        tools = data.get("tools") if isinstance(data, dict) else None
        if status != 200 or not isinstance(tools, list):
            raise DaemonUnavailable(f"the deck tool list answered HTTP {status}: {_message(data)}")
        return [t for t in tools if isinstance(t, dict) and isinstance(t.get("name"), str)]

    async def invoke(
        self, tool: str, args: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        """`POST /v1/deck/tools/invoke`: `{ok, tier, decision, result?|error?}`.

        Never raises: a daemon that cannot be asked is a denial (fail closed).
        """
        body = {"tool": tool, "args": args, "context": context}
        try:
            status, data = await self.request(
                "POST", "/v1/deck/tools/invoke", body, LONG_POLL_TIMEOUT
            )
        except DaemonUnavailable as exc:
            return {"ok": False, "tier": None, "decision": "deny", "error": str(exc)}
        if status != 200 or not isinstance(data, dict):
            error = f"lapis-daemon answered HTTP {status}: {_message(data)}"
            return {"ok": False, "tier": None, "decision": "deny", "error": error}
        if data.get("ok") is not True:
            data = {**data, "ok": False}
        return data

    async def close(self) -> None:
        if self._session is not None:
            await self._session.close()
            self._session = None


def _message(data: Any) -> str:
    error = data.get("error") if isinstance(data, dict) else None
    if isinstance(error, dict):
        return str(error.get("message") or error.get("code") or "error")[:200]
    return str(error or "no details")[:200]
