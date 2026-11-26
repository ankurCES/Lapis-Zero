"""Minimal asyncio cron runner for unattended background tasks (SPEC Phase 3).

Deliberately constrained to the five standard cron fields (minutes, hours, day-of-month,
month, day-of-week). It parses and computes next-run times itself so there is no need for a
third-party cron library on the 512 MB device.
"""

from __future__ import annotations

import asyncio
import calendar
import contextlib
import time
from dataclasses import dataclass
from typing import Any

_TOKEN_MAP = {
    "minute": 0,
    "hour": 1,
    "day": 2,
    "month": 3,
    "dow": 4,
}
_RANGES = {
    "minute": (0, 59),
    "hour": (0, 23),
    "day": (1, 31),
    "month": (1, 12),
    "dow": (0, 6),
}
_MONTH_DAYS = (31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)


class CronError(ValueError):
    pass


@dataclass(frozen=True)
class Schedule:
    name: str
    cron: str
    host: str
    prompt: str

    @property
    def fields(self) -> list[list[int]]:
        return parse_cron(self.cron)


def _one_field(field: str, lo: int, hi: int) -> list[int]:
    out: list[int] = []
    for part in field.split(","):
        part = part.strip()
        step = 1
        if "/" in part:
            base, _, s = part.partition("/")
            step = int(s)
            if step < 1:
                raise CronError(f"bad step: {field}")
            part = base
        if part == "*" or part == "?":
            rng = range(lo, hi + 1, step)
        elif "-" in part:
            a, _, b = part.partition("-")
            rng = range(int(a), int(b) + 1, step)
        else:
            rng = range(int(part), int(part) + 1, step)
        for v in rng:
            if v < lo or v > hi:
                raise CronError(f"value {v} out of range in {field}")
            if v not in out:
                out.append(v)
    return sorted(out)


def parse_cron(expr: str) -> list[list[int]]:
    """Parse a five-field cron expression into sorted value lists."""
    parts = expr.split()
    if len(parts) != 5:
        raise CronError(f"expected 5 fields, got {len(parts)}")
    result: list[list[int]] = []
    for name, field in zip(("minute", "hour", "day", "month", "dow"), parts, strict=True):
        lo, hi = _RANGES[name]
        vals = _one_field(field, lo, hi)
        if not vals:
            raise CronError(f"empty field '{field}'")
        result.append(vals)
    return result


def _days_in_month(month: int, year: int) -> int:
    if month == 2 and (year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)):
        return 29
    return _MONTH_DAYS[month - 1]


def next_run(expr: str, after: float | None = None) -> float:
    """Next epoch time strictly after `after` matching `expr` (5 fields)."""
    fields = parse_cron(expr)
    minutes, hours, days, months, dows = fields
    # Operate in UTC so the result is timezone-independent (cron runs in device local time;
    # the device is typically set to UTC on first boot before Tailscale pulls tzdata).
    t = time.gmtime(after if after is not None else time.time())
    year, month, day, hour, minute = t.tm_year, t.tm_mon, t.tm_mday, t.tm_hour, t.tm_min
    # scan forward minute-by-minute (worst case a few years of minutes is still < 3M iters)
    for _ in range(3_153_600):
        minute += 1
        if minute == 60:
            minute = 0
            hour += 1
        if hour == 24:
            hour = 0
            day += 1
        if day > _days_in_month(month, year):
            day = 1
            month += 1
        if month == 13:
            month = 1
            year += 1
        if (
            minute in minutes
            and hour in hours
            and day in days
            and month in months
            and day_of_week(year, month, day) in dows
        ):
            return calendar.timegm((year, month, day, hour, minute, 0, 0, 0, 0))
    raise CronError("cron expression has no future match")


_suppress = contextlib.suppress


def day_of_week(year: int, month: int, day: int) -> int:
    """0=Sunday .. 6=Saturday (standard cron day-of-week convention)."""
    import datetime

    return (datetime.date(year, month, day).weekday() + 1) % 7


class CronScheduler:
    """Wakes at the next close schedule and calls `run(name, host, prompt)`."""

    def __init__(self, run: Any) -> None:
        self.run = run  # async callable (name, host, prompt)
        self._task: asyncio.Task[None] | None = None
        self._schedules: dict[str, Schedule] = {}
        self._wake = asyncio.Event()

    def set_schedules(self, schedules: list[Schedule]) -> None:
        self._schedules = {s.name: s for s in schedules}
        self._wake.set()

    def start(self) -> None:
        if self._task is None:
            self._task = asyncio.create_task(self._loop())

    async def _loop(self) -> None:
        while True:
            if not self._schedules:
                self._wake.clear()
                await self._wake.wait()
            target = min(next_run(s.cron) for s in self._schedules.values())
            delay = max(0.0, target - time.time())
            try:
                await asyncio.wait_for(self._wake.wait(), timeout=delay)
                self._wake.clear()
                continue  # schedules changed; recompute
            except TimeoutError:
                pass
            for s in self._schedules.values():
                if target - 0.5 <= next_run(s.cron, target - 1.0) < target + 0.5:
                    with _suppress(Exception):
                        await self.run(s.name, s.host, s.prompt)

    def stop(self) -> None:
        if self._task is not None:
            self._task.cancel()
            self._task = None
