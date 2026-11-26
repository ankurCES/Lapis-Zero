from __future__ import annotations

import asyncio
import base64
import contextlib
import json
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from aiohttp import web
from conftest import wait_for

from lapis_harness.config import (
    CoreConfig,
    LLMConfig,
    is_reserved_name,
    load_config,
    load_hosts,
    read_env_file,
    write_env_file,
)
from lapis_harness.llm import anthropic_cua as claude
from lapis_harness.llm import openai_cua as compat
from lapis_harness.llm.anthropic_cua import TOOLS_SCHEMA, AnthropicCuaClient, AnthropicPlanner
from lapis_harness.llm.base import UNIFIED_TOOLS, LLMError, ToolCall, ToolResult
from lapis_harness.llm.openai_cua import OpenAICompatPlanner, parse_choice


def test_tool_schema_covers_unified_surface() -> None:
    names = {t["name"] for t in TOOLS_SCHEMA}
    assert names == set(UNIFIED_TOOLS) == {
        "screenshot", "mouse_click", "mouse_move", "mouse_drag", "scroll",
        "type_text", "key_press", "ssh_exec", "wait", "finish",
    }  # fmt: skip
    for tool in TOOLS_SCHEMA:
        assert tool["input_schema"]["type"] == "object" and tool["description"]


def test_parse_message_reads_blocks_by_type() -> None:
    resp = {
        "stop_reason": "tool_use",
        "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "reasoning"},
            {"type": "tool_use", "id": "tu_1", "name": "Mouse_Click", "input": {"x": 10, "y": 20}},
            {"type": "tool_use", "id": "tu_2", "name": "finish", "input": {"summary": "done"}},
        ],
    }
    call = claude.parse_message(resp)
    # First call wins, a near-miss on letter case is tolerated, ids are carried.
    assert call == ToolCall("mouse_click", {"x": 10, "y": 20}, "reasoning")
    assert call.call_id == "tu_1"
    assert AnthropicCuaClient.parse_tool_use(resp) == ToolCall("mouse_click", {"x": 10, "y": 20})
    # Plain text with no tool call means the model considers itself finished.
    done = claude.parse_message(
        {"stop_reason": "end_turn", "content": [{"type": "text", "text": "All set."}]}
    )
    assert done == ToolCall("finish", {"summary": "All set."}, "All set.")
    with pytest.raises(LLMError, match="neither an action nor a reply"):
        claude.parse_message({"content": []})
    with pytest.raises(LLMError, match="declined.*cyber"):
        claude.parse_message(
            {"stop_reason": "refusal", "stop_details": {"category": "cyber"}, "content": []}
        )
    with pytest.raises(LLMError, match="output tokens"):
        claude.parse_message(
            {"stop_reason": "max_tokens", "content": [{"type": "thinking", "thinking": ""}]}
        )


def planner_with(model: str, replies: list[Any], reject_fallbacks: bool = False):
    planner = AnthropicPlanner(LLMConfig(model=model, effort="low"))
    log: list[tuple[dict[str, Any], list[str]]] = []

    async def post(body: dict[str, Any], betas: list[str]) -> dict[str, Any]:
        log.append((dict(body, messages=list(body["messages"])), betas))
        if reject_fallbacks and "fallbacks" in body:
            raise claude.ApiStatusError(400, "invalid_request_error", "fallbacks: not supported")
        return replies.pop(0)

    planner._post = post  # type: ignore[method-assign]
    planner.system = "sys"
    planner.messages = [{"role": "user", "content": "do it"}]
    return planner, log


def tool_use(name: str, **args: Any) -> dict[str, Any]:
    return {
        "stop_reason": "tool_use",
        "content": [
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": f"tu_{name}", "name": name, "input": args},
        ],
    }


def test_planner_requests_and_append_only_history() -> None:
    async def scenario() -> None:
        first_reply = tool_use("screenshot")
        planner, log = planner_with(
            "claude-sonnet-5-5", [first_reply, tool_use("finish", summary="ok")]
        )
        assert (await planner.next(None)).tool == "screenshot"
        body, betas = log[0]
        assert body["model"] == "claude-sonnet-5-5" and body["system"] == "sys"
        assert body["max_tokens"] == 16000 and body["tools"] is TOOLS_SCHEMA
        # Forced tool choice and disabling thinking are both 400s on this model family,
        # and sampling parameters are rejected by this API version.
        assert body["tool_choice"] == {"type": "auto", "disable_parallel_tool_use": True}
        assert not {"thinking", "temperature", "top_p", "top_k"} & set(body)
        assert body["output_config"] == {"effort": "low"}
        assert body["cache_control"] == {"type": "ephemeral"}
        assert betas == [claude.FALLBACK_BETA] and body["fallbacks"] == "default"

        result = ToolResult("", b"\xff\xd8jpeg", 1280, 720, hint="Try something else.")
        assert (await planner.next(result)).tool == "finish"
        roles = [m["role"] for m in planner.messages]
        # user task, assistant turn, tool_result, operator hint, assistant.
        assert roles == ["user", "assistant", "user", "system", "assistant"]
        # The assistant turn goes back exactly as received, thinking block included.
        assert planner.messages[1]["content"] is first_reply["content"]
        assert log[1][0]["messages"][1]["content"][0]["type"] == "thinking"
        block = planner.messages[2]["content"][0]
        assert (block["type"], block["tool_use_id"]) == ("tool_result", "tu_screenshot")
        image = block["content"][1]
        assert image["source"]["media_type"] == "image/jpeg"
        assert base64.b64decode(image["source"]["data"]) == b"\xff\xd8jpeg"
        assert "is_error" not in block
        assert planner.messages[3] == {"role": "system", "content": "Try something else."}

    asyncio.run(scenario())


def test_planner_adapts_to_older_models_and_missing_fallback_support() -> None:
    async def scenario() -> None:
        # A model without effort / fallbacks / mid-conversation system messages.
        planner, log = planner_with(
            "claude-haiku-4-5", [tool_use("wait", seconds=1), tool_use("finish")]
        )
        await planner.next(None)
        assert "output_config" not in log[0][0] and "fallbacks" not in log[0][0]
        assert log[0][1] == []
        await planner.next(ToolResult("Error: boom", is_error=True, hint="Change approach."))
        tail = planner.messages[-2]
        assert tail["role"] == "user" and tail["content"][0]["is_error"] is True
        assert tail["content"][1] == {"type": "text", "text": "Change approach."}

        # A platform that rejects the fallback beta: retried once without it, then remembered.
        planner, log = planner_with(
            "claude-sonnet-5-5", [tool_use("screenshot"), tool_use("finish")], reject_fallbacks=True
        )
        assert (await planner.next(None)).tool == "screenshot"
        assert "fallbacks" in log[0][0] and "fallbacks" not in log[1][0] and log[1][1] == []
        await planner.next(ToolResult("ok"))
        assert "fallbacks" not in log[2][0] and len(log) == 3

    asyncio.run(scenario())


