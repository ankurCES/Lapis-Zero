"""Local cyberdeck operator mode: a chat on the built-in `deck` host runs the daemon's tools."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from conftest import make_core, wait_for
from fakedaemon import TOOLS, FakeDaemon, closed_port_url, held
from test_api import PHONE, client_for

from lapis_harness import deck
from lapis_harness.agent.runner import MockPlanner, TaskRunner
from lapis_harness.config import mock_hosts
from lapis_harness.daemon_client import DaemonClient
from lapis_harness.db import LapisDb
from lapis_harness.deck import DaemonDeckTools
from lapis_harness.llm.anthropic_cua import AnthropicPlanner
from lapis_harness.llm.base import ToolCall
from lapis_harness.llm.openai_cua import OpenAICompatPlanner


class FakeDeck:
    """The daemon's tool surface, in process."""

    def __init__(self, answers: dict[str, dict[str, Any]] | None = None) -> None:
        self.answers = answers or {}
        self.calls: list[tuple[str, dict[str, Any], dict[str, Any]]] = []
        self.release: asyncio.Event | None = None

    async def list_tools(self) -> list[dict[str, Any]]:
        return TOOLS

    async def invoke(
        self, tool: str, args: dict[str, Any], context: dict[str, Any]
    ) -> dict[str, Any]:
        self.calls.append((tool, args, context))
        if self.release is not None:
            await self.release.wait()
        return self.answers.get(tool) or {"ok": True, "tier": 0, "decision": "auto",
                                           "result": {"tool": tool}}  # fmt: skip


class Watched(MockPlanner):
    """Speaks the LLM's names (`sdr__scan_band`) and remembers what it was offered."""

    def __init__(self, script: tuple[ToolCall, ...]) -> None:
        super().__init__(script)
        self.results: list[Any] = []

    async def next(self, result: Any) -> ToolCall:
        self.results.append(result)
        call = await super().next(result)
        return ToolCall(deck.llm_name(call.tool), call.args)


def runner_for(db: LapisDb, tools: Any, **kwargs: Any) -> tuple[TaskRunner, list, list]:
    events: list[dict[str, Any]] = []
    finished: list[dict[str, Any]] = []

    def no_driver(name: str) -> Any:
        raise AssertionError("a deck chat never opens a fleet driver")

    runner = TaskRunner(
        db, mock_hosts(), emit_event=events.append, emit_finish=finished.append,
        driver_factory=no_driver, deck_tools=tools, **kwargs,
    )  # fmt: skip
    return runner, events, finished


async def test_the_deck_chat_runs_every_tool_through_the_daemon(tmp_path: Path) -> None:
    db = LapisDb(tmp_path / "lapis.db")
    await db.init()
    tools = FakeDeck()
    planner = Watched(deck.DECK_SCRIPT)
    runner, events, finished = runner_for(db, tools, deck_planner_factory=lambda: planner)
    task_id = await runner.submit_task("deck", "Where am I and what is on the air?", chat_id="c_d")
    await asyncio.wait_for(runner.wait_idle(), 5)
    # Offered: the daemon's tools under names a provider accepts, and finish.
    assert [t["name"] for t in planner.tools or []] == [
        "gps__get_fix", "sdr__scan_band", "mesh__list_nodes", "mesh__send_message",
        "hw__gpio_rail", "finish",
    ]  # fmt: skip
    assert planner.instructions and "cyberdeck" in planner.instructions
    assert planner.tools[3]["description"].endswith("(Tier 1: needs the owner's approval)")
    # Every call went to the daemon, under its own dotted name, with the chat and task.
    context = {"chat_id": "c_d", "task_id": task_id, "source": "harness"}
    assert tools.calls == [(c.tool, c.args, context) for c in deck.DECK_SCRIPT]
    assert [(e["kind"], e["status"]) for e in events] == [("prompt", "ok")] + [
        ("deck_tool", "ok")
    ] * 3
    assert events[2]["summary"] == (
        'sdr.scan_band {"end_hz":101000000,"start_hz":99000000,"step_hz":1000000}'
    )
    assert events[1]["detail"] == '{"tool":"gps.get_fix"}'
    assert planner.results[1].text == '{"tool":"gps.get_fix"}'  # what the model reads
    assert (finished[0]["status"], finished[0]["host"]) == ("done", "deck")
    assert finished[0]["summary"] == "Mock run complete: 3 scripted steps."
    assert (await db.get_task(task_id))["status"] == "done"


