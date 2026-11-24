from __future__ import annotations

from lapis_daemon.kvm.ssh_driver import TRUNCATE_MARKER, truncate_output


def test_truncate_output_keeps_short() -> None:
    assert truncate_output("ok") == "ok"
    assert truncate_output("x" * 4096, 4096) == "x" * 4096


def test_truncate_output_cuts_and_marks() -> None:
    out = truncate_output("x" * 5000, 4096)
    assert out.startswith("x" * 4096)
    assert TRUNCATE_MARKER.format(limit=4096) in out
    assert len(out) == 4096 + len(TRUNCATE_MARKER.format(limit=4096))
