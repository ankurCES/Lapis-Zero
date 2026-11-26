"""Multi-turn chat threads backed by SQLite (ANDROID_COMPANION_SPEC 5.1).

A chat is bound to one target host. Every user prompt starts an agent run in the
chat's conversation context; the run's tool calls and its closing summary are
appended as messages, so the phone and the device see one continuous thread.
"""

from __future__ import annotations

import builtins
import json
import secrets
import time
from collections.abc import Callable
from typing import Any

from lapis_harness.db import LapisDb

ROLES = ("user", "assistant", "tool", "system")
LLM_HISTORY_TURNS = 20
# A tool's session belongs to that tool, on that host, in that folder (Claude Code files
# sessions per folder): changing any of the three starts a new one.
SESSION_BOUND = {"agent", "host_id", "workdir"}


def _chat(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": row["id"],
        "title": row["title"],
        "host_id": row["host_id"],
        "model_override": row["model_override"],
        "policy_override": row["policy_override"],
        "archived": bool(row["archived"]),
        "created_at": int(row["created_at"]),
        "updated_at": int(row["updated_at"]),
        # The tool's session id stays on the device: it is of no use anywhere else.
        "agent": row["agent"],
        "workdir": row["workdir"],
    }


def _message(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": row["id"],
        "chat_id": row["chat_id"],
        "role": row["role"],
        "content": row["content"],
        "task_id": row["task_id"],
        "metadata": json.loads(row["metadata_json"] or "{}"),
        "created_at": int(row["created_at"]),
    }


