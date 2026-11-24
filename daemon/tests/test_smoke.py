"""Smoke tests for the lapis-daemon CLI."""

from __future__ import annotations

import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

import lapis_daemon
from lapis_daemon.main import main


def test_version_exported() -> None:
    assert lapis_daemon.__version__ == "0.1.0"


def test_help_lists_flags(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        main(["--help"])
    assert exc.value.code == 0
    out = capsys.readouterr().out
    assert "--mock" in out
    assert "--config" in out


def test_missing_config_without_mock_fails(
    short_tmp: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("LAPIS_CONFIG_DIR", str(short_tmp))
    assert main([]) == 2
    assert "cannot load configuration" in capsys.readouterr().err


def test_mock_daemon_honors_config_dir_and_stops_on_sigterm(short_tmp: Path) -> None:
    """`--mock` listens under LAPIS_CONFIG_DIR (no root) and exits 0 on SIGTERM."""
    proc = subprocess.Popen(
        [sys.executable, "-m", "lapis_daemon", "--mock"],
        env={**os.environ, "LAPIS_CONFIG_DIR": str(short_tmp)},
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line = proc.stdout.readline()
        sock = short_tmp / "lapisd.sock"
        assert line.strip() == f"lapis-daemon 0.1.0 listening on {sock} (mock KVM)"
        assert sock.is_socket()
        assert (short_tmp / "lapis.db").exists()
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=5) == 0
        assert not sock.exists()
    finally:
        proc.kill()
        proc.wait()
