"""OpenAI-compatible chat-completions adapter (OpenAI, OpenRouter, any `base_url`).

Plain HTTP over aiohttp, which the daemon already carries for its control
plane: no extra SDK on the 512 MB device.

"Compatible" endpoints differ in the details, so this adapter asks for little
and accepts a lot: an optional request parameter a provider rejects is dropped,
transient failures are retried, and anything that is not a chat-completions
answer becomes an LLMError the owner can act on. Each assistant message goes
back exactly as it was received (reasoning fields and provider extras
included), which MiniMax, DeepSeek and Gemini require inside a tool loop.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
import re
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

BASE_URLS = {
    "openai": "https://api.openai.com/v1",
    "openrouter": "https://openrouter.ai/api/v1",
}
TOOLS: list[dict[str, Any]] = [{"type": "function", "function": spec} for spec in TOOL_SPECS]
TOOLS_NO_SSH = [t for t in TOOLS if t["function"]["name"] != "ssh_exec"]

MAX_ATTEMPTS = 3
RETRYABLE = {408, 409, 429}  # and every 5xx
BACKOFF_CAP = 20.0  # seconds, also for a provider's Retry-After
# Screenshots kept as images in the conversation; an older one shrinks to a line
# of text. Re-sending all of them costs tokens and context on every turn, and
# some providers cap the images per request (Groq 3, Mistral 8).
KEEP_SCREENSHOTS = 3
DETAIL_MAX = 300  # characters of a provider's error message passed on to the owner
# Reasoning some models write inline; a reply cut short may leave the block open.
THINK_RE = re.compile(r"<think>.*?(?:</think>|\Z)", re.DOTALL | re.IGNORECASE)
THINK_OPEN_RE = re.compile(r"<think>", re.IGNORECASE)
THINK_CLOSE_RE = re.compile(r"</think>", re.IGNORECASE)


def _brief(text: str) -> str:
    """One printable line of at most DETAIL_MAX characters (cut first: it may be MBs)."""
    text = "".join(c if c.isprintable() else " " for c in text[: 16 * DETAIL_MAX])
    return " ".join(text.split())[:DETAIL_MAX]


class ApiError(LLMError):
    """An error the provider reported itself."""

    def __init__(self, vendor: str, status: int, detail: str, text: str) -> None:
        code = f" {status}" if status >= 400 else ""  # some report errors under HTTP 200
        super().__init__(f"{vendor} API error{code}: {_brief(detail)}")
        self.status, self.text = status, text


def _transient(status: int) -> bool:
    return status in RETRYABLE or status >= 500


def _stop_if_cancelled() -> None:
    """Let a pending cancellation (the kill switch) through.

    `asyncio.timeout` already does. This is for a layer below that reports a
    cancellation as a timeout or a connection error: it must never buy a retry.
    """
    task = asyncio.current_task()
    if task is not None and task.cancelling():
        raise asyncio.CancelledError


def _reported_error(data: dict[str, Any]) -> str:
    """The provider's own error message in an answer, or "" when it reports none."""
    error, base = data.get("error"), data.get("base_resp")
    if isinstance(error, dict):
        error = error.get("message") or error.get("code") or error.get("type")
    if error:
        return error if isinstance(error, str) else json.dumps(error)
    # MiniMax: every answer carries base_resp, and status_code 0 means success.
    if isinstance(base, dict) and base.get("status_code") not in (None, 0, "0"):
        return f"{base.get('status_msg') or 'error'} (code {base['status_code']})"
    return ""


