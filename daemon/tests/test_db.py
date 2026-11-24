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
