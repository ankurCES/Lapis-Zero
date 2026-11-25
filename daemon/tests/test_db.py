from __future__ import annotations

import asyncio
import sqlite3
from pathlib import Path

from lapis_daemon.db import LapisDb


def test_wal_and_roundtrip(tmp_path: Path) -> None:
    async def scenario() -> None:
        async with LapisDb(tmp_path / "lapis.db") as db:
            await db.insert_task("t_1", "macbook-pro", "check disk", "balanced", 1.0)
            for seq in range(1, 4):
                await db.insert_step("t_1", seq, "ssh_exec", f"cmd {seq}", "out", "ok", 1.0 + seq)
            await db.update_task_status("t_1", "done", 9.0)

            task = await db.get_task("t_1")
            assert task is not None
            assert (task["status"], task["policy"], task["updated_at"]) == ("done", "balanced", 9.0)

            steps = await db.list_recent_steps("t_1", 2)
            assert [s["summary"] for s in steps] == ["cmd 2", "cmd 3"]
            assert steps[0]["kind"] == "ssh_exec"

            await db.add_schedule("Daily Digest", "0 8 * * *", "macbook-pro", "digest", False)
            assert await db.list_schedules() == [
                {
                    "name": "Daily Digest",
                    "cron": "0 8 * * *",
                    "host": "macbook-pro",
                    "prompt": "digest",
                    "enabled": 0,
                }
            ]

    asyncio.run(scenario())
    conn = sqlite3.connect(tmp_path / "lapis.db")
    assert conn.execute("PRAGMA journal_mode").fetchone()[0] == "wal"
    conn.close()


# The `chats` table as the first release created it (before chats could be bound to a tool).
OLD_CHATS = """
CREATE TABLE chats(
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    host_id TEXT NOT NULL,
    model_override TEXT,
    policy_override TEXT,
    archived INTEGER NOT NULL DEFAULT 0,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
INSERT INTO chats VALUES ('c_old', 'From last week', 'macbook-pro', NULL, 'strict', 0, 1.0, 2.0);
"""


def test_a_database_from_before_tool_chats_is_migrated_in_place(tmp_path: Path) -> None:
    from lapis_daemon.chat_manager import ChatManager

    path = tmp_path / "lapis.db"
    conn = sqlite3.connect(path)
    conn.executescript(OLD_CHATS)
    conn.commit()
    conn.close()

    async def scenario() -> None:
        db = LapisDb(path)
        await db.init()
        await db.init()  # every start runs it: the second time there is nothing to add
        columns = {r["name"]: r for r in await db.run("PRAGMA table_info(chats)")}
        assert {"agent", "agent_session", "workdir"} <= set(columns)
        chats = ChatManager(db, lambda _msg: None)
        # The chat that was there is the device's own agent's, as it always was.
        old = await chats.get("c_old")
        assert old is not None and (old["agent"], old["workdir"]) == ("lapis", "")
        assert (old["title"], old["policy_override"]) == ("From last week", "strict")
        assert await chats.agent_session("c_old") == "" and "agent_session" not in old
        # And it can be bound to a tool now.
        bound = await chats.update("c_old", agent="claude", workdir="~/code")
        assert bound is not None and (bound["agent"], bound["workdir"]) == ("claude", "~/code")
        claude = {"agent": "claude", "workdir": "~/code", "host_id": "macbook-pro"}
        await chats.set_agent_session("c_old", "sess-9", **claude)
        assert await chats.agent_session("c_old") == "sess-9"
        # A session belongs to the tool, folder and host its run was started with: one
        # that ends after the chat was bound to another is not handed to that one.
        for change in ({"agent": "opencode"}, {"workdir": "~/other"}, {"host_id": "arch-dev"}):
            await chats.set_agent_session("c_old", "late", **{**claude, **change})
            assert await chats.agent_session("c_old") == "sess-9", change
        await chats.set_agent_session("c_old", "", **claude)  # "" forgets it
        assert await chats.agent_session("c_old") == ""
        new = await chats.create("arch-dev", agent="opencode")
        assert (new["agent"], new["workdir"]) == ("opencode", "")

    asyncio.run(scenario())