def read_reply(
    status: int, raw: bytes, vendor: str, secret: str = "", attempts: int = 1
) -> dict[str, Any]:
    """The assistant message of one HTTP answer, or an LLMError the owner can act on.

    `secret` is the API key, so that an error quoting it back never shows it.
    `attempts` is how often the request was made, this answer included.
    """
    bad_format = (
        f"{vendor} did not answer in the chat-completions format (HTTP {status}). "
        "Check the Base URL under Config > Agent & LLM."
    )

    def scrub(text: str) -> str:
        # Not the "x" or "none" a local server is given as its key.
        return text.replace(secret, "[api key]") if len(secret) >= 8 else text

    def api_error(detail: str) -> ApiError:
        """The provider's own words, or else its whole answer."""
        text = scrub(raw.decode(errors="replace"))
        return ApiError(vendor, status, scrub(detail) or text, text)

    try:
        data = json.loads(raw)
    except (ValueError, RecursionError):  # an HTML page, plain text, an empty body
        data = None
    if not isinstance(data, dict):
        if status < 400 or status in (404, 405):  # nothing at this address speaks the API
            raise LLMError(bad_format)
        # A proxy or gateway in front of the provider: a wrong key, a rate limit, an
        # outage. The Base URL is fine, so pass on what it says, unless that is a page.
        said = scrub(raw.decode(errors="replace")).strip()
        if not said or said.startswith("<"):
            said = _status_name(status)
        if attempts > 1 and _transient(status):  # it outlasted the retries
            raise LLMError(
                f"{vendor} is unavailable (HTTP {status}, {attempts} attempts): {_brief(said)}"
            )
        raise ApiError(vendor, status, said, said)
    detail = _reported_error(data)
    if detail or status >= 400:
        raise api_error(detail)
    choices = data.get("choices")
    if not choices:
        raise LLMError(f"{vendor} answered without any choices: the model produced no reply.")
    choice = choices[0] if isinstance(choices, list) else None
    if not isinstance(choice, dict):
        raise LLMError(bad_format)
    # OpenRouter: an upstream failure arrives inside the choice, under HTTP 200.
    detail = _reported_error(choice)
    if detail:
        raise api_error(detail)
    message = choice.get("message")
    calls = (message.get("tool_calls") or []) if isinstance(message, dict) else None
    if (
        not isinstance(message, dict)
        or not isinstance(calls, list)
        or not all(isinstance(c, dict) and isinstance(c.get("function"), dict) for c in calls[:1])
    ):
        raise LLMError(bad_format)
    if not calls and not visible_text(message.get("content")):
        # Nothing to act on and nothing to show: say why, where the answer does.
        reason = choice.get("finish_reason")
        if reason == "length":  # e.g. the whole budget went into reasoning
            raise LLMError("The model ran out of output tokens before choosing an action.")
        if reason == "content_filter" or message.get("refusal"):
            raise LLMError("The model declined this request.")
    return message


def _status_name(status: int) -> str:
    try:
        return HTTPStatus(status).phrase
    except ValueError:  # 529 and other codes outside the registry
        return "no details given"


def visible_text(content: Any) -> str:
    """What the owner may read of a message: its text parts, without <think> blocks."""
    if isinstance(content, list):
        content = "\n".join(
            part["text"]
            for part in content
            if isinstance(part, dict)
            and part.get("type", "text") == "text"
            and isinstance(part.get("text"), str)
        )
    if not isinstance(content, str):
        return ""
    # A chat template that prefills the opening tag sends "reasoning...</think>answer".
    close = THINK_CLOSE_RE.search(content)
    if close and not THINK_OPEN_RE.search(content, 0, close.start()):
        content = content[close.end() :]
    return THINK_RE.sub("", content).strip()


def parse_choice(message: dict[str, Any]) -> ToolCall:
    """First tool call of an assistant message; bare text means the model is done."""
    text = visible_text(message.get("content"))
    calls = message.get("tool_calls") or []
    if not calls:
        if not text:
            raise LLMError("The model returned neither an action nor a reply.")
        return ToolCall("finish", {"summary": text}, text)
    fn = calls[0].get("function") or {}
    args = fn.get("arguments") or {}
    if isinstance(args, str):  # the standard; some providers send the object itself
        try:
            args = json.loads(args)
        except (ValueError, RecursionError) as exc:  # the latter: "[[[[..." to the limit
            raise LLMError("The model produced malformed tool arguments.") from exc
    return ToolCall(
        str(fn.get("name") or ""),
        args if isinstance(args, dict) else {},
        text,
        str(calls[0].get("id") or ""),
    )


def _retry_after(header: str | None, default: float) -> float:
    try:
        return max(0.0, float(header or ""))
    except ValueError:  # absent, or an HTTP date
        return default