async def test_a_refused_tool_is_not_retried_and_waiting_shows(tmp_path: Path) -> None:
    db = LapisDb(tmp_path / "lapis.db")
    await db.init()
    refused = {"ok": False, "tier": 0, "decision": "deny", "error": "the owner said no"}
    tools = FakeDeck({"sdr.scan_band": refused})
    runner, events, finished = runner_for(db, tools)
    await runner.submit_task("deck", "scan")
    await asyncio.wait_for(runner.wait_idle(), 5)
    assert [c[0] for c in tools.calls] == ["gps.get_fix", "sdr.scan_band"]
    assert (events[-1]["status"], events[-1]["detail"]) == ("denied", "deny: the owner said no")
    assert (finished[0]["status"], finished[0]["summary"]) == (
        "cancelled",
        "Stopped: the last action was denied.",
    )
    # A call the daemon holds for the owner shows as WAITING_APPROVAL meanwhile.
    tools = FakeDeck()
    tools.release = asyncio.Event()
    runner, events, finished = runner_for(db, tools)
    await runner.submit_task("deck", "slow")
    await wait_for(lambda: runner.agent_state == "WAITING_APPROVAL")
    tools.release.set()
    await asyncio.wait_for(runner.wait_idle(), 5)
    assert finished[0]["status"] == "done"
    # A deck chat never hands the message to an agent CLI.
    from lapis_harness.agent.harness import BUILTIN
    from lapis_harness.agent.runner import AgentJob

    try:
        await runner.submit_task("deck", "x", agent=AgentJob(BUILTIN["claude"]))
    except ValueError as exc:
        assert "fleet hosts" in str(exc)
    else:
        raise AssertionError("an agent tool on the deck must be refused")


async def test_without_the_daemon_nothing_runs(tmp_path: Path) -> None:
    db = LapisDb(tmp_path / "lapis.db")
    await db.init()
    (tmp_path / "local.token").write_text("t" * 32)
    gone = DaemonDeckTools(DaemonClient(closed_port_url(), tmp_path / "local.token"))
    runner, events, finished = runner_for(db, gone)
    await runner.submit_task("deck", "x")
    await asyncio.wait_for(runner.wait_idle(), 5)
    assert finished[0]["status"] == "failed"
    assert finished[0]["summary"].startswith("Could not list the cyberdeck tools")
    # Listing works but the invoke cannot be made: a denial (fail closed).
    reply = await gone.invoke("gps.get_fix", {}, {})
    assert (reply["ok"], reply["decision"]) == (False, "deny")
    await gone.client.close()


def test_the_planners_offer_the_deck_tools_when_told() -> None:
    from lapis_harness.config import LLMConfig

    specs = deck.llm_specs(TOOLS)
    claude = AnthropicPlanner(LLMConfig())
    claude.use_tools(specs, deck.system_prompt())
    body, _ = claude._request()
    assert [t["name"] for t in body["tools"]][:2] == ["gps__get_fix", "sdr__scan_band"]
    assert body["tools"][1]["input_schema"]["required"] == ["start_hz", "end_hz"]
    compat = OpenAICompatPlanner(LLMConfig(provider="openai", api_key_env="OPENAI_API_KEY"))
    assert [t["function"]["name"] for t in compat._tools()][-1] == "finish"
    compat.use_tools(specs, "x")
    assert compat._tools()[0] == {"type": "function", "function": specs[0]}
    # Without it: the fleet KVM tools, as before.
    assert "mouse_click" in [t["name"] for t in AnthropicPlanner(LLMConfig())._tools()]
    assert deck.tool_name("sdr__scan_band") == "sdr.scan_band"


