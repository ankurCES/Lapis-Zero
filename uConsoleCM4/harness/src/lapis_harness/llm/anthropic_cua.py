"""Claude adapter: a manual Messages-API tool loop driven one step at a time.

The loop is hosted here, not by an SDK tool runner or Managed Agents, because
every tool call has to pass the on-device policy gate and may wait minutes for a
human approval on the LCD or the phone.

Transport is the Messages API over aiohttp rather than the `anthropic` SDK.
That is a measured decision for this device, not a preference: importing the
SDK costs 35-50 MB of RSS on the CM0's Python (aarch64, 512 MB RAM), which alone
pushed a screenshot cycle past the daemon's 130 MB peak budget (SPEC 2), while
aiohttp is already loaded for the control plane and adds nothing.

The conversation is strictly append-only: each assistant turn is passed back
exactly as received (thinking blocks included), which current Claude models
require to keep their reasoning valid across turns.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
from http import HTTPStatus
from typing import Any
from urllib.parse import urlsplit

from lapis_harness.config import is_key_name
from lapis_harness.llm.base import (
    TOOL_SPECS,
    LLMError,
    Planner,
    ToolCall,
    ToolResult,
    system_prompt,
)

TOOLS_SCHEMA: list[dict[str, Any]] = [
    {"name": t["name"], "description": t["description"], "input_schema": t["parameters"]}
    for t in TOOL_SPECS
]
TOOLS_NO_SSH = [t for t in TOOLS_SCHEMA if t["name"] != "ssh_exec"]
_TOOL_NAMES = {t["name"].lower(): t["name"] for t in TOOL_SPECS}

API_BASE = "https://api.anthropic.com"
API_VERSION = "2026-10-21"
MAX_TOKENS = 16000
MAX_ATTEMPTS = 3
RETRYABLE = {408, 409, 429, 500, 502, 503, 504, 529}
BACKOFF_CAP = 20.0  # seconds, also for a Retry-After
DETAIL_MAX = 300  # characters of an endpoint's error message passed on to the owner
# The usual reason for an answer in another format, seen on the device: an
# OpenAI-style Base URL (MiniMax's) under the anthropic protocol.
PROTOCOL_HINT = (
    " With a Base URL set, the protocol under Config > Agent & LLM usually has to be openai."
)
FALLBACK_BETA = "server-side-fallback-2026-07-01"
# Models whose safety classifiers can decline a turn and that support the
# server-side fallback beta (a declined turn is re-run on a fallback model).
FALLBACK_MODELS = {
    "claude-sonnet-5-5",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-fable-5-2",
    "claude-fable-5-1",
}
# Models that accept an operator message in the middle of `messages`.
MID_SYSTEM_MODELS = FALLBACK_MODELS | {"claude-haiku-5-5", "claude-opus-4-8", "claude-fable-5"}
# `output_config.effort` exists from the 4.6 generation on.
_EFFORT_PREFIXES = (
    "claude-fable-",
    "claude-mythos-",
    "claude-opus-5",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-sonnet-5",
    "claude-sonnet-4-6",
    "claude-haiku-5",
)


def _brief(text: str) -> str:
    """One printable line of at most DETAIL_MAX characters (cut first: it may be MBs)."""
    text = "".join(c if c.isprintable() else " " for c in text[: 16 * DETAIL_MAX])
    return " ".join(text.split())[:DETAIL_MAX]


class ApiStatusError(Exception):
    """An error answer from the Messages API."""

    def __init__(self, status: int, kind: str, message: str) -> None:
        message = _brief(message)  # an endpoint may echo the whole request
        super().__init__(f"{status} {kind}: {message}")
        self.status, self.kind, self.message = status, kind, message


def _stop_if_cancelled() -> None:
    """Let a pending cancellation (the kill switch) through.

    `asyncio.timeout` already does. This is for a layer below that reports a
    cancellation as a timeout or a connection error: it must never buy a retry.
    """
    task = asyncio.current_task()
    if task is not None and task.cancelling():
        raise asyncio.CancelledError


def _retry_after(header: str | None, default: float) -> float:
    try:
        return max(0.0, float(header or ""))
    except ValueError:  # absent, or an HTTP date
        return default


def _status_name(status: int) -> str:
    try:
        return HTTPStatus(status).phrase
    except ValueError:  # 529 and other codes outside the registry
        return "no details given"


def read_message(
    status: int, raw: bytes, vendor: str, secret: str = "", hint: str = ""
) -> dict[str, Any]:
    """The assistant message of one HTTP answer.

    Anything else raises: ApiStatusError for an error the endpoint reports, LLMError
    when the answer is not the Messages API's at all (an OpenAI-style endpoint behind
    the Base URL, a proxy's page). `secret` is the API key, so that an error quoting
    it back never shows it; `hint` is appended to the LLMError.
    """

    def scrub(text: str) -> str:
        return text.replace(secret, "[api key]") if len(secret) >= 8 else text

    try:
        data = json.loads(raw)
    except (ValueError, RecursionError):  # "404 page not found", an HTML page, an empty body
        data = None
    error = data.get("error") if isinstance(data, dict) else None
    kind: Any = ""
    if isinstance(error, dict):  # the Messages API's envelope; a gateway may send a string
        kind, error = error.get("type"), error.get("message") or error.get("type")
    if error:
        detail = error if isinstance(error, str) else json.dumps(error)
        raise ApiStatusError(status, kind if isinstance(kind, str) else "", scrub(detail))
    if status == 200:
        content = data.get("content") if isinstance(data, dict) else None
        if isinstance(content, list) and all(isinstance(block, dict) for block in content):
            return dict(data)
    elif status >= 400 and status not in (404, 405):
        # A proxy or gateway in front of the API: a wrong key, a rate limit, an outage.
        said = scrub(raw.decode(errors="replace")).strip()
        if not said or said.startswith("<"):  # nothing, or a page
            said = _status_name(status)
        raise ApiStatusError(status, "", said)
    raise LLMError(f"{vendor} did not answer in Anthropic's Messages format (HTTP {status}).{hint}")


def parse_message(message: dict[str, Any]) -> ToolCall:
    """Reduce one assistant message to the next tool call.

    Reads blocks by type, never by position: a response may open with thinking
    blocks. Plain text with no tool call means the model considers itself done.
    """
    content = message.get("content")
    blocks = [b for b in content if isinstance(b, dict)] if isinstance(content, list) else []
    if message.get("stop_reason") == "refusal":
        details = message.get("stop_details")
        category = details.get("category") if isinstance(details, dict) else None
        category = category if category and isinstance(category, str) else "policy"
        raise LLMError(f"The model declined this request ({category}).")
    text = " ".join(
        b["text"].strip()
        for b in blocks
        if b.get("type") == "text" and isinstance(b.get("text"), str)
    ).strip()
    for block in blocks:
        if block.get("type") != "tool_use":
            continue
        # Tolerate a near-miss on letter case in the tool name.
        name = str(block.get("name") or "")
        args = block.get("input")
        return ToolCall(
            _TOOL_NAMES.get(name.lower(), name),
            dict(args) if isinstance(args, dict) else {},
            text,
            str(block.get("id") or ""),
        )
    if message.get("stop_reason") == "max_tokens":
        raise LLMError("The model ran out of output tokens before choosing an action.")
    if not text:
        raise LLMError("The model returned neither an action nor a reply.")
    return ToolCall("finish", {"summary": text}, text)


class AnthropicPlanner(Planner):
    def __init__(self, config: Any) -> None:
        self.config = config
        self.model: str = config.model
        self.base_url = (config.base_url or API_BASE).rstrip("/")
        # Named when the answer is not the Messages API's: the owner sees who gave it.
        self.vendor = urlsplit(self.base_url).hostname or "anthropic"
        self.messages: list[dict[str, Any]] = []
        self.system = ""
        self._session: Any = None
        self._api_key = ""
        self._pending_id = ""
        self._use_fallbacks = self.model in FALLBACK_MODELS

    async def start(
        self, task: str, *, host: str, host_os: str, history: list[dict[str, str]]
    ) -> None:
        import aiohttp

        name = self.config.api_key_env
        if not is_key_name(name):  # it may be the key itself: never repeated, never looked up
            raise LLMError(
                "The API key name in Config > Agent & LLM is not a name. Enter a name such "
                "as MINIMAX_API_KEY there; the key itself goes under Config > Secrets."
            )
        self._api_key = os.environ.get(name, "")
        if not self._api_key:
            raise LLMError(f"{name} is not set. Add it in the app under Config > Secrets.")
        # No timer of aiohttp's: _post sets the deadline for each attempt.
        self._session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None))
        self.system = self.instructions or system_prompt(host_os, host, self.ssh)
        # Earlier turns of this chat, as plain text, then the new request.
        self.messages = [
            {"role": m["role"], "content": m["content"]}
            for m in history
            if m["role"] in ("user", "assistant") and m["content"].strip()
        ]
        if self.messages and self.messages[-1]["role"] == "user":
            self.messages.append({"role": "assistant", "content": "(no reply was recorded)"})
        self.messages.append({"role": "user", "content": task})

    def _append_result(self, result: ToolResult) -> None:
        content: list[dict[str, Any]] = [{"type": "text", "text": result.text or "ok"}]
        if result.jpeg:
            content.append(
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/jpeg",
                        "data": base64.b64encode(result.jpeg).decode(),
                    },
                }
            )
            content.append({"type": "text", "text": f"Screenshot {result.width}x{result.height}."})
        block: dict[str, Any] = {
            "type": "tool_result",
            "tool_use_id": self._pending_id,
            "content": content,
        }
        if result.is_error or result.denied:
            block["is_error"] = True
        self.messages.append({"role": "user", "content": [block]})
        if result.hint:
            # A harness notice is an operator instruction, not tool output: keep
            # it out of the tool_result so it is not mistaken for injected text.
            if self.model in MID_SYSTEM_MODELS:
                self.messages.append({"role": "system", "content": result.hint})
            else:
                self.messages[-1]["content"].append({"type": "text", "text": result.hint})

    def _tools(self) -> list[dict[str, Any]]:
        if self.tools is not None:  # a deck chat: the daemon's cyberdeck tools
            return [
                {
                    "name": t["name"],
                    "description": t["description"],
                    "input_schema": t["parameters"],
                }
                for t in self.tools
            ]
        return TOOLS_SCHEMA if self.ssh else TOOLS_NO_SSH

    def _request(self) -> tuple[dict[str, Any], list[str]]:
        """(JSON body, beta flags) for the next turn."""
        body: dict[str, Any] = {
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": self.system,
            "tools": self._tools(),
            # One call per turn: each one passes the policy gate on its own.
            # (`auto`: forcing a tool is rejected by current models.)
            "tool_choice": {"type": "auto", "disable_parallel_tool_use": True},
            # The history only grows, so each turn re-reads the previous prefix.
            "cache_control": {"type": "ephemeral"},
            "messages": self.messages,
        }
        if self.model.startswith(_EFFORT_PREFIXES):
            body["output_config"] = {"effort": self.config.effort}
        if self._use_fallbacks:
            body["fallbacks"] = "default"
            return body, [FALLBACK_BETA]
        return body, []

    async def _post(self, body: dict[str, Any], betas: list[str]) -> dict[str, Any]:
        """One Messages API call, retrying what is safe to retry; returns the message."""
        import aiohttp

        headers = {"x-api-key": self._api_key, "anthropic-version": API_VERSION}
        if betas:
            headers["anthropic-beta"] = ",".join(betas)
        hint = PROTOCOL_HINT if self.config.base_url else ""
        for attempt in range(MAX_ATTEMPTS):
            last = attempt + 1 == MAX_ATTEMPTS
            delay = float(2**attempt)
            failure: Exception | None = None
            try:
                # The deadline is asyncio's, not aiohttp's own timer: that one reports a
                # kill landing in the loop tick it fires in as a timeout, which is retried.
                async with (
                    asyncio.timeout(self.config.request_timeout_sec),
                    self._session.post(
                        f"{self.base_url}/v1/messages", json=body, headers=headers
                    ) as resp,
                ):
                    raw = await resp.read()
            except (aiohttp.ClientError, OSError) as exc:  # a timeout is an OSError too
                failure = exc
            _stop_if_cancelled()
            if failure is None:
                retry = resp.status in RETRYABLE and resp.headers.get("x-should-retry") != "false"
                if last or not retry:
                    return read_message(resp.status, raw, self.vendor, self._api_key, hint)
                delay = _retry_after(resp.headers.get("retry-after"), delay)
            elif last:
                raise LLMError("Could not reach the Anthropic API (network).") from failure
            await asyncio.sleep(min(delay, BACKOFF_CAP))
        raise AssertionError("unreachable: the last attempt returns or raises")

    async def next(self, result: ToolResult | None) -> ToolCall:
        if result is not None:
            self._append_result(result)
        try:
            try:
                message = await self._post(*self._request())
            except ApiStatusError as exc:
                # Platforms without the fallback beta reject the parameter.
                rejected = exc.status == 400 and "fallback" in exc.message.lower()
                if not (self._use_fallbacks and rejected):
                    raise
                self._use_fallbacks = False
                message = await self._post(*self._request())
        except ApiStatusError as exc:
            if exc.status == 401:
                raise LLMError("The Anthropic API key was rejected.") from exc
            if exc.status == 403:
                raise LLMError(f"The API key may not use {self.model}: {exc.message}") from exc
            if exc.status in (404, 405) and self.config.base_url and exc.kind != "not_found_error":
                # Not the Messages API's "no such model" but some other API's "no such
                # path": its own words, and the usual cause.
                said = f"{self.vendor} API error {exc.status}: {exc.message.rstrip('.')}."
                raise LLMError(said + PROTOCOL_HINT) from exc
            if exc.status == 404:
                raise LLMError(f"Unknown model {self.model!r}. Check Config > Agent.") from exc
            if exc.status == 429:
                raise LLMError("Rate limited by the Anthropic API; try again shortly.") from exc
            code = f" {exc.status}" if exc.status >= 400 else ""  # an error under HTTP 200
            raise LLMError(f"Anthropic API error{code}: {exc.message}") from exc
        # Pass the turn back unchanged on the next request (thinking blocks too).
        self.messages.append({"role": "assistant", "content": message.get("content") or []})
        call = parse_message(message)
        self._pending_id = call.call_id
        return call

    async def close(self) -> None:
        if self._session is not None:
            await self._session.close()
            self._session = None


class AnthropicCuaClient:
    """Back-compat shim for the pure parser tests."""

    @staticmethod
    def parse_tool_use(response: dict[str, Any]) -> ToolCall:
        call = parse_message(response)
        return ToolCall(call.tool, call.args)
