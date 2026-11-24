"""Claude Computer Use adapter behind `LLMClient` (SPEC Phase 3)."""

from __future__ import annotations

import os
from typing import Any

from lapis_daemon.llm.base import LLMClient, Observation, ToolCall

TOOLS_SCHEMA: list[dict[str, Any]] = [
    {
        "name": "screenshot",
        "description": "Capture the remote display (downscaled JPEG already attached).",
        "input_schema": {"type": "object", "properties": {}},
    },
    {
        "name": "mouse_click",
        "description": "Click at (x, y) in the downscaled frame.",
        "input_schema": {
            "type": "object",
            "properties": {
                "x": {"type": "integer"},
                "y": {"type": "integer"},
                "button": {"type": "string", "enum": ["left", "right", "middle"]},
                "clicks": {"type": "integer"},
            },
            "required": ["x", "y"],
        },
    },
    {
        "name": "type_text",
        "description": "Type text into the focused field.",
        "input_schema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "key_press",
        "description": "Press a key or a modifier combo such as ctrl+c.",
        "input_schema": {
            "type": "object",
            "properties": {"combo": {"type": "string"}},
            "required": ["combo"],
        },
    },
    {
        "name": "scroll",
        "description": "Scroll at (x, y) by (dx, dy).",
        "input_schema": {
            "type": "object",
            "properties": {
                "x": {"type": "integer"},
                "y": {"type": "integer"},
                "dx": {"type": "integer"},
                "dy": {"type": "integer"},
            },
            "required": ["x", "y", "dx", "dy"],
        },
    },
    {
        "name": "ssh_exec",
        "description": "Run a non-interactive shell command on the host.",
        "input_schema": {
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        },
    },
    {
        "name": "wait",
        "description": "Wait for the UI to settle.",
        "input_schema": {
            "type": "object",
            "properties": {"seconds": {"type": "number"}},
            "required": ["seconds"],
        },
    },
    {
        "name": "finish",
        "description": "Signal task completion.",
        "input_schema": {
            "type": "object",
            "properties": {"summary": {"type": "string"}},
            "required": ["summary"],
        },
    },
]


class AnthropicCuaClient(LLMClient):
    """Calls the Anthropic Messages API with computer-use-style tool definitions."""

    def __init__(
        self,
        api_key_env: str = "ANTHROPIC_API_KEY",
        model: str = "claude-sonnet-4-20250514",
        max_tokens: int = 1024,
    ) -> None:
        self.api_key_env = api_key_env
        self.model = model
        self.max_tokens = max_tokens

    async def act(self, task: str, obs: Observation) -> ToolCall:
        import anthropic  # lazy: live adapter requires the SDK

        api_key = os.environ.get(self.api_key_env)
        if not api_key:
            raise RuntimeError(f"{self.api_key_env} is not set")
        client = anthropic.Anthropic(api_key=api_key)

        content: list[dict[str, Any]] = [{"type": "text", "text": task}]
        if obs.screenshot_jpeg_b64:
            content.append(
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/jpeg",
                        "data": obs.screenshot_jpeg_b64,
                    },
                }
            )
        response = client.messages.create(
            model=self.model,
            max_tokens=self.max_tokens,
            tools=TOOLS_SCHEMA,
            messages=[{"role": "user", "content": content}],
        )
        return self.parse_tool_use(response.model_dump())

    @staticmethod
    def parse_tool_use(response: dict[str, Any]) -> ToolCall:
        """Extract the first tool_use block from an Anthropic message dict (pure/testable)."""
        for block in response.get("content") or []:
            if block.get("type") == "tool_use":
                return ToolCall(block["name"], dict(block.get("input") or {}))
        raise ValueError("no tool_use block in LLM response")
