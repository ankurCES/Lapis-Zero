from __future__ import annotations

import datetime

import pytest

from lapis_harness.agent.scheduler import next_run, parse_cron


def test_parse_cron_five_fields() -> None:
    assert parse_cron("0 8 * * *") == [
        [0],
        [8],
        list(range(1, 32)),
        list(range(1, 13)),
        list(range(7)),
    ]


def test_parse_cron_errors() -> None:
    with pytest.raises(ValueError, match="5 fields"):
        parse_cron("* * * *")
    with pytest.raises(ValueError, match="bad step"):
        parse_cron("*/0 * * * *")


def test_next_run_matches_fields() -> None:
    # after Sunday 2024-01-01 00:00, the next "0 8 * * 1" is Monday (dow 1) 08:00 UTC.
    after = datetime.datetime(2024, 1, 1, 0, 0, tzinfo=datetime.UTC)
    nxt = next_run("0 8 * * 1", after.timestamp())
    t = datetime.datetime.fromtimestamp(nxt, tz=datetime.UTC)
    assert (t.year, t.month, t.day, t.hour, t.minute) == (2024, 1, 1, 8, 0)
    assert t.weekday() == 0
    assert t.timestamp() > after.timestamp()
