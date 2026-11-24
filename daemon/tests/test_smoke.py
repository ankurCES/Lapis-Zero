"""Phase 0 smoke tests for lapis-daemon."""

from __future__ import annotations

import subprocess
import sys

import pytest

import lapis_daemon
from lapis_daemon.main import main


def test_version_exported() -> None:
    """Package exports the expected semantic version string."""
    assert lapis_daemon.__version__ == "0.1.0"


def test_main_mock_returns_zero(capsys: pytest.CaptureFixture[str]) -> None:
    """Running main with --mock prints the Phase 0 stub message and exits 0."""
    exit_code = main(["--mock"])
    captured = capsys.readouterr()
    assert exit_code == 0
    assert "lapis-daemon 0.1.0 (mock mode)" in captured.out


def test_main_default_returns_zero(capsys: pytest.CaptureFixture[str]) -> None:
    """Running main without --mock exits 0 without touching /etc/lapis in Phase 0."""
    exit_code = main([])
    captured = capsys.readouterr()
    assert exit_code == 0
    assert "config loading lands in Phase 2" in captured.out


def test_help_lists_mock() -> None:
    """Invoking `python -m lapis_daemon --help` advertises the --mock flag."""
    result = subprocess.run(
        [sys.executable, "-m", "lapis_daemon", "--help"],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0
    assert "--mock" in result.stdout