def test_planner_speaks_the_messages_api_over_http(monkeypatch: pytest.MonkeyPatch) -> None:
    """A local stand-in for api.anthropic.com: headers, retry on 529, error mapping."""
    from aiohttp import web

    def api_error(status: int, kind: str, **headers: str) -> web.Response:
        body = {"type": "error", "error": {"type": kind, "message": kind}}
        return web.json_response(body, status=status, headers=headers)

    seen: list[dict[str, Any]] = []
    answers = [
        api_error(529, "overloaded_error", **{"retry-after": "0"}),
        web.json_response(tool_use("ssh_exec", command="ls")),
        api_error(401, "authentication_error"),
    ]

    async def messages(request: web.Request) -> web.Response:
        seen.append({"headers": dict(request.headers), "body": await request.json()})
        return answers.pop(0)

    async def scenario() -> None:
        app = web.Application()
        app.router.add_post("/v1/messages", messages)
        runner = web.AppRunner(app)
        await runner.setup()
        site = web.TCPSite(runner, "127.0.0.1", 0)
        await site.start()
        port = runner.addresses[0][1]
        monkeypatch.setenv("LAPIS_TEST_KEY", "sk-test-123")
        planner = AnthropicPlanner(
            LLMConfig(
                model="claude-sonnet-5-5",
                api_key_env="LAPIS_TEST_KEY",
                base_url=f"http://127.0.0.1:{port}",
            )
        )
        history = [{"role": "user", "content": "hi"}]
        await planner.start("list files", host="mac", host_os="macos", history=history)
        assert (await planner.next(None)).args == {"command": "ls"}
        assert len(seen) == 2  # the 529 was retried
        headers = {k.lower(): v for k, v in seen[1]["headers"].items()}
        assert headers["x-api-key"] == "sk-test-123"
        assert headers["anthropic-version"] == claude.API_VERSION == "2026-10-21"
        assert headers["anthropic-beta"] == claude.FALLBACK_BETA
        body = seen[1]["body"]
        assert "macos" in body["system"]
        assert body["messages"][-1] == {"role": "user", "content": "list files"}
        # History ending on a user turn gets a placeholder so roles still alternate.
        assert [m["role"] for m in body["messages"]] == ["user", "assistant", "user"]
        with pytest.raises(LLMError, match="API key was rejected"):
            await planner.next(ToolResult("a.txt"))
        await planner.close()
        await runner.cleanup()

    asyncio.run(scenario())