class ChatManager:
    def __init__(self, db: LapisDb, publish: Callable[[dict[str, Any]], None]) -> None:
        self.db = db
        self.publish = publish

    async def create(
        self,
        host_id: str,
        title: str | None = None,
        policy_override: str | None = None,
        agent: str = "lapis",
        workdir: str = "",
    ) -> dict[str, Any]:
        chat_id = f"c_{secrets.token_hex(4)}"
        now = time.time()
        title = (title or "New chat")[:120]
        await self.db.run(
            "INSERT INTO chats(id, title, host_id, policy_override, agent, workdir,"
            " created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (chat_id, title, host_id, policy_override, agent, workdir, now, now),
        )
        chat = await self.get(chat_id)
        assert chat is not None
        self.publish({"type": "chat_updated", "chat": chat})
        return chat

    async def get(self, chat_id: str) -> dict[str, Any] | None:
        rows = await self.db.run("SELECT * FROM chats WHERE id = ?", (chat_id,))
        if not rows:
            return None
        chat = _chat(rows[0])
        last = await self.db.run(
            "SELECT * FROM chat_messages WHERE chat_id = ? AND role != 'tool'"
            " ORDER BY id DESC LIMIT 1",
            (chat_id,),
        )
        chat["last_message"] = _message(last[0]) if last else None
        return chat

    async def list(
        self, host_id: str | None = None, include_archived: bool = False
    ) -> list[dict[str, Any]]:
        sql, params = "SELECT id FROM chats WHERE 1=1", []
        if host_id:
            sql += " AND host_id = ?"
            params.append(host_id)
        if not include_archived:
            sql += " AND archived = 0"
        rows = await self.db.run(sql + " ORDER BY updated_at DESC", tuple(params))
        return [c for r in rows if (c := await self.get(r["id"])) is not None]

    async def update(self, chat_id: str, **fields: Any) -> dict[str, Any] | None:
        allowed = {"title", "host_id", "policy_override", "model_override", "archived"}
        allowed |= {"agent", "workdir"}
        sets = {k: v for k, v in fields.items() if k in allowed}
        if "archived" in sets:
            sets["archived"] = int(bool(sets["archived"]))
        if SESSION_BOUND & set(sets):
            rows = await self.db.run("SELECT * FROM chats WHERE id = ?", (chat_id,))
            if rows and any(rows[0][k] != sets[k] for k in SESSION_BOUND & set(sets)):
                sets["agent_session"] = None
        if sets:
            cols = ", ".join(f"{k} = ?" for k in sets)
            await self.db.run(
                f"UPDATE chats SET {cols}, updated_at = ? WHERE id = ?",
                (*sets.values(), time.time(), chat_id),
            )
        chat = await self.get(chat_id)
        if chat is not None:
            self.publish({"type": "chat_updated", "chat": chat})
        return chat

    async def agent_session(self, chat_id: str) -> str:
        """The session id the chat's agent tool gave its conversation ("" = none yet)."""
        rows = await self.db.run("SELECT agent_session FROM chats WHERE id = ?", (chat_id,))
        return (rows[0]["agent_session"] or "") if rows else ""

    async def set_agent_session(
        self, chat_id: str, session: str, *, agent: str, workdir: str, host_id: str
    ) -> None:
        """Keep the session id a tool's run ended with ("" = forget it).

        Only while the chat is still bound to what that run was started with: changed
        meanwhile (see SESSION_BOUND), the id would be handed to another tool, or to
        the same one in a folder where it does not know it.
        """
        await self.db.run(
            "UPDATE chats SET agent_session = ?"
            " WHERE id = ? AND agent = ? AND workdir = ? AND host_id = ?",
            (session or None, chat_id, agent, workdir, host_id),
        )

    async def delete(self, chat_id: str) -> bool:
        if await self.get(chat_id) is None:
            return False
        await self.db.run("DELETE FROM chat_messages WHERE chat_id = ?", (chat_id,))
        await self.db.run("DELETE FROM chats WHERE id = ?", (chat_id,))
        self.publish({"type": "chat_deleted", "chat_id": chat_id})
        return True

    async def add_message(
        self,
        chat_id: str,
        role: str,
        content: str,
        task_id: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        assert role in ROLES
        now = time.time()
        await self.db.run(
            "INSERT INTO chat_messages(chat_id, role, content, task_id, metadata_json, created_at)"
            " VALUES (?, ?, ?, ?, ?, ?)",
            (chat_id, role, content, task_id, json.dumps(metadata or {}), now),
        )
        await self.db.run("UPDATE chats SET updated_at = ? WHERE id = ?", (now, chat_id))
        row = await self.db.run(
            "SELECT * FROM chat_messages WHERE chat_id = ? ORDER BY id DESC LIMIT 1", (chat_id,)
        )
        message = _message(row[0])
        self.publish({"type": "chat_message_created", "chat_id": chat_id, "message": message})
        return message

    async def messages(
        self, chat_id: str, before: int | None = None, limit: int = 50
    ) -> tuple[builtins.list[dict[str, Any]], bool]:
        """Newest `limit` messages older than `before`, oldest first, plus has_more."""
        limit = max(1, min(limit, 200))
        sql = "SELECT * FROM chat_messages WHERE chat_id = ?"
        params: builtins.list[Any] = [chat_id]
        if before:
            sql += " AND id < ?"
            params.append(before)
        rows = await self.db.run(sql + " ORDER BY id DESC LIMIT ?", (*params, limit + 1))
        return [_message(r) for r in rows[:limit]][::-1], len(rows) > limit

    async def history_for_llm(self, chat_id: str) -> builtins.list[dict[str, str]]:
        """Prior user/assistant turns as plain text (tool traffic is not replayed)."""
        rows = await self.db.run(
            "SELECT role, content FROM chat_messages WHERE chat_id = ?"
            " AND role IN ('user', 'assistant') ORDER BY id DESC LIMIT ?",
            (chat_id, LLM_HISTORY_TURNS),
        )
        return [{"role": r["role"], "content": r["content"]} for r in rows[::-1]]

    async def device_chat(self, host_id: str) -> dict[str, Any]:
        """The thread that collects prompts typed or spoken on the device itself."""
        title = f"On device: {host_id}"
        rows = await self.db.run(
            "SELECT id FROM chats WHERE title = ? AND host_id = ? AND archived = 0"
            " ORDER BY updated_at DESC LIMIT 1",
            (title, host_id),
        )
        if rows and (chat := await self.get(rows[0]["id"])) is not None:
            return chat
        return await self.create(host_id, title)