async def test_the_deck_chat_over_the_api_with_the_daemons_tools(short_tmp: Path) -> None:
    async with FakeDaemon(short_tmp) as daemon:
        release = asyncio.Event()
        core, client = await client_for(short_tmp, deck_tools=DaemonDeckTools(daemon.client()))
        ws = await client.ws_connect("/v1/ws", headers=PHONE)
        chat = await (
            await client.post("/v1/chats", json={"host_id": "deck"}, headers=PHONE)
        ).json()
        assert chat["host_id"] == "deck"
        bad = await client.post(
            "/v1/chats", json={"host_id": "deck", "agent": "claude"}, headers=PHONE
        )
        assert bad.status == 422
        bad = await client.patch(f"/v1/chats/{chat['id']}", json={"agent": "claude"}, headers=PHONE)
        assert bad.status == 422

        # A Tier 1 tool: held at the daemon until the owner answers there.
        daemon.invoke = lambda body: held(
            release, {"ok": True, "tier": 1, "decision": "approve_once", "result": {"sent": 1}}
        )
        core.runner.deck_planner_factory = lambda: MockPlanner(
            (ToolCall("mesh.send_message", {"text": "deck online"}),)
        )
        resp = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "Say hi on the mesh"},
            headers=PHONE,
        )  # fmt: skip
        assert resp.status == 202
        await wait_for(lambda: core.runner.agent_state == "WAITING_APPROVAL")
        listing = await (await client.get("/v1/chats", headers=PHONE)).json()
        assert [(c["active"], c["pending_approvals"]) for c in listing["chats"]] == [(True, 1)]
        state = await (await client.get("/v1/state", headers=PHONE)).json()
        assert state["pending_approvals"][0]["action"] == "mesh.send_message"
        release.set()
        await wait_for(lambda: not core.runner.is_running)
        await core._chat_q.join()
        assert daemon.invoked == [
            {"tool": "mesh.send_message", "args": {"text": "deck online"},
             "context": {"chat_id": chat["id"], "task_id": daemon.invoked[0]["context"]["task_id"],
                         "source": "harness"}}
        ]  # fmt: skip
        thread = await (await client.get(f"/v1/chats/{chat['id']}", headers=PHONE)).json()
        rows = [(m["role"], m["content"]) for m in thread["messages"]]
        assert rows[0] == ("user", "Say hi on the mesh")
        assert ("tool", 'mesh.send_message {"text":"deck online"}') in rows
        assert rows[-1] == ("assistant", "Mock run complete: 1 scripted steps.")

        # The deck as a host: probed through its tools, no SSH, no remote screen.
        probe = await (await client.post("/v1/hosts/deck/probe", headers=PHONE)).json()
        assert probe["deck"] == {"ok": True, "latency_ms": probe["deck"]["latency_ms"],
                                 "detail": "5 cyberdeck tools"}  # fmt: skip
        assert (await client.get("/v1/hosts/deck/agents", headers=PHONE)).status == 409
        assert (await client.post("/v1/hosts/deck/ssh/authorize", headers=PHONE)).status == 422
        assert (await client.get("/v1/hosts/deck/screen.jpg", headers=PHONE)).status == 409
        resp = await client.put("/v1/agent/active-host", json={"host_id": "deck"}, headers=PHONE)
        assert (await resp.json()) == {"active_host": "deck"}
        await ws.close()
        await client.close()
        await core.stop()
        await daemon.client().close()


async def test_mock_mode_has_a_deck_script(short_tmp: Path) -> None:
    core = await make_core(short_tmp, deck_tools=FakeDeck())
    assert core.host_ids()[-1] == "deck"
    planner = core._deck_planner()
    assert isinstance(planner, MockPlanner) and planner.script == deck.DECK_SCRIPT
    assert [c.tool for c in deck.DECK_SCRIPT] == ["gps.get_fix", "sdr.scan_band", "mesh.list_nodes"]
    await core.stop()
