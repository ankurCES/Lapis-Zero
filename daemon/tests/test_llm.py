from __future__ import annotations

import pytest

from lapis_daemon.llm.anthropic_cua import TOOLS_SCHEMA, AnthropicCuaClient
from lapis_daemon.llm.base import ToolCall


def test_tool_schema_covers_unified_surface() -> None:
    names = {t["name"] for t in TOOLS_SCHEMA}
    assert {
        "screenshot",
        "mouse_click",
        "type_text",
        "key_press",
        "scroll",
        "ssh_exec",
        "wait",
        "finish",
    } <= names


def test_parse_tool_use_returns_first_block() -> None:
    resp = {
        "content": [
            {"type": "text", "text": "reasoning"},
            {"type": "tool_use", "name": "mouse_click", "input": {"x": 10, "y": 20}},
            {"type": "tool_use", "name": "finish", "input": {"summary": "done"}},
        ]
    }
    call = AnthropicCuaClient.parse_tool_use(resp)
    assert call == ToolCall("mouse_click", {"x": 10, "y": 20})


def test_parse_tool_use_raises_without_block() -> None:
    with pytest.raises(ValueError, match="tool_use"):
        AnthropicCuaClient.parse_tool_use({"content": [{"type": "text", "text": "none"}]})
