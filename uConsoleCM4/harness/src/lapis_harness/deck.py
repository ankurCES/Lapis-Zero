"""Local cyberdeck operator mode (SPEC 5.2): a chat bound to the `deck` host.

The configured LLM gets the daemon's cyberdeck tools (`sdr.*`, `mesh.*`, `gps.*`, `net.*`,
`hw.*`) as function-calling tools, and every call it makes is run by the daemon through
`POST /v1/deck/tools/invoke`, which puts it to the Policy Gate (and the owner, at Tier 1/2)
before it runs. That client is the agent loop's only executor for deck tools: nothing in
the harness touches the radios, the GPIO rails or the shell of the uConsole itself.

Tool names contain dots; LLM providers allow `[a-zA-Z0-9_-]` only, so `.` is sent as `__`
and mapped back (`llm_name` / `tool_name`), as the MCP server does.
"""

from __future__ import annotations

import json
from typing import Any, Protocol

from lapis_harness.daemon_client import DaemonClient
from lapis_harness.llm.base import TOOL_SPECS, ToolCall

# The mock planner's deck run: what `--mock` shows without an LLM.
DECK_SCRIPT: tuple[ToolCall, ...] = (
    ToolCall("gps.get_fix", {}),
    ToolCall(
        "sdr.scan_band",
        {"start_hz": 99_000_000, "end_hz": 101_000_000, "step_hz": 1_000_000},
    ),
    ToolCall("mesh.list_nodes", {}),
)
FINISH_SPEC = next(spec for spec in TOOL_SPECS if spec["name"] == "finish")
SEP = "__"
SUMMARY_MAX = 160

SYSTEM_PROMPT = """\
You operate the owner's ClockworkPi uConsole cyberdeck ("{host}") through typed tools for \
its own hardware: software-defined radio, LoRa/Meshtastic mesh, GPS, passive network \
surveys, power rails and a local shell. The owner is watching on the deck's screen and \
their phone.

How to work:
- Call exactly one tool per turn and read its result before the next.
- Prefer passive, read-only tools. Transmitting, probing the network, switching power \
rails and shell commands need the owner's approval; the daemon asks them for you.
- If a call is denied, do not try the same thing another way. Adapt, or finish and say \
what was left undone.
- When the task is complete, or you cannot make further progress, call finish with a \
short summary of what you found.
"""


class DeckTools(Protocol):
    """The daemon's cyberdeck tool surface (`DaemonDeckTools`; a fake in tests)."""

    async def list_tools(self) -> list[dict[str, Any]]: ...

    async def invoke(
        self, tool: str, args: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]: ...


class DaemonDeckTools:
    """`GET /v1/deck/tools` and `POST /v1/deck/tools/invoke` on lapis-daemon."""

    def __init__(self, client: DaemonClient) -> None:
        self.client = client

    async def list_tools(self) -> list[dict[str, Any]]:
        return await self.client.deck_tools()

    async def invoke(
        self, tool: str, args: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        return await self.client.invoke(tool, args, context)


def llm_name(name: str) -> str:
    return name.replace(".", SEP)


def tool_name(name: str) -> str:
    return name.replace(SEP, ".")


def llm_specs(tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The daemon's tool list as the planners' tool specs, plus `finish`."""
    specs = []
    for tool in tools:
        schema = tool.get("input_schema")
        tier = tool.get("tier")
        note = f" (Tier {tier}: needs the owner's approval)" if tier in (1, 2) else ""
        specs.append(
            {
                "name": llm_name(str(tool["name"])),
                "description": f"{tool.get('description') or tool['name']}{note}",
                "parameters": schema
                if isinstance(schema, dict)
                else {"type": "object", "properties": {}},
            }
        )
    return [*specs, FINISH_SPEC]


def system_prompt(host: str = "deck") -> str:
    return SYSTEM_PROMPT.format(host=host)


def summary(call: ToolCall) -> str:
    """The ticker line of one deck tool call: its name and its arguments, compact."""
    if not call.args:
        return call.tool
    args = json.dumps(call.args, separators=(",", ":"), sort_keys=True, default=str)
    return f"{call.tool} {args}"[:SUMMARY_MAX]
