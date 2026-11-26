"""Screenshot ring buffer on disk (ANDROID_COMPANION_SPEC 5.1).

Recent agent frames are kept as JPEG files so the phone can show them in full
colour; the directory is capped (25 MB by default) and the oldest frames are
evicted first, so SD-card use stays small and bounded.
"""

from __future__ import annotations

import asyncio
import time
from pathlib import Path
from typing import Any

from lapis_harness.db import LapisDb
from lapis_harness.kvm.base import Frame


class FrameStore:
    def __init__(self, db: LapisDb, directory: str | Path, cap_bytes: int = 25 * 1024 * 1024):
        self.db = db
        self.dir = Path(directory)
        self.cap_bytes = cap_bytes

    async def save(
        self, task_id: str, step: int, frame: Frame, click: tuple[int, int] | None = None
    ) -> int:
        """Persist one frame, evict down to the cap, return its screenshot id."""
        name = f"{task_id}-{step:03d}-{time.time_ns() % 10**9:09d}.jpg"
        path = self.dir / name

        def _write() -> None:
            self.dir.mkdir(parents=True, exist_ok=True)
            path.write_bytes(frame.jpeg)

        await asyncio.to_thread(_write)
        cx, cy = click if click else (None, None)
        await self.db.run(
            "INSERT INTO screenshots(task_id, step_num, jpeg_path, width, height, click_x,"
            " click_y, bytes, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (task_id, step, name, frame.width, frame.height, cx, cy, len(frame.jpeg), time.time()),
        )
        row = await self.db.run("SELECT MAX(id) AS id FROM screenshots")
        await self._evict()
        return int(row[0]["id"])

    async def _evict(self) -> None:
        rows = await self.db.run("SELECT id, jpeg_path, bytes FROM screenshots ORDER BY id")
        total = sum(r["bytes"] for r in rows)
        doomed = []
        for row in rows[:-1]:  # never evict the frame that was just written
            if total <= self.cap_bytes:
                break
            total -= row["bytes"]
            doomed.append(row)
        if not doomed:
            return

        def _unlink() -> None:
            for row in doomed:
                (self.dir / row["jpeg_path"]).unlink(missing_ok=True)

        await asyncio.to_thread(_unlink)
        marks = ",".join("?" * len(doomed))
        await self.db.run(
            f"DELETE FROM screenshots WHERE id IN ({marks})", tuple(r["id"] for r in doomed)
        )

    async def get(self, shot_id: int) -> dict[str, Any] | None:
        """Row plus an absolute `path`, or None if unknown or already evicted."""
        rows = await self.db.run("SELECT * FROM screenshots WHERE id = ?", (shot_id,))
        if not rows:
            return None
        path = self.dir / rows[0]["jpeg_path"]
        return {**rows[0], "path": path} if path.exists() else None