class OpenAICompatPlanner(Planner):
    def __init__(self, config: Any) -> None:
        self.config = config
        self.base_url = (config.base_url or BASE_URLS[config.provider]).rstrip("/")
        # Named in every error, so the owner sees which endpoint it came from.
        self.vendor = urlsplit(config.base_url or "").hostname or config.provider
        self.messages: list[dict[str, Any]] = []
        # Optional request parameters; one a provider rejects is not sent again.
        self._extras: dict[str, Any] = {"tool_choice": "auto", "parallel_tool_calls": False}
        self._shots: list[dict[str, Any]] = []  # the messages that still hold an image
        self._session: Any = None
        self._key = ""
        self._pending_id = ""

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
        api_key = os.environ.get(name)
        if not api_key:
            raise LLMError(f"{name} is not set. Add it in the app under Config > Secrets.")
        self._key = api_key
        self._session = aiohttp.ClientSession(
            headers={"Authorization": f"Bearer {api_key}"},
            # No timer of aiohttp's: _post sets the deadline for each attempt.
            timeout=aiohttp.ClientTimeout(total=None),
        )
        self.messages = [
            {
                "role": "system",
                "content": self.instructions or system_prompt(host_os, host, self.ssh),
            },
            *(m for m in history if m["role"] in ("user", "assistant") and m["content"].strip()),
            {"role": "user", "content": task},
        ]
        self._shots = []

    def _append_result(self, result: ToolResult) -> None:
        text = result.text or "ok"
        if result.hint:
            text += f"\n\n{result.hint}"
        self.messages.append({"role": "tool", "tool_call_id": self._pending_id, "content": text})
        if not result.jpeg:
            return
        size = f"Screenshot {result.width}x{result.height}."
        url = "data:image/jpeg;base64," + base64.b64encode(result.jpeg).decode()
        shot = {
            "role": "user",
            "content": [
                {"type": "text", "text": size},
                {"type": "image_url", "image_url": {"url": url}},
            ],
        }
        self.messages.append(shot)
        self._shots.append(shot)
        if len(self._shots) > KEEP_SCREENSHOTS:
            # The oldest image leaves the conversation (and memory); its message
            # stays in place, so the turns around it remain a valid conversation.
            old = self._shots.pop(0)
            old["content"] = f"{old['content'][0]['text']} (Image removed; newer ones follow.)"

    async def _post(self, body: dict[str, Any]) -> dict[str, Any]:
        """One request, retrying what is safe to retry; returns the assistant message."""
        import aiohttp

        for attempt in range(MAX_ATTEMPTS):
            last = attempt + 1 == MAX_ATTEMPTS
            delay = float(2**attempt)
            failure: Exception | None = None
            try:
                # The deadline is asyncio's, not aiohttp's own timer: that one reports a
                # kill landing in the loop tick it fires in as a timeout, which is retried.
                async with (
                    asyncio.timeout(self.config.request_timeout_sec),
                    self._session.post(f"{self.base_url}/chat/completions", json=body) as resp,
                ):
                    raw = await resp.read()
            except (aiohttp.ClientError, OSError) as exc:  # a timeout is an OSError too
                failure = exc
            _stop_if_cancelled()
            if failure is None:
                transient = _transient(resp.status)
                if last or not transient or resp.headers.get("x-should-retry") == "false":
                    return read_reply(resp.status, raw, self.vendor, self._key, attempt + 1)
                delay = _retry_after(resp.headers.get("retry-after"), delay)
            elif last and isinstance(failure, TimeoutError):
                raise LLMError(
                    f"{self.vendor} did not answer within {self.config.request_timeout_sec} s "
                    f"({MAX_ATTEMPTS} attempts)."
                ) from failure
            elif last:
                reason = _brief(str(failure)) or type(failure).__name__
                raise LLMError(f"Could not reach {self.vendor}: {reason}") from failure
            await asyncio.sleep(min(delay, BACKOFF_CAP))
        raise AssertionError("unreachable: the last attempt returns or raises")

    def _tools(self) -> list[dict[str, Any]]:
        if self.tools is not None:  # a deck chat: the daemon's cyberdeck tools
            return [{"type": "function", "function": spec} for spec in self.tools]
        return TOOLS if self.ssh else TOOLS_NO_SSH

    async def _ask(self) -> dict[str, Any]:
        """The next assistant message, without the parameters this provider rejects."""
        while True:
            body = {
                "model": self.config.model,
                "messages": self.messages,
                "tools": self._tools(),
                **self._extras,
            }
            try:
                return await self._post(body)
            except ApiError as exc:
                # e.g. "Unsupported parameter: 'parallel_tool_calls'" (422 from FastAPI servers).
                bad_request = exc.status in (400, 422)
                rejected = [name for name in self._extras if bad_request and name in exc.text]
                if not rejected:
                    raise
                for name in rejected:  # each is dropped once, so this loop ends
                    del self._extras[name]

    async def next(self, result: ToolResult | None) -> ToolCall:
        if result is not None:
            self._append_result(result)
        message = await self._ask()
        message.setdefault("role", "assistant")
        calls = message.get("tool_calls")
        if calls:
            # Keep only the call we act on, so every tool_call gets exactly one
            # result, and give it an id if it has none: the result must name it.
            # Nothing else changes; the provider gets its message back as sent.
            first = calls[0]
            if not (isinstance(first.get("id"), str) and first["id"]):
                first = {**first, "id": f"call_{len(self.messages)}"}
            message = {**message, "tool_calls": [first]}
        self.messages.append(message)
        call = parse_choice(message)
        self._pending_id = call.call_id
        return call

    async def close(self) -> None:
        if self._session is not None:
            await self._session.close()
            self._session = None
