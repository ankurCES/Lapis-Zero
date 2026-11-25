"""Smoke tests for the lapis-daemon CLI."""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

import pytest
from conftest import free_port

import lapis_daemon
from lapis_daemon.main import main


def test_version_exported() -> None:
    assert lapis_daemon.__version__ == "0.3.2"


def test_help_lists_flags(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        main(["--help"])
    assert exc.value.code == 0
    out = capsys.readouterr().out
    assert "--mock" in out
    assert "--config" in out


def test_dev_flags_require_mock(capsys: pytest.CaptureFixture[str]) -> None:
    for flags in (["--dev-pair-token", "t"], ["--dev-bind", "0.0.0.0"], ["--dev-advertise", "h:1"]):
        with pytest.raises(SystemExit) as exc:
            main(flags)
        assert exc.value.code == 2
    assert "require --mock" in capsys.readouterr().err


def test_nothing_heavy_is_loaded_until_it_is_needed() -> None:
    """512 MB of RAM: SSH, VNC, key and image libraries are imported by their first use."""
    code = (
        "import sys\n"
        "import lapis_daemon.main, lapis_daemon.api.server, lapis_daemon.ipc_server\n"
        "import lapis_daemon.agent.harness, lapis_daemon.kvm.device_key, lapis_daemon.kvm.base\n"
        "print(sorted({m.partition('.')[0] for m in sys.modules}"
        " & {'asyncssh', 'asyncvnc', 'cryptography', 'PIL', 'numpy'}))"
    )
    done = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=30, check=False
    )
    assert (done.stdout.strip(), done.returncode) == ("[]", 0), done


def test_real_mode_starts_without_any_config_and_keeps_the_api_on_loopback(
    short_tmp: Path,
) -> None:
    """An unseeded card must boot: no lapis.toml / hosts.toml is not an error."""
    env = {**os.environ, "LAPIS_CONFIG_DIR": str(short_tmp), "PATH": "/usr/bin:/bin"}
    port = free_port()
    (short_tmp / "lapis.toml").write_text(f"[api]\nport = {port}\n")
    (short_tmp / "hosts.toml").write_text("this is = not [valid toml\n")
    proc = subprocess.Popen(
        [sys.executable, "-m", "lapis_daemon"],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert proc.stdout is not None
        sock = short_tmp / "lapisd.sock"
        assert proc.stdout.readline().strip() == (
            f"lapis-daemon 0.3.2 listening on {sock} (real Option C KVM)"
        )
        assert proc.stdout.readline().strip() == (
            f"lapis-daemon control plane on http://127.0.0.1:{port}"
        )
        with pytest.raises(urllib.error.HTTPError) as err:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/v1/health", timeout=3)
        assert err.value.code == 401
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=8) == 0
        assert "ignoring invalid" in proc.stderr.read()  # the broken hosts.toml was reported
    finally:
        proc.kill()
        proc.wait()


def test_mock_daemon_honors_config_dir_and_stops_on_sigterm(short_tmp: Path) -> None:
    """`--mock` listens under LAPIS_CONFIG_DIR (no root) and exits 0 on SIGTERM."""
    proc = subprocess.Popen(
        [sys.executable, "-m", "lapis_daemon", "--mock", "--api-port", str(free_port())],
        env={**os.environ, "LAPIS_CONFIG_DIR": str(short_tmp)},
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line = proc.stdout.readline()
        sock = short_tmp / "lapisd.sock"
        assert line.strip() == f"lapis-daemon 0.3.2 listening on {sock} (mock KVM)"
        assert sock.is_socket()
        assert (short_tmp / "lapis.db").exists()
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=5) == 0
        assert not sock.exists()
    finally:
        proc.kill()
        proc.wait()
