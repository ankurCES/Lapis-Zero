"""LLM adapter interface and the unified tool surface (SPEC 7.3)."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any

# The tool surface the LLM is allowed to reason over. The runner maps these names
# onto HostDriver/VncDriver/SshDriver calls in the live loop (wired in a later phase).
UNIFIED_TOOLS = (
    "screenshot",
    "mouse_click",
    "mouse_move",
    "mouse_drag",
    "scroll",
    "type_text",
    "key_press",
    "ssh_exec",
    "wait",
    "finish",
)


@dataclass
class ToolCall:
    tool: str
    args: dict[str, Any] = field(default_factory=dict)
    thought: str = ""


@dataclass
class Observation:
    """What the model sees each step: one downscaled JPEG plus optional context."""

    screenshot_jpeg_b64: str | None = None
    width: int = 0
    height: int = 0
    activity: str = ""


class LLMClient(ABC):
    """Async adapter: turn a task + the current observation into the next tool call."""

    @abstractmethod
    async def act(self, task: str, obs: Observation) -> ToolCall: ...
