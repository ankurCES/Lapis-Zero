"""Provider-neutral LLM interface and the unified tool surface (SPEC 7.3)."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any

_INTENT = {
    "type": "string",
    "description": "Short plain description shown to the owner, e.g. 'Click Retry Build'.",
}
_RISK = {
    "type": "string",
    "enum": ["low", "high"],
    "description": "'high' if destructive, irreversible, sends/publishes, spends money, "
    "changes security settings, or enters a password or other secret.",
}


def _tool(name: str, description: str, props: dict[str, Any], required: list[str]) -> dict:
    return {
        "name": name,
        "description": description,
        "parameters": {"type": "object", "properties": props, "required": required},
    }


_XY = {"x": {"type": "integer"}, "y": {"type": "integer"}}

# One schema list for every provider; adapters only rename the envelope keys.
TOOL_SPECS: list[dict[str, Any]] = [
    _tool("screenshot", "Capture the remote display.", {}, []),
    _tool(
        "mouse_click",
        "Click at (x, y) in the latest screenshot. Returns a fresh screenshot.",
        {
            **_XY,
            "button": {"type": "string", "enum": ["left", "right", "middle"]},
            "clicks": {"type": "integer", "minimum": 1, "maximum": 3},
            "intent": _INTENT,
            "risk": _RISK,
        },
        ["x", "y", "intent"],
    ),
    _tool("mouse_move", "Move the pointer to (x, y).", _XY, ["x", "y"]),
    _tool(
        "mouse_drag",
        "Press at (x1, y1), drag to (x2, y2), release.",
        {
            "x1": {"type": "integer"},
            "y1": {"type": "integer"},
            "x2": {"type": "integer"},
            "y2": {"type": "integer"},
            "intent": _INTENT,
            "risk": _RISK,
        },
        ["x1", "y1", "x2", "y2", "intent"],
    ),
    _tool(
        "scroll",
        "Scroll at (x, y): dy > 0 scrolls down, dx > 0 scrolls right (wheel notches).",
        {**_XY, "dx": {"type": "integer"}, "dy": {"type": "integer"}},
        ["x", "y", "dx", "dy"],
    ),
    _tool(
        "type_text",
        "Type text into the focused field.",
        {"text": {"type": "string"}, "intent": _INTENT, "risk": _RISK},
        ["text", "intent"],
    ),
    _tool(
        "key_press",
        "Press one key or a combo such as 'ctrl+c', 'cmd+space', 'Return', 'Escape'.",
        {"combo": {"type": "string"}, "intent": _INTENT, "risk": _RISK},
        ["combo", "intent"],
    ),
    _tool(
        "ssh_exec",
        "Run a non-interactive shell command on the host over SSH; returns its output.",
        {
            "command": {"type": "string"},
            "timeout_s": {"type": "integer", "minimum": 1, "maximum": 120},
        },
        ["command"],
    ),
    _tool(
        "wait",
        "Wait for the UI to settle (1-15 seconds).",
        {"seconds": {"type": "number"}},
        ["seconds"],
    ),
    _tool(
        "finish",
        "End the task with a short summary of what was done and what was not.",
        {"summary": {"type": "string"}},
        ["summary"],
    ),
]
UNIFIED_TOOLS = tuple(spec["name"] for spec in TOOL_SPECS)

_SSH_RULE = (
    "- Prefer ssh_exec for anything a shell command can do; it is faster and more "
    "reliable than driving the GUI. Use the GUI tools for what only the GUI can do.\n"
)
_NO_SSH_RULE = (
    "- There is no shell tool: SSH is not set up for this computer. Do everything "
    "through the screen with the mouse and keyboard tools, opening a terminal window "
    "there if the task needs a command line.\n"
)

SYSTEM_PROMPT = (
    """\
You operate a remote {os} computer named "{host}" on behalf of its owner, who is \
watching from a small handheld controller and their phone. You see the machine \
only through screenshots and act on it only through the tools provided.

How to work:
"""
    + _SSH_RULE
    + """\
- Coordinates are pixels in the most recent screenshot. Take a screenshot before \
your first GUI action; each GUI action returns a fresh one.
- Call exactly one tool per turn.
- Give every GUI action an `intent`: a few plain words the owner will read on a \
tiny screen. Mark `risk` as "high" for anything destructive, irreversible, that \
sends or publishes, spends money, changes security settings, or enters a password \
or other secret. The owner approves those on the device before they run.
- If the owner denies an action, do not attempt the same thing another way. \
Adapt, or finish and say what was left undone.
- When the task is complete, or you cannot make further progress, call finish \
with a short summary: what you did, what you found, and anything that failed.
"""
)


def system_prompt(os: str, host: str, ssh: bool = True) -> str:
    """The instructions for one task; without `ssh` the shell tool is not mentioned."""
    text = SYSTEM_PROMPT if ssh else SYSTEM_PROMPT.replace(_SSH_RULE, _NO_SSH_RULE)
    return text.format(os=os, host=host)


def tool_specs(ssh: bool = True) -> list[dict[str, Any]]:
    """The tools offered for one task: all of them, or all but the shell."""
    return [spec for spec in TOOL_SPECS if ssh or spec["name"] != "ssh_exec"]


@dataclass
class ToolCall:
    tool: str
    args: dict[str, Any] = field(default_factory=dict)
    thought: str = ""
    call_id: str = field(default="", compare=False)


@dataclass
class ToolResult:
    """What a tool call produced, fed back to the model on the next turn."""

    text: str = ""
    jpeg: bytes | None = None
    width: int = 0
    height: int = 0
    is_error: bool = False
    denied: bool = False
    hint: str | None = None  # harness notice (stuck-loop corrective), not tool output


class LLMError(RuntimeError):
    """The provider could not produce a next action (auth, network, refusal, ...)."""


class Planner(ABC):
    """Chooses the next tool call for one task, given the previous call's result."""

    # Set to False by the runner before `start()` when the host has no working SSH
    # login: the shell tool is then neither offered nor recommended. A model that is
    # offered a tool that always fails keeps calling it until the task is stopped.
    ssh: bool = True

    @abstractmethod
    async def start(
        self, task: str, *, host: str, host_os: str, history: list[dict[str, str]]
    ) -> None: ...

    @abstractmethod
    async def next(self, result: ToolResult | None) -> ToolCall: ...

    async def close(self) -> None:  # noqa: B027 - optional hook
        """Release provider resources."""


def build_planner(llm_config: Any) -> Planner:
    """Planner for the configured provider (SDKs are imported lazily)."""
    if llm_config.provider == "anthropic":
        from lapis_daemon.llm.anthropic_cua import AnthropicPlanner

        return AnthropicPlanner(llm_config)
    from lapis_daemon.llm.openai_cua import OpenAICompatPlanner

    return OpenAICompatPlanner(llm_config)
