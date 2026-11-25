"""SQLite WAL persistence for tasks, steps, and schedules (SPEC 7.3)."""

from __future__ import annotations

import asyncio
import sqlite3
import threading
from contextlib import closing
from pathlib import Path

SCHEMA = """
CREATE TABLE IF NOT EXISTS tasks(
    id TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL,
    policy TEXT NOT NULL DEFAULT 'balanced',
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS steps(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    summary TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ok',
    created_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS schedules(
    name TEXT PRIMARY KEY,
    cron TEXT NOT NULL,
    host TEXT NOT NULL,
    prompt TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0
);
-- Control plane (ANDROID_COMPANION_SPEC 5.1) --------------------------------
CREATE TABLE IF NOT EXISTS paired_devices(
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    token_hash TEXT NOT NULL UNIQUE,
    created_at REAL NOT NULL,
    last_seen_at REAL,
    revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS chats(
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    host_id TEXT NOT NULL,
    model_override TEXT,
    policy_override TEXT,
    archived INTEGER NOT NULL DEFAULT 0,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS chat_messages(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    task_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS chat_messages_chat ON chat_messages(chat_id, id);
CREATE TABLE IF NOT EXISTS screenshots(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    step_num INTEGER NOT NULL,
    jpeg_path TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    click_x INTEGER,
    click_y INTEGER,
    bytes INTEGER NOT NULL,
    created_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS schedule_runs(
    name TEXT PRIMARY KEY,
    last_run_at REAL,
    last_chat_id TEXT
);
"""

# Columns added after the first release; ALTER is skipped when already present.
MIGRATIONS = (
    ("tasks", "chat_id", "TEXT"),
    ("tasks", "summary", "TEXT NOT NULL DEFAULT ''"),
    # Who answers in a chat: 'lapis' (the device's own agent) or an agent tool on the
    # host; that tool's own session id, to continue it; the folder it works in.
    ("chats", "agent", "TEXT NOT NULL DEFAULT 'lapis'"),
    ("chats", "agent_session", "TEXT"),
    ("chats", "workdir", "TEXT NOT NULL DEFAULT ''"),
)


class LapisDb:
    """Async facade over sqlite3; each call runs in a worker thread on a fresh connection."""

    def __init__(self, path: str | Path) -> None:
        self.path = str(path)
        # ponytail: one global lock + connection per call; pool if step volume ever matters
        self._lock = threading.Lock()

    async def __aenter__(self) -> LapisDb:
        await self.init()
        return self

    async def __aexit__(self, *exc: object) -> None:
        self.close()

    def _conn(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.path)
        conn.row_factory = sqlite3.Row
        # journal_mode=WAL persists in the file; these two are per-connection.
        conn.execute("PRAGMA synchronous=NORMAL")
        conn.execute("PRAGMA busy_timeout=5000")
        return conn

    def _run(self, sql: str, params: tuple = ()) -> list[dict]:
        with self._lock, closing(self._conn()) as conn, conn:
            return [dict(row) for row in conn.execute(sql, params).fetchall()]

    def _init(self) -> None:
        with self._lock, closing(self._conn()) as conn:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.executescript(SCHEMA)
            for table, column, decl in MIGRATIONS:
                have = {row["name"] for row in conn.execute(f"PRAGMA table_info({table})")}
                if column not in have:
                    conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {decl}")
            conn.commit()

    async def init(self) -> None:
        await asyncio.to_thread(self._init)

    async def run(self, sql: str, params: tuple = ()) -> list[dict]:
        """Run one statement off the event loop; SELECTs return rows as dicts."""
        return await asyncio.to_thread(self._run, sql, params)

    def run_sync(self, sql: str, params: tuple = ()) -> list[dict]:
        """Blocking variant for the BLE provisioning helper and CLI tools."""
        return self._run(sql, params)

    async def insert_task(
        self,
        id: str,
        host: str,
        prompt: str,
        policy: str,
        now: float,
        chat_id: str | None = None,
    ) -> None:
        await asyncio.to_thread(
            self._run,
            "INSERT INTO tasks(id, host, prompt, status, policy, created_at, updated_at, chat_id)"
            " VALUES (?, ?, ?, 'running', ?, ?, ?, ?)",
            (id, host, prompt, policy, now, now, chat_id),
        )

    async def update_task_status(self, id: str, status: str, now: float, summary: str = "") -> None:
        await asyncio.to_thread(
            self._run,
            "UPDATE tasks SET status = ?, updated_at = ?, summary = ? WHERE id = ?",
            (status, now, summary, id),
        )

    async def get_task(self, id: str) -> dict | None:
        rows = await asyncio.to_thread(self._run, "SELECT * FROM tasks WHERE id = ?", (id,))
        return rows[0] if rows else None

    async def insert_step(
        self,
        task_id: str,
        seq: int,
        kind: str,
        summary: str,
        detail: str,
        status: str,
        now: float,
    ) -> None:
        await asyncio.to_thread(
            self._run,
            "INSERT INTO steps(task_id, seq, kind, summary, detail, status, created_at)"
            " VALUES (?, ?, ?, ?, ?, ?, ?)",
            (task_id, seq, kind, summary, detail, status, now),
        )

    async def list_recent_steps(self, task_id: str, limit: int) -> list[dict]:
        """Return the newest `limit` steps of a task, oldest first."""
        rows = await asyncio.to_thread(
            self._run,
            "SELECT * FROM steps WHERE task_id = ? ORDER BY seq DESC, id DESC LIMIT ?",
            (task_id, limit),
        )
        return rows[::-1]

    async def add_schedule(
        self, name: str, cron: str, host: str, prompt: str, enabled: bool
    ) -> None:
        await asyncio.to_thread(
            self._run,
            "INSERT OR REPLACE INTO schedules(name, cron, host, prompt, enabled)"
            " VALUES (?, ?, ?, ?, ?)",
            (name, cron, host, prompt, int(enabled)),
        )

    async def list_schedules(self) -> list[dict]:
        return await asyncio.to_thread(self._run, "SELECT * FROM schedules ORDER BY name")

    def close(self) -> None:
        """Nothing to release: connections are per call."""
