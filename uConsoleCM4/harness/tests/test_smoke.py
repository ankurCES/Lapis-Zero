"""Smoke tests for the lapis-harness CLI."""

from __future__ import annotations

import asyncio
import importlib.metadata
import os
import signal
import subprocess
import sys
from pathlib import Path

import aiohttp
import pytest

import lapis_harness
from lapis_harness.main import main


def test_version_exported() -> None:
    assert lapis_harness.__version__ == importlib.metadata.version("lapis-harness")


def test_help_lists_flags(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exc:
        main(["--help"])
    assert exc.value.code == 0
    out = capsys.readouterr().out
    for flag in ("--mock", "--socket", "--daemon-url", "--token-file", "--state-dir", "--config"):
        assert flag in out


def test_the_gate_is_only_ever_asked_on_loopback(capsys: pytest.CaptureFixture[str]) -> None:
    """The local token goes with every gate question: it must not leave the device."""
    for url in ("http://example.com:8421", "https://127.0.0.1:8421", "http://10.0.0.2:8421"):
        with pytest.raises(SystemExit) as exc:
            main(["--daemon-url", url])
        assert exc.value.code == 2
    assert "loopback only" in capsys.readouterr().err


def test_nothing_heavy_is_loaded_until_it_is_needed() -> None:
    """SSH, VNC, key and image libraries are imported by their first use."""
    code = (
        "import sys\n"
        "import lapis_harness.main, lapis_harness.api.server, lapis_harness.mcp\n"
        "import lapis_harness.remote, lapis_harness.gate, lapis_harness.deck\n"
        "import lapis_harness.agent.harness, lapis_harness.kvm.device_key, lapis_harness.kvm.base\n"
        "print(sorted({m.partition('.')[0] for m in sys.modules}"
        " & {'asyncssh', 'asyncvnc', 'cryptography', 'PIL', 'numpy'}))"
    )
    done = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=30, check=False
    )
    assert (done.stdout.strip(), done.returncode) == ("[]", 0), done


async def _get(sock: Path, path: str) -> tuple[int, dict]:
    async with (
        aiohttp.ClientSession(connector=aiohttp.UnixConnector(path=str(sock))) as http,
        http.get(f"http://lapis-harness{path}") as resp,
    ):
        return resp.status, await resp.json()


def _start(*args: str) -> subprocess.Popen[str]:
    return subprocess.Popen(
        [sys.executable, "-m", "lapis_harness", *args],
        env={**os.environ, "PATH": "/usr/bin:/bin"},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def test_real_mode_starts_without_any_config_and_serves_its_socket(short_tmp: Path) -> None:
    """An unseeded device must boot: no lapis.toml / hosts.toml is not an error."""
    etc, state, sock = short_tmp / "etc", short_tmp / "state", short_tmp / "h.sock"
    etc.mkdir()
    (etc / "hosts.toml").write_text("this is = not [valid toml\n")
    proc = _start(
        "--socket", str(sock), "--state-dir", str(state), "--config", str(etc),
        "--token-file", str(short_tmp / "no.token"),
    )  # fmt: skip
    try:
        assert proc.stdout is not None and proc.stderr is not None
        line = proc.stdout.readline().strip()
        assert line == (
            f"lapis-harness: {lapis_harness.__version__} listening on {sock} (real fleet KVM); "
            "gate http://127.0.0.1:8421"
        )
        assert oct(sock.stat().st_mode & 0o777) == oct(0o660)
        status, chats = asyncio.run(_get(sock, "/v1/chats"))
        assert (status, chats) == (200, {"chats": []})
        status, hosts = asyncio.run(_get(sock, "/v1/hosts"))
        assert list(hosts["hosts"]) == ["deck"] and hosts["active_host"] == "deck"
        assert (state / "lapis.db").exists() and not (etc / "lapis.db").exists()
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=8) == 0
        assert not sock.exists()
        assert "ignoring invalid" in proc.stderr.read()  # the broken hosts.toml was reported
    finally:
        proc.kill()
        proc.wait()


def test_mock_harness_stops_on_sigterm(short_tmp: Path) -> None:
    sock = short_tmp / "m.sock"
    proc = _start("--mock", "--socket", str(sock), "--state-dir", str(short_tmp / "s"))
    try:
        assert proc.stdout is not None
        line = proc.stdout.readline()
        assert line.strip().endswith(
            f"listening on {sock} (mock fleet); gate http://127.0.0.1:8421"
        )
        assert sock.is_socket()
        assert (short_tmp / "s" / "lapis.db").exists()
        status, hosts = asyncio.run(_get(sock, "/v1/hosts"))
        assert list(hosts["hosts"]) == ["macbook-pro", "arch-dev", "deck"]
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=5) == 0
        assert not sock.exists()
    finally:
        proc.kill()
        proc.wait()