def test_planner_needs_an_api_key(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("LAPIS_TEST_MISSING_KEY", raising=False)
    planner = AnthropicPlanner(LLMConfig(api_key_env="LAPIS_TEST_MISSING_KEY"))
    with pytest.raises(LLMError, match="LAPIS_TEST_MISSING_KEY is not set"):
        asyncio.run(planner.start("task", host="mac", host_os="macos", history=[]))


def test_openai_compatible_choice_parsing() -> None:
    call = parse_choice(
        {
            "content": "thinking out loud",
            "tool_calls": [
                {"id": "call_1", "function": {"name": "ssh_exec", "arguments": '{"command": "ls"}'}}
            ],
        }
    )
    assert call == ToolCall("ssh_exec", {"command": "ls"}, "thinking out loud")
    assert parse_choice({"content": "Finished."}).tool == "finish"
    with pytest.raises(LLMError):
        parse_choice({"content": ""})
    with pytest.raises(LLMError, match="malformed"):
        parse_choice({"tool_calls": [{"id": "c", "function": {"name": "x", "arguments": "{"}}]})
    # A model stuck on "[" until its output limit: too deep for the JSON parser.
    stuck = {"name": "x", "arguments": "[" * 100_000}
    with pytest.raises(LLMError, match="malformed"):
        parse_choice({"tool_calls": [{"id": "c", "function": stuck}]})


# --- OpenAI-compatible adapter against a local stand-in for a third-party vendor ---

KEY = "sk-test-123"
HANG_UP = object()


class Vendor:
    """An OpenAI-compatible endpoint on 127.0.0.1 that answers from a script.

    A script entry is a JSON payload (sent with HTTP 200), a callable taking the
    request body and returning a response (or a coroutine giving one), or
    HANG_UP to drop the connection without an answer.
    """

    def __init__(self, *script: Any, rejects: tuple[str, ...] = ()) -> None:
        self.script, self.rejects = list(script), rejects
        self.bodies: list[dict[str, Any]] = []
        self.auth: list[str | None] = []

    async def handle(self, request: web.Request) -> web.StreamResponse:
        body = await request.json()
        self.bodies.append(body)
        self.auth.append(request.headers.get("Authorization"))
        for name in self.rejects:
            if name in body:
                return http(400, {"error": {"message": f"Unsupported parameter: '{name}'."}})(body)
        answer = self.script.pop(0)
        if answer is HANG_UP:
            assert request.transport is not None
            request.transport.close()
            return web.Response()
        if callable(answer):
            answer = answer(body)
        if asyncio.iscoroutine(answer):
            answer = await answer
        return answer if isinstance(answer, web.StreamResponse) else web.json_response(answer)


def http(status: int, payload: Any = None, **headers: str) -> Callable[[Any], web.Response]:
    """An answer with an HTTP status: JSON when there is a payload, else an empty body."""
    if payload is None:
        return lambda _body: web.Response(status=status, headers=headers)
    return lambda _body: web.json_response(payload, status=status, headers=headers)


def plain(status: int, body: str | bytes = b"", **headers: str) -> Callable[[Any], web.Response]:
    """An answer that need not be JSON: a proxy's text, a page, any bytes."""
    data = body.encode() if isinstance(body, str) else body
    return lambda _body: web.Response(status=status, body=data, headers=headers)


def completion(message: Any, **extra: Any) -> dict[str, Any]:
    return {
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "choices": [{"message": message}],
    } | extra


def tool_message(name: str, call_id: str = "call_1", **args: Any) -> dict[str, Any]:
    function = {"name": name, "arguments": json.dumps(args)}
    call = {"id": call_id, "type": "function", "function": function}
    return {"role": "assistant", "content": None, "tool_calls": [call]}


def against(
    vendor: Vendor,
    scenario: Callable[[Any], Awaitable[None]],
    monkeypatch: pytest.MonkeyPatch,
    anthropic: bool = False,
    **llm: Any,
) -> Any:
    """Run `scenario(planner)` with a started planner whose base_url is `vendor`.

    The planner is the OpenAI-compatible one, or with `anthropic` the Claude one
    (the stand-in answers on both paths; the script decides what it says).
    """
    planners: list[Any] = []

    async def main() -> None:
        app = web.Application()
        app.router.add_post("/v1/chat/completions", vendor.handle)
        app.router.add_post("/v1/messages", vendor.handle)
        # Cancelled handlers: a request the planner gave up on does not hold up cleanup.
        runner = web.AppRunner(app, handler_cancellation=True)
        await runner.setup()
        await web.TCPSite(runner, "127.0.0.1", 0).start()
        monkeypatch.setenv("LAPIS_TEST_KEY", KEY)
        url = f"http://127.0.0.1:{runner.addresses[0][1]}"
        config = LLMConfig(
            provider="anthropic" if anthropic else "openai",
            model="claude-test" if anthropic else "some-model",
            api_key_env="LAPIS_TEST_KEY",
            base_url=url if anthropic else f"{url}/v1",
        ).model_copy(update=llm)
        planner = AnthropicPlanner(config) if anthropic else OpenAICompatPlanner(config)
        planners.append(planner)
        await planner.start("do it", host="mac", host_os="macos", history=[])
        try:
            await scenario(planner)
        finally:
            await planner.close()
            await runner.cleanup()

    asyncio.run(main())
    return planners[0]


def first_turn(
    monkeypatch: pytest.MonkeyPatch, vendor: Vendor, anthropic: bool = False, **llm: Any
) -> tuple[ToolCall, Any]:
    calls: list[ToolCall] = []

    async def scenario(planner: Any) -> None:
        calls.append(await planner.next(None))

    planner = against(vendor, scenario, monkeypatch, anthropic, **llm)
    return calls[0], planner


def in_choice(message: Any, **beside: Any) -> dict[str, Any]:
    """A completion whose single choice carries `beside` next to the message."""
    return {"id": "chatcmpl-1", "choices": [{"message": message, **beside}]}


NOT_CHAT_FORMAT = (
    r"^127\.0\.0\.1 did not answer in the chat-completions format \(HTTP {}\)\. "
    r"Check the Base URL under Config > Agent & LLM\.$"
)


@pytest.mark.parametrize(
    ("answer", "message"),
    [
        # Not a chat-completions answer at all (a wrong path, a proxy page, ...).
        (
            lambda _: web.Response(status=404, text="<html>404</html>", content_type="text/html"),
            NOT_CHAT_FORMAT.format(404),
        ),
        (http(404), NOT_CHAT_FORMAT.format(404)),
        (plain(405, "Method Not Allowed"), NOT_CHAT_FORMAT.format(405)),
        (lambda _: web.Response(status=200, text="OK"), NOT_CHAT_FORMAT.format(200)),
        # A gateway in front of the provider answers without JSON: the Base URL is not
        # the problem, so its own words are passed on (the name of the status for a
        # page or an empty body), on one line and never with the key.
        (http(401), r"^127\.0\.0\.1 API error 401: Unauthorized$"),
        (plain(401, "Invalid  API key\n"), r"^127\.0\.0\.1 API error 401: Invalid API key$"),
        (plain(403, "<html><h1>Access denied</h1>"), r"^127\.0\.0\.1 API error 403: Forbidden$"),
        (
            plain(400, f"no such key: {KEY}"),
            r"^127\.0\.0\.1 API error 400: no such key: \[api key\]$",
        ),
        (plain(499), r"^127\.0\.0\.1 API error 499: no details given$"),
        (
            plain(400, b"\x00\x1b[31mred\x07 alert\r\n"),
            r"^127\.0\.0\.1 API error 400: \[31mred alert$",
        ),
        ([], NOT_CHAT_FORMAT.format(200)),
        (completion("not an object"), NOT_CHAT_FORMAT.format(200)),
        (
            completion({"tool_calls": [{"id": "c", "function": "wait"}]}),
            NOT_CHAT_FORMAT.format(200),
        ),
        (completion({"tool_calls": {"id": "c"}}), NOT_CHAT_FORMAT.format(200)),
        # The provider's own error text is passed on, whatever shape it comes in.
        (http(400, {"error": "model not found"}), r"^127\.0\.0\.1 API error 400: model not found$"),
        (
            http(404, {"detail": "Not Found"}),
            r'^127\.0\.0\.1 API error 404: \{"detail": "Not Found"\}$',
        ),
        # ... also when it arrives with HTTP 200 and no choices.
        (
            {"error": {"code": 502, "message": "Provider returned error"}},
            r"^127\.0\.0\.1 API error: Provider returned error$",
        ),
        ({"error": "quota exceeded"}, r"^127\.0\.0\.1 API error: quota exceeded$"),
        (
            {"choices": None, "base_resp": {"status_code": 1004, "status_msg": "login fail"}},
            r"^127\.0\.0\.1 API error: login fail \(code 1004\)$",
        ),
        ({"choices": []}, r"^127\.0\.0\.1 answered without any choices"),
        ({"id": "chatcmpl-1"}, r"^127\.0\.0\.1 answered without any choices"),
        # ... or inside the choice (OpenRouter, when the upstream provider fails).
        (
            in_choice(
                {"role": "assistant", "content": ""},
                finish_reason="error",
                error={"code": 502, "message": f"Upstream died ({KEY})"},
            ),
            r"^127\.0\.0\.1 API error: Upstream died \(\[api key\]\)$",
        ),
        (in_choice(None, error="upstream timeout"), r"^127\.0\.0\.1 API error: upstream timeout$"),
        # No action and no text: the answer says why.
        (
            in_choice(
                {"role": "assistant", "content": "", "reasoning_content": "First, I ..."},
                finish_reason="length",
            ),
            r"^The model ran out of output tokens before choosing an action\.$",
        ),
        (
            in_choice({"role": "assistant", "content": "<think>First, I"}, finish_reason="length"),
            r"^The model ran out of output tokens before choosing an action\.$",
        ),
        (
            in_choice({"role": "assistant", "content": None, "refusal": "I can't help with that."}),
            r"^The model declined this request\.$",
        ),
        (
            in_choice({"role": "assistant", "content": None}, finish_reason="content_filter"),
            r"^The model declined this request\.$",
        ),
        (
            in_choice({"role": "assistant", "content": ""}, finish_reason="stop"),
            r"^The model returned neither an action nor a reply\.$",
        ),
    ],
)
def test_compat_unexpected_answers_become_errors_the_owner_can_act_on(
    monkeypatch: pytest.MonkeyPatch, answer: Any, message: str
) -> None:
    vendor = Vendor(answer)
    with pytest.raises(LLMError, match=message):
        first_turn(monkeypatch, vendor)
    assert len(vendor.bodies) == 1  # and none of these is retried


def test_compat_tolerates_harmless_deviations(monkeypatch: pytest.MonkeyPatch) -> None:
    # Content as a list of parts: the text parts are the reply.
    parts = [
        {"type": "text", "text": "All"},
        {"type": "image_url"},
        {"type": "text", "text": "done."},
    ]
    message = {"role": "assistant", "content": parts}
    call, planner = first_turn(monkeypatch, Vendor(completion(message)))
    assert call == ToolCall("finish", {"summary": "All\ndone."}, "All\ndone.")
    assert planner.messages[-1] == message
    # Arguments as an object instead of a JSON string.
    message = tool_message("wait")
    message["tool_calls"][0]["function"]["arguments"] = {"seconds": 2}
    call, planner = first_turn(monkeypatch, Vendor(completion(message)))
    assert call == ToolCall("wait", {"seconds": 2}) and call.call_id == "call_1"
    assert planner.messages[-1] == message
    # MiniMax reports success as base_resp.status_code 0 next to the choices.
    ok = completion(tool_message("screenshot"), base_resp={"status_code": 0, "status_msg": ""})
    assert first_turn(monkeypatch, Vendor(ok))[0].tool == "screenshot"
    # An answer cut at the token limit is still used when it holds an action or a reply,
    # and an empty `error` or `refusal` beside it means nothing.
    cut = in_choice(tool_message("wait", seconds=1), finish_reason="length", error=None)
    assert first_turn(monkeypatch, Vendor(cut))[0] == ToolCall("wait", {"seconds": 1})
    cut = in_choice(
        {"role": "assistant", "content": "Half a", "refusal": None}, finish_reason="length"
    )
    assert first_turn(monkeypatch, Vendor(cut))[0].args == {"summary": "Half a"}


@pytest.mark.parametrize("missing", [{}, {"id": ""}, {"id": None}])
def test_compat_gives_an_id_to_a_tool_call_without_one(
    monkeypatch: pytest.MonkeyPatch, missing: dict[str, Any]
) -> None:
    function = {"name": "wait", "arguments": '{"seconds": 1}'}
    received = {"role": "assistant", "tool_calls": [{"type": "function", "function": function}]}
    received["tool_calls"][0].update(missing)
    vendor = Vendor(completion(received), completion({"role": "assistant", "content": "Done."}))

    async def scenario(planner: OpenAICompatPlanner) -> None:
        call = await planner.next(None)
        assert call == ToolCall("wait", {"seconds": 1}) and call.call_id
        assert (await planner.next(ToolResult("waited"))).tool == "finish"

    against(vendor, scenario, monkeypatch)
    assistant, tool = vendor.bodies[1]["messages"][-2:]
    # The id in the history is the one the tool result answers.
    assert assistant["tool_calls"][0]["id"] == tool["tool_call_id"] != ""
    assert assistant["tool_calls"][0]["function"] == function
    assert tool == {"role": "tool", "tool_call_id": tool["tool_call_id"], "content": "waited"}


def test_compat_hides_think_blocks_but_returns_the_message_unchanged(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    first = {"id": "call_a", "type": "function", "extra_content": {"google": {"sig": "abc"}}}
    first["function"] = {"name": "ssh_exec", "arguments": '{"command": "ls"}'}
    second = {"id": "call_b", "type": "function", "function": {"name": "wait", "arguments": "{}"}}
    acting = {
        "role": "assistant",
        "content": "<think>They want a listing.\nUse the shell.</think>\n\nListing the files.",
        "reasoning_content": "private chain of thought",
        "tool_calls": [first, second],
    }
    final = {"role": "assistant", "content": "<THINK>wrap up</THINK>Done.<think>and"}
    vendor = Vendor(completion(acting), completion(final))
    calls: list[ToolCall] = []

    async def scenario(planner: OpenAICompatPlanner) -> None:
        calls.append(await planner.next(None))
        calls.append(await planner.next(ToolResult("a.txt")))

    planner = against(vendor, scenario, monkeypatch)
    # The owner sees the reply without the reasoning ...
    assert calls[0] == ToolCall("ssh_exec", {"command": "ls"}, "Listing the files.")
    assert calls[1] == ToolCall("finish", {"summary": "Done."}, "Done.")
    # ... while the provider gets its message back as sent (reasoning fields and
    # provider extras included), apart from the tool calls we did not act on.
    sent_back = vendor.bodies[1]["messages"][-2]
    assert sent_back == {**acting, "tool_calls": [first]} == planner.messages[-3]
    assert vendor.bodies[1]["messages"][-1]["tool_call_id"] == "call_a"
    assert planner.messages[-1] == final
    with pytest.raises(LLMError, match="neither an action nor a reply"):
        parse_choice({"content": "<think>never got to the answer"})


def test_compat_hides_reasoning_that_ends_in_a_lone_closing_tag(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A chat template that prefills "<think>" in the prompt (DeepSeek-R1 distills, QwQ
    # behind vLLM, llama.cpp or Ollama) returns the reasoning with only the closing tag.
    final = {"role": "assistant", "content": "They want a listing. I ran ls.\n</think>\n\nDone."}
    call, planner = first_turn(monkeypatch, Vendor(completion(final)))
    assert call == ToolCall("finish", {"summary": "Done."}, "Done.")
    assert planner.messages[-1] == final  # the history keeps the content as received
    assert compat.visible_text("plan</THINK>Listing.<think>more</think>") == "Listing."
    assert compat.visible_text([{"type": "text", "text": "plan\n</think>"}, {"text": "ok"}]) == "ok"
    # Paired blocks and plain text are read as before.
    assert compat.visible_text("<think>a</think>b<think>c</think>d") == "bd"
    assert compat.visible_text("2 </ 3, no tags") == "2 </ 3, no tags"
    with pytest.raises(LLMError, match="neither an action nor a reply"):
        parse_choice({"content": "only the reasoning arrived</think>"})


@pytest.mark.parametrize(
    ("rejects", "sent"),
    [
        (("parallel_tool_calls",), [(True, True), (False, True), (False, True)]),
        (("tool_choice",), [(True, True), (True, False), (True, False)]),
        (
            ("tool_choice", "parallel_tool_calls"),
            [(True, True), (True, False), (False, False), (False, False)],
        ),
    ],
)
def test_compat_drops_a_parameter_the_provider_rejects_and_remembers_it(
    monkeypatch: pytest.MonkeyPatch, rejects: tuple[str, ...], sent: list[tuple[bool, bool]]
) -> None:
    vendor = Vendor(
        completion(tool_message("wait", seconds=1)),
        completion({"role": "assistant", "content": "Done."}),
        rejects=rejects,
    )

    async def scenario(planner: OpenAICompatPlanner) -> None:
        assert (await planner.next(None)).tool == "wait"
        assert (await planner.next(ToolResult("waited"))).tool == "finish"

    against(vendor, scenario, monkeypatch)
    # Retried once per rejected parameter; the second turn does not send it again.
    assert [("parallel_tool_calls" in b, "tool_choice" in b) for b in vendor.bodies] == sent
    assert all(b["model"] == "some-model" and len(b["tools"]) == 10 for b in vendor.bodies)


def test_compat_does_not_retry_an_unrelated_bad_request(monkeypatch: pytest.MonkeyPatch) -> None:
    vendor = Vendor(http(400, {"error": {"message": "context length exceeded"}}))
    with pytest.raises(LLMError, match="API error 400: context length exceeded"):
        first_turn(monkeypatch, vendor)
    body = vendor.bodies[0]
    assert len(vendor.bodies) == 1
    assert body["tool_choice"] == "auto" and body["parallel_tool_calls"] is False


def test_compat_retries_transient_failures_and_honours_retry_after(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    ok = completion(tool_message("screenshot"))
    # Without Retry-After the backoff would be 1 s + 2 s.
    vendor = Vendor(
        http(429, {"error": {"message": "Rate limit exceeded"}}, **{"Retry-After": "0"}),
        http(503, **{"Retry-After": "0"}),
        ok,
    )
    started = time.monotonic()
    assert first_turn(monkeypatch, vendor)[0].tool == "screenshot"
    assert len(vendor.bodies) == 3 and time.monotonic() - started < 1.0
    for status in (408, 409, 429, 500, 502, 503, 504, 529):
        vendor = Vendor(http(status, {"error": {"message": "later"}}, **{"Retry-After": "0"}), ok)
        assert first_turn(monkeypatch, vendor)[0].tool == "screenshot", status
        assert len(vendor.bodies) == 2, status


def test_compat_gives_up_after_three_attempts(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(compat, "BACKOFF_CAP", 0.0)  # no real sleeping below
    assert compat.MAX_ATTEMPTS == 3
    ok = completion(tool_message("screenshot"))
    overloaded = http(500, {"error": {"message": "overloaded"}}, **{"Retry-After": "3600"})
    vendor = Vendor(overloaded, overloaded, overloaded)
    with pytest.raises(LLMError, match=r"^127\.0\.0\.1 API error 500: overloaded$"):
        first_turn(monkeypatch, vendor)
    assert len(vendor.bodies) == 3

    # Other 4xx answers are final, and so is a retryable one marked x-should-retry: false.
    for answer in (
        http(401, {"error": {"message": "bad key"}}),
        http(404, {"error": {"message": "no such model"}}),
        http(429, {"error": {"message": "out of credit"}}, **{"x-should-retry": "false"}),
    ):
        vendor = Vendor(answer)
        with pytest.raises(LLMError, match="API error 4"):
            first_turn(monkeypatch, vendor)
        assert len(vendor.bodies) == 1

    # A gateway's page or plain text is retried like any other answer with that status;
    # when it outlasts the attempts the owner is told so, not sent to check the Base URL.
    page = plain(502, "<html><h1>502 Bad Gateway</h1></html>")
    vendor = Vendor(page, page, ok)
    assert first_turn(monkeypatch, vendor)[0].tool == "screenshot"
    for answer, said in ((page, "Bad Gateway"), (plain(429, "Slow  down\n"), "Slow down")):
        vendor = Vendor(answer, answer, answer)
        unavailable = rf"^127\.0\.0\.1 is unavailable \(HTTP \d+, 3 attempts\): {said}$"
        with pytest.raises(LLMError, match=unavailable):
            first_turn(monkeypatch, vendor)
        assert len(vendor.bodies) == 3

    # A dropped connection is retried like a 5xx ...
    vendor = Vendor(HANG_UP, HANG_UP, ok)
    assert first_turn(monkeypatch, vendor)[0].tool == "screenshot"
    vendor = Vendor(HANG_UP, HANG_UP, HANG_UP)
    with pytest.raises(LLMError, match=r"^Could not reach 127\.0\.0\.1: \S"):
        first_turn(monkeypatch, vendor)
    assert len(vendor.bodies) == 3

    # ... and so is a timeout; the final one says how long each attempt waited.
    async def slow(_body: Any) -> web.Response:
        await asyncio.sleep(5)
        return web.json_response(ok)

    vendor = Vendor(slow, ok)
    assert first_turn(monkeypatch, vendor, request_timeout_sec=0.1)[0].tool == "screenshot"
    assert len(vendor.bodies) == 2
    vendor = Vendor(slow, slow, slow)
    waited = r"^127\.0\.0\.1 did not answer within 0\.1 s \(3 attempts\)\.$"
    with pytest.raises(LLMError, match=waited):
        first_turn(monkeypatch, vendor, request_timeout_sec=0.1)
    assert len(vendor.bodies) == 3


def image_urls(messages: list[dict[str, Any]]) -> list[bytes]:
    return [
        base64.b64decode(part["image_url"]["url"].removeprefix("data:image/jpeg;base64,"))
        for m in messages
        if isinstance(m.get("content"), list)
        for part in m["content"]
        if part["type"] == "image_url"
    ]


def test_compat_keeps_only_the_latest_screenshots_as_images(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shots = [b"\xff\xd8jpeg-%d" % n for n in range(5)]
    vendor = Vendor(
        *(completion(tool_message("screenshot", f"call_{n}")) for n in range(5)),
        completion({"role": "assistant", "content": "Done."}),
    )

    async def scenario(planner: OpenAICompatPlanner) -> None:
        result = None
        for n in range(5):
            assert (await planner.next(result)).call_id == f"call_{n}"
            result = ToolResult("", shots[n], 1280, 720)
        assert (await planner.next(result)).tool == "finish"

    planner = against(vendor, scenario, monkeypatch)
    assert compat.KEEP_SCREENSHOTS == 3
    sent = [image_urls(body["messages"]) for body in vendor.bodies]
    assert [len(images) for images in sent] == [0, 1, 2, 3, 3, 3]
    assert sent[4] == shots[1:4] and sent[5] == shots[2:5]  # always the most recent ones
    for body in vendor.bodies:
        messages = body["messages"]
        for i, m in enumerate(messages):
            if m["role"] == "tool":  # answers the one tool call right before it
                assert messages[i - 1]["role"] == "assistant"
                assert [c["id"] for c in messages[i - 1]["tool_calls"]] == [m["tool_call_id"]]
            if m.get("tool_calls") and i + 1 < len(messages):
                assert messages[i + 1]["role"] == "tool"
    last = vendor.bodies[-1]["messages"]
    assert [m["role"] for m in last] == ["system", "user"] + ["assistant", "tool", "user"] * 5
    # A pruned screenshot stays in place as one line of text with its size.
    for pruned in (last[4], last[7]):
        assert isinstance(pruned["content"], str)
        assert pruned["content"].startswith("Screenshot 1280x720.")
    assert last[10]["content"][0] == {"type": "text", "text": "Screenshot 1280x720."}
    # Nothing but the kept images stays in memory either.
    assert image_urls(planner.messages) == shots[2:5]


def test_compat_errors_name_the_vendor_and_never_the_key(monkeypatch: pytest.MonkeyPatch) -> None:
    # Provider "openai" with a base_url elsewhere: the host is what the owner is told.
    vendor = Vendor(http(401, {"error": {"message": f"Incorrect API key provided: {KEY}. " * 40}}))
    with pytest.raises(LLMError) as err:
        first_turn(monkeypatch, vendor)
    message = str(err.value)
    assert message.startswith("127.0.0.1 API error 401: Incorrect API key provided: [api key].")
    assert "openai" not in message and KEY not in message
    assert len(message) < 350  # a long provider message is cut
    assert vendor.auth == [f"Bearer {KEY}"]
    # Only error text is rewritten: an answer goes through as the model wrote it.
    echo = {"role": "assistant", "content": f"The clipboard holds {KEY}."}
    call, planner = first_turn(monkeypatch, Vendor(completion(echo)))
    assert call.args == {"summary": f"The clipboard holds {KEY}."} and planner.messages[-1] == echo
    # Without a base_url it is the provider's name.
    assert OpenAICompatPlanner(LLMConfig(provider="openrouter")).vendor == "openrouter"
    minimax = LLMConfig(provider="openai", base_url="https://api.minimax.io/v1/")
    assert OpenAICompatPlanner(minimax).vendor == "api.minimax.io"
    assert OpenAICompatPlanner(minimax).base_url == "https://api.minimax.io/v1"


# --- the kill switch against a request in flight (both adapters) -----------------

DEADLINE = 0.2  # request_timeout_sec in the kill tests


async def stalled(_body: Any) -> web.Response:
    await asyncio.sleep(30)
    return web.Response()


def an_action(anthropic: bool) -> dict[str, Any]:
    """What a retry would be answered with: the action a stopped agent must never get."""
    if anthropic:
        return tool_use("ssh_exec", command="rm -rf x")
    return completion(tool_message("ssh_exec", command="rm -rf x"))


BOTH = pytest.mark.parametrize("anthropic", [False, True], ids=["openai", "anthropic"])


@BOTH
def test_a_kill_in_the_tick_of_the_request_deadline_stops_the_planner(
    monkeypatch: pytest.MonkeyPatch, anthropic: bool
) -> None:
    """The kill switch is task.cancel(). Landing in the loop tick in which the request
    deadline expires, it finds the task cancelled once already; aiohttp's own timer
    reported that as a timeout, and the retry loop went on to ask the provider again."""
    vendor = Vendor(stalled, an_action(anthropic))
    seen: dict[str, Any] = {}

    async def scenario(planner: Any) -> None:
        turn = asyncio.ensure_future(planner.next(None))
        await wait_for(lambda: vendor.bodies)  # in flight, so its deadline is armed
        # The kill is due after the deadline, and the loop is held until both are
        # overdue: it then runs the deadline, the kill, and only then the task.
        asyncio.get_running_loop().call_later(DEADLINE, turn.cancel)
        time.sleep(DEADLINE + 0.05)
        killed = time.monotonic()
        seen["outcome"] = (await asyncio.gather(turn, return_exceptions=True))[0]
        seen["took"] = time.monotonic() - killed

    against(vendor, scenario, monkeypatch, anthropic, request_timeout_sec=DEADLINE)
    assert isinstance(seen["outcome"], asyncio.CancelledError), seen
    assert seen["took"] < 0.5 and len(vendor.bodies) == 1  # at once; nothing asked again


@BOTH
def test_a_kill_reported_as_a_timeout_by_a_layer_below_still_stops_the_planner(
    monkeypatch: pytest.MonkeyPatch, anthropic: bool
) -> None:
    """The planner does not rely on every layer under it getting cancellation right."""
    vendor = Vendor(stalled, an_action(anthropic))
    seen: dict[str, Any] = {}

    async def scenario(planner: Any) -> None:
        session = planner._session

        @contextlib.asynccontextmanager
        async def post(*args: Any, **kwargs: Any) -> Any:
            try:
                async with session.post(*args, **kwargs) as resp:
                    yield resp
            except asyncio.CancelledError:
                raise TimeoutError from None  # what aiohttp's nested timers did to a kill

        planner._session = SimpleNamespace(post=post, close=session.close)
        turn = asyncio.ensure_future(planner.next(None))
        await wait_for(lambda: vendor.bodies)
        turn.cancel()
        killed = time.monotonic()
        seen["outcome"] = (await asyncio.gather(turn, return_exceptions=True))[0]
        seen["took"] = time.monotonic() - killed

    against(vendor, scenario, monkeypatch, anthropic)
    assert isinstance(seen["outcome"], asyncio.CancelledError), seen
    assert seen["took"] < 0.5 and len(vendor.bodies) == 1


def test_a_request_deadline_without_a_kill_is_still_a_timeout_and_retried(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(claude, "BACKOFF_CAP", 0.0)  # no real sleeping below
    vendor = Vendor(stalled, tool_use("screenshot"))
    call, _ = first_turn(monkeypatch, vendor, anthropic=True, request_timeout_sec=0.1)
    assert call.tool == "screenshot" and len(vendor.bodies) == 2
    vendor = Vendor(stalled, stalled, stalled)
    with pytest.raises(LLMError, match=r"^Could not reach the Anthropic API \(network\)\.$"):
        first_turn(monkeypatch, vendor, anthropic=True, request_timeout_sec=0.1)
    assert len(vendor.bodies) == 3


# --- the Claude adapter against an endpoint that does not speak the Messages API ---

NOT_MESSAGES_FORMAT = (
    r"^127\.0\.0\.1 did not answer in Anthropic's Messages format \(HTTP {}\)\. With a Base "
    r"URL set, the protocol under Config > Agent & LLM usually has to be openai\.$"
)


@pytest.mark.parametrize(
    ("answer", "message"),
    [
        # Seen on the device: protocol anthropic with MiniMax's OpenAI-style Base URL.
        # The owner read "Extra data: line 1 column 5 (char 4)".
        (plain(404, "404 page not found"), NOT_MESSAGES_FORMAT.format(404)),
        (http(404, {"detail": "Not Found"}), NOT_MESSAGES_FORMAT.format(404)),
        (plain(405), NOT_MESSAGES_FORMAT.format(405)),
        (plain(200, "<html>Sign in</html>"), NOT_MESSAGES_FORMAT.format(200)),
        (plain(200), NOT_MESSAGES_FORMAT.format(200)),
        ([], NOT_MESSAGES_FORMAT.format(200)),
        # A chat-completions answer, and a "message" that is not one.
        (completion({"role": "assistant", "content": "Done."}), NOT_MESSAGES_FORMAT.format(200)),
        ({"content": "Done."}, NOT_MESSAGES_FORMAT.format(200)),
        (
            {"content": [{"type": "text", "text": "Done."}, "Done."]},
            NOT_MESSAGES_FORMAT.format(200),
        ),
        # An error the endpoint words is passed on, whatever shape it comes in ...
        (plain(401, "Unauthorized"), r"^The Anthropic API key was rejected\.$"),
        (plain(400, "upstream  said\nno"), r"^Anthropic API error 400: upstream said no$"),
        (plain(400, "<html>Bad Request</html>"), r"^Anthropic API error 400: Bad Request$"),
        (http(400, {"error": "a bare string"}), r"^Anthropic API error 400: a bare string$"),
        (http(400, {"detail": "nope"}), r'^Anthropic API error 400: \{"detail": "nope"\}$'),
        (
            {"type": "error", "error": {"type": "api_error", "message": "under HTTP 200"}},
            r"^Anthropic API error: under HTTP 200$",
        ),
        # ... on one line, cut short, and never with the key in it.
        (
            http(403, {"error": {"message": f"key {KEY} may\nnot. " + "x" * 5000}}),
            r"^The API key may not use claude-test: key \[api key\] may not\. x{277}$",
        ),
        (plain(400, f"bad header x-api-key: {KEY}"), r": bad header x-api-key: \[api key\]$"),
        # The Messages API's own 404 is about the model; another API's is about the path.
        (
            http(
                404, {"type": "error", "error": {"type": "not_found_error", "message": "model: x"}}
            ),
            r"^Unknown model 'claude-test'\. Check Config > Agent\.$",
        ),
        (
            http(404, {"error": {"message": "Invalid URL (POST /v1/v1/messages)", "type": "x"}}),
            r"^127\.0\.0\.1 API error 404: Invalid URL \(POST /v1/v1/messages\)\. With a Base URL "
            r"set, the protocol under Config > Agent & LLM usually has to be openai\.$",
        ),
        # Bytes that are not text stay out of what the owner reads.
        (
            plain(400, b"\x00\x1b[31mred\x07 alert\r\n"),
            r"^Anthropic API error 400: \[31mred alert$",
        ),
        # A well-formed answer with nothing in it.
        ({"content": [], "stop_reason": "end_turn"}, r"^The model returned neither an action"),
    ],
)
def test_claude_unexpected_answers_become_errors_the_owner_can_act_on(
    monkeypatch: pytest.MonkeyPatch, answer: Any, message: str
) -> None:
    vendor = Vendor(answer)
    with pytest.raises(LLMError, match=message) as err:
        first_turn(monkeypatch, vendor, anthropic=True)
    assert KEY not in str(err.value) and len(vendor.bodies) == 1  # none of these is retried


def test_claude_retries_a_retryable_status_whatever_its_body(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(claude, "BACKOFF_CAP", 0.0)  # no real sleeping below
    page = plain(502, "<html><h1>502 Bad Gateway</h1></html>")
    # Retry-After as an HTTP date is not a number of seconds: the usual backoff applies.
    dated = plain(529, "", **{"Retry-After": "Wed, 21 Oct 2026 07:28:00 GMT"})
    vendor = Vendor(page, dated, tool_use("screenshot"))
    assert first_turn(monkeypatch, vendor, anthropic=True)[0].tool == "screenshot"
    assert len(vendor.bodies) == 3
    vendor = Vendor(page, page, page)
    with pytest.raises(LLMError, match=r"^Anthropic API error 502: Bad Gateway$"):
        first_turn(monkeypatch, vendor, anthropic=True)
    assert len(vendor.bodies) == 3
    # ... and a dropped connection, which used to leave as aiohttp's own exception.
    vendor = Vendor(HANG_UP, HANG_UP, HANG_UP)
    with pytest.raises(LLMError, match=r"^Could not reach the Anthropic API \(network\)\.$"):
        first_turn(monkeypatch, vendor, anthropic=True)
    assert len(vendor.bodies) == 3


def test_claude_names_the_endpoint_and_hints_at_the_protocol_only_with_a_base_url() -> None:
    assert AnthropicPlanner(LLMConfig()).vendor == "api.anthropic.com"
    minimax = LLMConfig(base_url="https://api.minimax.io/v1")
    assert AnthropicPlanner(minimax).vendor == "api.minimax.io"
    with pytest.raises(LLMError) as err:
        claude.read_message(404, b"404 page not found", "api.anthropic.com")
    assert str(err.value) == (
        "api.anthropic.com did not answer in Anthropic's Messages format (HTTP 404)."
    )
    # An error in the Messages API's own envelope keeps its meaning for the caller.
    body = json.dumps(
        {"type": "error", "error": {"type": "not_found_error", "message": "model: x"}}
    )
    with pytest.raises(claude.ApiStatusError) as status:
        claude.read_message(404, body.encode(), "api.anthropic.com")
    assert (status.value.status, status.value.kind, status.value.message) == (
        404,
        "not_found_error",
        "model: x",
    )


def test_parse_message_tolerates_blocks_of_the_wrong_shape() -> None:
    message = {
        "content": [
            None,
            {"type": "text", "text": 5},
            {"type": "text", "text": " Clicking. "},
            {"type": "tool_use", "id": 7, "name": "Wait", "input": "one second"},
        ],
        "stop_reason": "tool_use",
    }
    call = claude.parse_message(message)
    assert call == ToolCall("wait", {}, "Clicking.") and call.call_id == "7"
    assert claude.parse_message({"content": [{"type": "tool_use", "name": None}]}).tool == ""
    for refusal in ({"stop_details": "because"}, {"stop_details": {"category": {"x": 1}}}, {}):
        with pytest.raises(LLMError, match=r"^The model declined this request \(policy\)\.$"):
            claude.parse_message({"stop_reason": "refusal", "content": "no"} | refusal)


# --- no response body may reach the owner as a Python error (both adapters) -------


def nested(levels: int) -> str:
    return "[" * levels + "]" * levels


# Valid JSON, 2000 levels deep, in each place either adapter looks.
DEEP = [
    '{"error": %s}',
    '{"error": {"message": %s}}',
    '{"choices": %s}',
    '{"choices": [{"error": %s, "message": {"content": "x"}}]}',
    '{"choices": [{"message": {"content": %s}}]}',
    '{"choices": [{"message": {"content": "Done.", "extra": %s}}]}',
    '{"choices": [{"message": {"tool_calls": [{"function": {"name": "wait", "arguments": %s}}]}}]}',
    '{"content": %s}',
    '{"content": [{"type": "text", "text": "Done.", "extra": %s}]}',
    '{"content": [{"type": "tool_use", "id": "t", "name": "wait", "input": %s}]}',
    '{"content": [], "stop_reason": "refusal", "stop_details": %s}',
]
ODD_SHAPES: list[Any] = [
    {},
    {"choices": "nope"},
    {"choices": {"0": {}}},
    {"choices": [None]},
    {"choices": [{"message": None}]},
    {"choices": [{"message": []}]},
    {"choices": [{"delta": {"content": "hi"}}]},
    {"choices": [{"message": {"tool_calls": "x"}}]},
    {"choices": [{"message": {"tool_calls": [None]}}]},
    {"choices": [{"message": {"tool_calls": [{"id": "a", "function": None}]}}]},
    {"choices": [{"message": {"content": 5}}]},
    {"choices": [{"message": {"content": {"text": "x"}}}]},
    {"choices": [{"message": {"content": [None, "x", {"type": "text"}, {"text": 5}]}}]},
    {"choices": [{"message": {"role": None, "content": "Done.", "refusal": {"x": 1}}}]},
    {"choices": [{"message": {"tool_calls": [{"id": 7, "function": {"name": {"x": 1}}}]}}]},
    {"choices": [{"message": {"tool_calls": [{"function": {"name": "wait", "arguments": 5}}]}}]},
    {"choices": [{"message": {"tool_calls": [{"function": {"arguments": "[" * 100_000}}]}}]},
    {"choices": [{"message": {"tool_calls": [{"function": {"arguments": '{"a":' * 50_000}}]}}]},
    {"choices": [{"error": ["a", {"b": None}], "message": {}}]},
    {"choices": [{"error": {"message": {"deep": ["x"]}}, "message": {}}]},
    {"choices": [{"finish_reason": ["length"], "message": {"content": None}}]},
    {"error": 500},
    {"error": True},
    {"error": ["a", "b"]},
    {"error": {}},
    {"error": {"message": {"deep": ["x"]}}},
    {"error": {"type": ["x"], "message": 5}},
    {"type": "error", "error": {"type": 7}},
    {"base_resp": {"status_code": "1004", "status_msg": None}},
    {"base_resp": "broken"},
    {"content": "text"},
    {"content": {"type": "text", "text": "x"}},
    {"content": [None, 5, "x"]},
    {"content": [{}]},
    {"content": [{"type": "text"}], "stop_reason": ["max_tokens"]},
    {"content": [{"type": "text", "text": 5}, {"type": "tool_use", "name": {"x": 1}, "id": []}]},
    {"content": [{"type": "tool_use", "name": None, "input": [1, 2]}]},
    {"content": [{"type": "tool_use", "name": "wait", "input": "soon"}]},
    {"content": [], "stop_reason": "refusal", "stop_details": "because"},
    {"content": None, "stop_reason": "max_tokens"},
]
ODD_BODIES: list[bytes] = [
    b"",
    b" \r\n",
    b"404 page not found",
    b"<!doctype html><html><body><h1>502 Bad Gateway</h1></body></html>",
    b"[]",
    b'"hello"',
    b"5",
    b"null",
    b"true",
    b"NaN",
    b'{"choices": NaN}',
    b"\xff\xfe\x00\x01 not text",
    b'{"error": "\xff\xfe"}',
    b'{"choices": [{"message": {"role": "assistant", "content": "cut of',  # truncated
    b'{"content": [{"type": "text", "text": "cut of',
    b'{"content": [{"type": "tool_use", "name": "wait", "input": {"seconds": 1}}]} trailing',
    b"[" * 100_000,  # too deep for the parser
    b'{"a":' * 50_000,
    f"Traceback: KeyError {KEY} ".encode() * 400,  # long, and quoting the key back
    *((shape % nested(2000)).encode() for shape in DEEP),
    *(json.dumps(shape).encode() for shape in ODD_SHAPES),
]
# What Python says when one of these is read carelessly.
PYTHONISH = (
    "Expecting value", "Extra data", "line 1 column", "recursion", "has no attribute",
    "NoneType", "not subscriptable", "indices must be", "' object", "unhashable", "codec can't",
)  # fmt: skip


def readable(text: str) -> bool:
    """One printable line, bounded, in nobody's programming language, and without the key."""
    return (
        text.isprintable()
        and len(text) <= 200 + compat.DETAIL_MAX  # our own words plus the provider's, cut
        and KEY not in text
        and not any(sign in text for sign in PYTHONISH)
    )


@BOTH
def test_no_response_body_reaches_the_owner_as_a_python_error(
    monkeypatch: pytest.MonkeyPatch, anthropic: bool
) -> None:
    # 200: read as an answer. 404: "nothing here". 400: an error. 503: retried twice.
    cases = [(status, body) for body in ODD_BODIES for status in (200, 404, 400, 503)]
    vendor = Vendor()
    for status, body in cases:
        vendor.script += [plain(status, body, **{"Retry-After": "0"})] * (3 if status == 503 else 1)
    outcomes: list[Any] = []

    async def scenario(planner: Any) -> None:
        for _ in cases:
            try:
                outcomes.append(await planner.next(None))
            except Exception as exc:  # noqa: BLE001 - which kind is the question
                outcomes.append(exc)

    against(vendor, scenario, monkeypatch, anthropic)
    assert not vendor.script  # every answer was asked for: the retries happened too
    bad = []
    for (status, body), outcome in zip(cases, outcomes, strict=True):
        if isinstance(outcome, ToolCall) or (
            isinstance(outcome, LLMError) and readable(str(outcome))
        ):
            continue
        bad.append((status, body[:60], repr(outcome)[:200]))
    assert not bad, bad
    # The fuzz is not vacuous: some of these bodies are answers.
    assert sum(isinstance(outcome, ToolCall) for outcome in outcomes) >= 3


# --- what may be stored in the LLM config and under which secret names ------------

EXAMPLES = Path(__file__).resolve().parents[1] / "config"


@pytest.mark.parametrize(
    "llm",
    [
        {"api_key_env": "sk-test-pasted-by-mistake-9999"},  # the key instead of its name
        {"api_key_env": ""},
        {"api_key_env": "MINIMAX-KEY"},
        {"api_key_env": "A" * 65},
        {"api_key_env": "LD_PRELOAD"},
        {"base_url": "https://user:sk-test-embedded@example.invalid/v1"},
        {"base_url": "https://sk-test-embedded@example.invalid/v1"},
        {"base_url": "https://example.invalid/v1?api_key=sk-test-embedded"},
        {"base_url": "https://example.invalid/v1#sk-test-embedded"},
        {"base_url": "not a url"},
        {"base_url": "example.invalid/v1"},
        {"base_url": "ftp://example.invalid/v1"},
        {"base_url": "https:///v1"},
        {"base_url": "http://example.invalid:port/v1"},
        {"base_url": "https://exa mple.invalid/v1"},
    ],
)
def test_llm_config_rejects_what_must_not_be_stored_or_echoed(llm: dict[str, str]) -> None:
    with pytest.raises(ValueError) as err:
        CoreConfig.model_validate({"llm": llm})
    # PUT /v1/config answers with this text: it names the field, never the input.
    assert "sk-test" not in str(err.value) and next(iter(llm)) in str(err.value)
    with pytest.raises(ValueError):
        LLMConfig(**llm)


def test_llm_config_accepts_real_endpoints_defaults_and_examples() -> None:
    for url in (
        "",
        "https://api.minimax.io/v1",
        "https://openrouter.ai/api/v1/",  # a trailing slash is fine
        "http://127.0.0.1:8080/v1",
        "http://[fd7a:115c:a1e0::1]:11434/v1",
    ):
        assert LLMConfig(base_url=url).base_url == url
    for name in ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "MINIMAX_API_KEY", "my_router_key", "_K"):
        assert LLMConfig(api_key_env=name).api_key_env == name
    # What a phone keyboard appends is dropped rather than refused.
    pasted = LLMConfig(api_key_env=" MINIMAX_API_KEY ", base_url="https://api.minimax.io/v1 \n")
    assert (pasted.api_key_env, pasted.base_url) == ("MINIMAX_API_KEY", "https://api.minimax.io/v1")
    assert CoreConfig.model_validate(CoreConfig().model_dump()) == CoreConfig()
    assert load_config(EXAMPLES / "lapis.example.toml").llm == LLMConfig()


def test_a_secret_name_cannot_shadow_the_process_environment(tmp_path: Path) -> None:
    path = tmp_path / "secrets.env"
    for name in (
        "PATH", "HOME", "USER", "SHELL", "LANG", "TZ", "LAPIS_CONFIG_DIR",
        "LD_PRELOAD", "LD_LIBRARY_PATH", "PYTHONPATH", "PYTHONSTARTUP", "DYLD_INSERT_LIBRARIES",
    ):  # fmt: skip
        with pytest.raises(ValueError, match=f"secret name {name} is reserved"):
            write_env_file(path, {"MINIMAX_API_KEY": "sk-test-123", name: "/tmp/evil"})
    assert not path.exists()  # nothing of a refused write is stored
    with pytest.raises(ValueError, match="invalid secret name"):
        write_env_file(path, {"MINIMAX_API_KEY\n": "sk-test-123"})
    # Every name the product itself uses stays allowed, the shipped examples included.
    names = {
        "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "MINIMAX_API_KEY", "OPENROUTER_API_KEY",
        "LAPIS_VNC_PASSWORD_macbook_pro", "LAPIS_VNC_USER_x", "LAPIS_TS_AUTHKEY",
        "LAPIS_TEST_KEY", "PATHFINDER_KEY", "HOME_ASSISTANT_TOKEN",
    }  # fmt: skip
    in_examples = set(read_env_file(EXAMPLES / "secrets.example.env"))
    in_examples |= {
        h.vnc.password_env for h in load_hosts(EXAMPLES / "hosts.example.toml").hosts.values()
    }
    assert {"ANTHROPIC_API_KEY", "LAPIS_VNC_PASSWORD_arch_dev", "LAPIS_TS_AUTHKEY"} <= in_examples
    allowed = dict.fromkeys(names | in_examples, "sk-test-123")
    assert not any(is_reserved_name(name) for name in allowed)
    write_env_file(path, allowed)
    assert read_env_file(path) == allowed


# -- a host without a working SSH login: the shell is not offered ----------------------


def test_without_ssh_the_prompt_and_the_tool_list_leave_the_shell_out() -> None:
    from lapis_harness.llm.base import system_prompt, tool_specs

    with_shell, without = system_prompt("macos", "mac"), system_prompt("macos", "mac", ssh=False)
    assert "Prefer ssh_exec" in with_shell and system_prompt("macos", "mac", True) == with_shell
    assert "ssh_exec" not in without and "There is no shell tool" in without
    assert 'computer named "mac"' in without and "macos" in without  # still filled in
    # Everything but that one rule is word for word the same.
    rule = next(line for line in with_shell.splitlines() if "ssh_exec" in line)
    assert [x for x in with_shell.splitlines() if x != rule] == [
        x for x in without.splitlines() if "no shell tool" not in x
    ]
    assert [t["name"] for t in tool_specs()] == list(UNIFIED_TOOLS)
    assert [t["name"] for t in tool_specs(ssh=False)] == [
        n for n in UNIFIED_TOOLS if n != "ssh_exec"
    ]
    assert [t["name"] for t in claude.TOOLS_NO_SSH] == [t["name"] for t in tool_specs(False)]
    assert [t["function"]["name"] for t in compat.TOOLS_NO_SSH] == [
        t["name"] for t in tool_specs(False)
    ]


@BOTH
def test_a_planner_told_there_is_no_ssh_does_not_offer_the_shell(
    monkeypatch: pytest.MonkeyPatch, anthropic: bool
) -> None:
    """The runner sets `ssh = False` before `start()` when the host's login fails."""

    def sent(ssh: bool) -> tuple[list[str], str]:
        monkeypatch.setattr(AnthropicPlanner if anthropic else OpenAICompatPlanner, "ssh", ssh)
        reply = tool_use("screenshot") if anthropic else completion(tool_message("screenshot"))
        vendor = Vendor(reply)
        call, _planner = first_turn(monkeypatch, vendor, anthropic)
        assert call.tool == "screenshot"
        body = vendor.bodies[0]
        if anthropic:
            return [t["name"] for t in body["tools"]], json.dumps(body["system"])
        return [t["function"]["name"] for t in body["tools"]], body["messages"][0]["content"]

    names, prompt = sent(True)
    assert "ssh_exec" in names and "Prefer ssh_exec" in prompt
    names, prompt = sent(False)
    assert "ssh_exec" not in names and len(names) == len(UNIFIED_TOOLS) - 1
    assert "ssh_exec" not in prompt and "There is no shell tool" in prompt
