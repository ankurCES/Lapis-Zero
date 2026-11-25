"""SSH for real, over loopback: logins, authorising the device, agent tools, the kill."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import signal
import socket
import subprocess
import sys
import time
import tracemalloc
from pathlib import Path
from typing import Any

import asyncssh
import pytest
from conftest import wait_for
from sshhost import USER, FakeHost, Relay, alive, file_mode

from lapis_daemon.agent import harness
from lapis_daemon.agent.harness import BUILTIN
from lapis_daemon.agent.runner import AgentJob, MockPlanner, TaskRunner
from lapis_daemon.config import CoreConfig, HostConfig, HostsConfig, HostSsh
from lapis_daemon.db import LapisDb
from lapis_daemon.kvm import base, device_key, ssh_driver
from lapis_daemon.kvm.base import build_host_driver
from lapis_daemon.kvm.ssh_driver import TRUNCATE_MARKER, SshDriver
from lapis_daemon.llm.base import ToolCall

SECRET = "LAPIS_TEST_SSH_PASSWORD"
PASSWORD = "correct horse battery staple"


def blob(key: dict[str, str] | Path) -> str:
    line = key["public_key"] if isinstance(key, dict) else device_key.public_key(key)["public_key"]
    return line.split()[1]


def args(host: FakeHost) -> str:
    """The arguments the stand-in for Claude Code was last started with."""
    return (host.home / "args").read_bytes().replace(b"\0", b" ").decode().strip()


ACTOR = harness.custom("actor", "Actor", "actor {prompt}")  # fakebin/actor: tools that misbehave
DETACHER = harness.custom("detacher", "Detacher", "detacher {prompt} parent")
ORPHANER = harness.custom("orphaner", "Detacher", "detacher {prompt} orphan")
FREE = "autonomous"  # the one tier a tool known only by its command line runs under


def composite(host: FakeHost, key: Path, **ssh: Any) -> Any:
    return build_host_driver("fake", host.config(**ssh), CoreConfig(), key)


@pytest.fixture
def password(monkeypatch: pytest.MonkeyPatch) -> str:
    monkeypatch.setenv(SECRET, PASSWORD)
    return PASSWORD


def test_the_device_key_is_made_once_and_stays_private(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = tmp_path / "state" / "ssh" / "id_ed25519"
    first = device_key.public_key(path)
    assert file_mode(path) == 0o600 and file_mode(path.parent) == 0o700
    assert [p.name for p in path.parent.iterdir()] == ["id_ed25519"]  # no temp file left
    kind, data, comment = first["public_key"].split()
    assert kind == "ssh-ed25519" and comment == f"lapis-zero@{comment.split('@')[1]}"
    assert "\n" not in first["public_key"]
    # asyncssh reads the same key and agrees on what it is.
    key = asyncssh.read_private_key(str(path))
    assert key.export_public_key().decode().split()[1] == data
    assert key.get_fingerprint() == first["fingerprint"]
    # A second need finds the key: nothing is generated again, nothing replaced.
    before = path.read_bytes()
    assert device_key.public_key(path) == first and path.read_bytes() == before
    assert device_key.ensure_device_key(path) == path and path.read_bytes() == before

    # Two tasks need the key at the same moment: the one that comes second keeps the
    # first one's key (it may be on a host already) instead of writing its own over it.
    raced = tmp_path / "raced" / "id_ed25519"
    real = device_key.tempfile.mkstemp

    def meanwhile(*args: Any, **kwargs: Any) -> Any:
        raced.write_bytes(before)  # the other task finishes first
        return real(*args, **kwargs)

    monkeypatch.setattr(device_key.tempfile, "mkstemp", meanwhile)
    assert device_key.ensure_device_key(raced).read_bytes() == before
    assert [p.name for p in raced.parent.iterdir()] == ["id_ed25519"]


def test_login_by_device_key_by_password_and_in_that_order(tmp_path: Path, password: str) -> None:
    async def login(host: FakeHost, **kw: Any) -> str:
        driver = SshDriver(host.config(**kw.pop("ssh", {})), device_key=tmp_path / "id", **kw)
        await driver.connect()
        try:
            assert await driver.run("echo in") == "in"
            return driver.login
        finally:
            await driver.disconnect()

    async def scenario() -> None:
        identity = tmp_path / "identity"
        asyncssh.generate_private_key("ssh-ed25519").write_private_key(str(identity))
        ident = asyncssh.read_private_key(str(identity)).export_public_key().decode().split()[1]
        async with FakeHost(tmp_path, password=PASSWORD) as host:
            mine = blob(tmp_path / "id")
            # Neither key is known to the host and no password is configured: refused,
            # and the password was never tried (some hosts ban after wrong ones).
            with pytest.raises(asyncssh.PermissionDenied):
                await login(host, ssh={"identity_file": str(identity)})
            assert host.offered == [ident, mine] and host.passwords == []
            # A password secret that is named but not stored is not tried either.
            with pytest.raises(asyncssh.PermissionDenied):
                await login(host, ssh={"password_env": "LAPIS_TEST_NOT_STORED"})
            assert host.passwords == []

            # Configured: the keys first, then the password.
            host.offered.clear()
            both = {"identity_file": str(identity), "password_env": SECRET}
            assert await login(host, ssh=both) == "password"
            assert host.offered == [ident, mine] and host.passwords == [PASSWORD]

            # The device key alone.
            host.authorized.append(mine)
            host.passwords.clear()
            assert await login(host, ssh={"password_env": SECRET}) == "device key"
            assert host.passwords == []
            # The identity file goes first when the host knows it; one that cannot be
            # read is skipped instead of ending the login.
            host.authorized.append(ident)
            assert await login(host, ssh=both) == "identity file"
            assert await login(host, ssh={"identity_file": str(tmp_path / "gone")}) == "device key"
            # Narrowed to one method (the proof after authorising).
            host.authorized.clear()
            with pytest.raises(asyncssh.PermissionDenied):
                await login(host, ssh=both, methods=("device key",))
            assert host.passwords == []

            # A Mac asks for the password in a dialogue rather than taking it outright:
            # the same secret answers it, and counts as the same method.
            async with FakeHost(tmp_path / "mac", password=PASSWORD, asks=True) as mac:
                assert await login(mac, ssh={"password_env": SECRET}) == "password"
                assert mac.passwords == [PASSWORD]
                with pytest.raises(asyncssh.PermissionDenied):
                    await login(mac)  # ...and is not asked at all when none is configured
                assert mac.passwords == [PASSWORD]

            # A wrong password is sent once, and no error repeats it.
            os.environ[SECRET] = "wrong " + PASSWORD
            with pytest.raises(asyncssh.PermissionDenied) as err:
                await login(host, ssh={"password_env": SECRET})
            assert host.passwords == ["wrong " + PASSWORD] and PASSWORD not in str(err.value)

    asyncio.run(scenario())


def test_ssh_problem_says_why_and_a_working_login_is_kept(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            # Refused: the reason is short, names no secret, and nothing is left open.
            refused = composite(host, tmp_path / "id")
            reason = await refused.ssh_problem()
            assert reason is not None and reason.startswith("PermissionDenied: Permission denied")
            assert len(reason) <= 200 and refused.ssh_login == ""
            with pytest.raises(asyncssh.PermissionDenied):
                await refused.run_ssh("echo never", 5)
            assert host.commands == []

            # Working: the session opened by the check is the one `run_ssh` uses.
            host.authorized.append(blob(tmp_path / "id"))
            driver = composite(host, tmp_path / "id")
            assert await driver.ssh_problem() is None and driver.ssh_login == "device key"
            assert await driver.run_ssh("echo one", 5) == "one"
            assert await driver.ssh_problem() is None
            assert await driver.run_ssh("echo two; exit 3", 5) == "two\n\n[exit status 3]"
            assert host.logins == 1 and host.commands == ["echo one", "echo two; exit 3"]
            await driver.disconnect()

        # A port that accepts and then says nothing (a firewall, a sleeping host).
        monkeypatch.setattr(base, "SSH_CHECK_TIMEOUT", 0.2)
        silent = socket.create_server(("127.0.0.1", 0))
        try:
            mute = HostConfig(
                hostname="mute",
                tailscale_ip="127.0.0.1",
                ssh=HostSsh(port=silent.getsockname()[1], user=USER),
            )
            driver = build_host_driver("mute", mute, CoreConfig(), tmp_path / "id")
            start = time.monotonic()
            assert await driver.ssh_problem() == "no answer on the SSH port"
            assert time.monotonic() - start < 2
            await driver.disconnect()
        finally:
            silent.close()

    asyncio.run(scenario())


def test_authorising_adds_one_line_and_touches_nothing_else(tmp_path: Path, password: str) -> None:
    async def scenario() -> None:
        async with FakeHost(tmp_path, password=PASSWORD) as host:
            key = device_key.public_key(tmp_path / "id")
            by_password = SshDriver(
                host.config(password_env=SECRET), device_key=tmp_path / "id", methods=("password",)
            )
            await by_password.connect()
            keys = host.authorized_keys()

            # Nothing there yet: ~/.ssh and the file are created, private.
            assert await by_password.authorize(key["public_key"]) is False
            assert keys.read_text() == key["public_key"] + "\n"
            assert file_mode(keys.parent) == 0o700 and file_mode(keys) == 0o600
            # Again: found, not added twice (also under another comment).
            assert await by_password.authorize(key["public_key"]) is True
            assert await by_password.authorize(key["public_key"] + "-renamed") is True
            assert keys.read_text() == key["public_key"] + "\n"

            # Somebody else's file, last line without its newline, modes of their own:
            # the other lines stay byte for byte, the modes stay, the key gets a line.
            theirs = 'ssh-rsa AAAAB3Nza other@laptop\ncommand="x y" ssh-ed25519 AAAAC3Nz backup'
            keys.write_text(theirs)
            keys.chmod(0o644)
            keys.parent.chmod(0o755)
            assert await by_password.authorize(key["public_key"]) is False
            assert keys.read_text() == theirs + "\n" + key["public_key"] + "\n"
            assert file_mode(keys.parent) == 0o755 and file_mode(keys) == 0o644
            await by_password.disconnect()

            # And now the key logs in by itself.
            alone = SshDriver(host.config(), device_key=tmp_path / "id")
            await alone.connect()
            assert alone.login == "device key"
            await alone.disconnect()

            # A home folder that cannot be written: said in words, not left half done.
            keys.parent.chmod(0o555)
            keys.chmod(0o444)
            await by_password.connect()
            with pytest.raises(OSError, match="could not write ~/.ssh/authorized_keys"):
                await by_password.authorize("ssh-ed25519 AAAAnother other-device")
            await by_password.disconnect()
            keys.parent.chmod(0o755)

    asyncio.run(scenario())


def test_agent_tools_are_found_and_run_through_the_login_shell(tmp_path: Path) -> None:
    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            claude = host.tool("claude")
            host.tool("gemini", "actor")
            (host.home / "work dir").mkdir()
            driver = composite(host, tmp_path / "id")

            # A plain `ssh host command` does not see them; the login shell does, past
            # everything its start-up files print.
            assert await driver.run_ssh("command -v claude || echo missing", 5) == "missing"
            assert await driver.find_agents(BUILTIN) == [
                {"id": "claude", "name": "Claude Code", "path": str(claude)},
                {"id": "gemini", "name": "Gemini CLI", "path": str(host.home / "bin" / "gemini")},
            ]

            # A message that is nothing but trouble arrives as it was typed, on stdin.
            nasty = "fix `this` $(touch PWNED); echo 'it''s' \"x\" \\ > out\n- naïve 🙂\n\n"
            seen: list[str] = []
            first = await driver.run_agent(
                BUILTIN["claude"],
                nasty,
                tier="strict",
                workdir="~/work dir",
                timeout_s=20,
                on_progress=seen.append,
            )
            assert (first.text, first.session, first.is_error) == ("answer 1", "sess-1", False)
            assert (host.home / "stdin").read_bytes() == nasty.encode()
            assert args(host) == "-p --output-format json --permission-mode plan"
            assert (host.home / "cwd").read_text().strip().endswith("/home/work dir")
            assert (host.home / "env").read_text() == "dumb 1\n"
            assert host.terminals[-1] == "dumb"  # a terminal, so a hang-up reaches the tool
            assert not list(tmp_path.rglob("PWNED")) and not (host.home / "out").exists()
            assert seen == []  # Claude Code prints its answer only: no ticker lines

            # The follow-up continues the session the tool named.
            second = await driver.run_agent(
                BUILTIN["claude"], "and now?", session=first.session, tier="autonomous"
            )
            # A tier the tool has no flags for: refused before anything reaches the host.
            ran = len(host.commands)
            with pytest.raises(ValueError, match="cannot be held to the balanced policy"):
                await driver.run_agent(BUILTIN["opencode"], "hello", tier="balanced")
            assert len(host.commands) == ran
            assert second.session == "sess-2" and args(host) == (
                "-p --output-format json --permission-mode bypassPermissions --resume sess-1"
            )
            # A tool whose answer is plain text; one that is not installed; a folder
            # that is not there.
            said = await driver.run_agent(BUILTIN["gemini"], "--yolo", tier="balanced")
            assert (said.text, said.session, said.is_error) == (
                "said: --approval-mode auto_edit -p  --yolo", "", False,
            )  # fmt: skip
            absent = await driver.run_agent(BUILTIN["opencode"], "hello", tier="autonomous")
            assert absent.is_error and absent.text.startswith("opencode is not installed on fake")
            lost = await driver.run_agent(
                BUILTIN["gemini"], "hello", tier="strict", workdir="/no/such/folder"
            )
            assert lost.is_error and lost.text == (
                "Gemini CLI failed (exit status 1). Its last output:\n"
                "Folder not found: /no/such/folder"
            )
            # A long message still arrives whole; one that cannot fit on a command line
            # is refused before anything is sent.
            long = "refactor the parser and keep every test green " * 400  # 18 KB
            assert (await driver.run_agent(BUILTIN["claude"], long, tier="strict")).text == (
                "answer 3"
            )
            assert (host.home / "stdin").read_text() == long
            with pytest.raises(ValueError, match="too long to hand over"):
                await driver.run_agent(BUILTIN["claude"], "é" * 30_000, tier="strict")
            assert (host.home / "stdin").read_text() == long  # nothing ran
            assert host.logins == 1  # all of it over the one session
            await driver.disconnect()

    asyncio.run(scenario())


def test_a_run_reports_failure_progress_and_keeps_only_the_newest_output(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "PROGRESS_INTERVAL", 0.04)

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            host.tool("actor", "actor")
            driver = composite(host, tmp_path / "id")

            failed = await driver.run_agent(ACTOR, "fail", tier=FREE)
            assert failed.is_error and failed.text == (
                "Actor failed (exit status 3). Its last output:\nreading the repo\nno API key"
            )

            lines: list[str] = []
            chatty = await driver.run_agent(ACTOR, "chatty", tier=FREE, on_progress=lines.append)
            assert chatty.text.endswith("step 6\nthe answer") and not chatty.is_error
            # Progress: the tool's lines without escape codes, at most one per interval.
            said = {*(f"step {i}" for i in range(1, 7)), "the answer"}
            assert 1 <= len(lines) <= 5 and set(lines) <= said

            # 300 KB of output: only the newest 64 KB is ever held.
            kept: list[Any] = []
            real = ssh_driver._Collector.result
            monkeypatch.setattr(
                ssh_driver._Collector,
                "result",
                lambda self, *a: kept.append(self) or real(self, *a),
            )
            flood = await driver.run_agent(ACTOR, "flood", tier=FREE)
            assert len(kept[0].buf) <= ssh_driver.OUTPUT_KEEP and kept[0].cut
            assert flood.text.endswith("xxx\nTHE END") and len(flood.text) == harness.ANSWER_MAX

            # An answer in JSON that is longer than the buffer (Claude Code prints one
            # line): its start is gone, with `is_error` and the session id in it. Not
            # shown as an answer, and not as a success.
            (host.home / "answer.json").write_text(
                json.dumps({"is_error": True, "result": "word " * 20_000, "session_id": "s-9"})
            )
            big = host.home / "bin" / "claude"
            big.write_text('#!/bin/sh\ncat > /dev/null\ncat "$HOME/answer.json"; echo\n')
            big.chmod(0o755)
            long = await driver.run_agent(BUILTIN["claude"], "hello", tier="strict")
            assert long.is_error and long.session == "" and long.text.startswith(
                "Claude Code printed more than the 64 KB this device keeps of one run, so its "
                "answer could not be read and whether it succeeded is not known."
            )  # fmt: skip

            # The time limit: reported with the last output, and the tool is gone.
            start = time.monotonic()
            slow = await asyncio.wait_for(
                driver.run_agent(ACTOR, "polite", tier=FREE, timeout_s=0.25), 5
            )
            assert slow.is_error and slow.text == (
                "Actor was stopped after 0.25 seconds without finishing. Its last output:\nstarted"
            )
            assert time.monotonic() - start < 2
            # Stopped means stopped: gone by the time the answer is there, not a while later.
            pids = [int(p) for p in (host.home / "pids").read_text().split()]
            assert not any(alive(p) for p in pids) and driver.left_running == ""
            await driver.disconnect()

    asyncio.run(scenario())


def test_stopping_a_run_ends_the_remote_processes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "STOP_GRACE", 0.2)

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            host.tool("actor", "actor")
            pids_file, asked = host.home / "pids", host.home / "asked"
            # Both start a child of their own; the second one refuses to be asked.
            for name, limit in (("polite", 1.0), ("stubborn", 1.5)):
                for how in ("cancel", "hang up"):
                    pids_file.unlink(missing_ok=True)
                    asked.unlink(missing_ok=True)
                    driver = composite(host, tmp_path / "id")
                    seen: list[str] = []
                    run = asyncio.create_task(
                        driver.run_agent(
                            ACTOR, name, tier=FREE, timeout_s=120, on_progress=seen.append
                        )
                    )
                    await wait_for(lambda seen=seen: seen == ["started"])
                    pids = [int(p) for p in pids_file.read_text().split()]
                    assert all(alive(p) for p in pids)
                    start = time.monotonic()
                    if how == "cancel":  # what the kill switch does to the task
                        run.cancel()
                        with pytest.raises(asyncio.CancelledError):
                            await run
                        assert time.monotonic() - start < limit, (name, how)
                        # Gone when the cancellation has gone through, and the host
                        # said so itself: nothing is left to hope for.
                        assert not any(alive(p) for p in pids), (name, how)
                        assert driver.left_running == harness.LEFT_NOTHING
                    else:  # the connection goes away under a running tool
                        await driver.disconnect()
                        dropped = await run
                        assert dropped.is_error and dropped.text == (
                            "The connection to fake closed before Actor finished: it may "
                            "still be running there. Its last output:\nstarted"
                        )
                        assert driver.left_running == harness.LEFT_UNKNOWN
                    # Asked before anything harsher: a tool that listens gets to tidy up.
                    assert asked.exists() == ((name, how) == ("polite", "cancel"))
                    if (name, how) == ("stubborn", "hang up"):
                        # It ignores the hang-up, and with the connection gone there is
                        # no way left to kill it: the same is true of sshd.
                        assert all(alive(p) for p in pids)
                    else:
                        await wait_for(lambda pids=pids: not any(alive(p) for p in pids), 2)
                    await driver.disconnect()

    asyncio.run(scenario())


# -- stopping what was started on the host: the kill switch and the time limits --------


def number(path: Path) -> int:
    return int(path.read_text().split()[0])


def group_alive(pgid: int) -> bool:
    """Whether anything is left of a process group (a detached command and what it runs)."""
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    return True


async def appears(path: Path) -> int:
    """The process id a stand-in wrote to `path`, once it is there (and complete)."""
    await wait_for(lambda: path.exists() and path.read_text().endswith("\n"))
    return number(path)


async def runner_on(
    tmp: Path, host: FakeHost, script: tuple[ToolCall, ...] = (), config: HostConfig | None = None
) -> tuple[TaskRunner, list[dict[str, Any]], list[str]]:
    """The task runner with the real drivers on the loopback host: (runner, finished, toasts)."""
    db = LapisDb(tmp / "lapis.db")
    await db.init()
    hosts = HostsConfig(hosts={"fake": config or host.config()})
    finished: list[dict[str, Any]] = []
    toasts: list[str] = []
    runner = TaskRunner(
        db,
        hosts,
        driver_factory=lambda n: build_host_driver(n, hosts.hosts[n], CoreConfig(), tmp / "id"),
        planner_factory=lambda: MockPlanner(script),
        emit_finish=finished.append,
        emit_toast=lambda level, text: toasts.append(text),
    )
    return runner, finished, toasts


def test_ssh_exec_keeps_the_head_of_the_output_and_never_holds_the_whole(tmp_path: Path) -> None:
    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            ssh = SshDriver(host.config(), device_key=tmp_path / "id")
            await ssh.connect()
            # What the model sees: stdout, then stderr, then how it ended.
            assert await ssh.run("echo one; echo two >&2; exit 3", 5) == (
                "one\n\ntwo\n\n[exit status 3]"
            )
            assert await ssh.run("exit 7", 5) == "[exit status 7]"
            assert await ssh.run("echo late >&2", 5) == "late"

            # More than 4 KB: the head, the marker, and still how it ended (said after
            # the cut, where a long output cannot push it out).
            cut = TRUNCATE_MARKER.format(limit=4096)
            big = "head -c 300000 /dev/zero | tr '\\0' x"
            assert await ssh.run(f"{big}; exit 4", 10) == "x" * 4096 + cut + "\n\n[exit status 4]"
            assert await ssh.run(f"{big} >&2", 10) == "x" * 4096 + cut
            exact = "head -c 4096 /dev/zero | tr '\\0' y"
            assert await ssh.run(exact, 10) == "y" * 4096  # all of it: no marker
            # Characters of several bytes: cut by bytes, and marked although the text is
            # shorter than the limit in characters.
            wide = await ssh.run("head -c 3000 /dev/zero | tr '\\0' x; printf '\\303\\251%.0s' "
                                 "$(seq 1 600)", 10)  # fmt: skip
            assert wide.startswith("x" * 3000 + "é" * 548) and wide.endswith(cut)

            # 512 MB of RAM: 8 MB of output must pass through, not pile up (collected
            # first and cut afterwards it took three times its size). Measured with the
            # loopback host in the same process: its side is counted too.
            tracemalloc.start()
            flood = await ssh.run("head -c 8000000 /dev/zero | tr '\\0' z; echo done >&2", 30)
            peak = tracemalloc.get_traced_memory()[1]
            tracemalloc.stop()
            assert flood == "z" * 4096 + cut and peak < 2_000_000, peak

            # A command that never stops printing: the head, then the time limit.
            endless = await ssh.run('echo $$ > "$HOME/pid"; exec yes 0123456789', 0.4)
            assert endless.startswith("0123456789\n" * 372) and endless.endswith(
                cut + "\n\n[timed out after 0.4s]"
            )
            assert not alive(number(host.home / "pid")) and ssh.left == harness.LEFT_NOTHING
            await ssh.disconnect()

    asyncio.run(scenario())


def test_ssh_exec_is_stopped_at_its_time_limit_and_by_the_kill_switch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "STOP_GRACE", 0.15)
    then = "sleep 60"

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            pid_file = host.home / "pid"
            driver = composite(host, tmp_path / "id")

            # The time limit: the model is told, and the command is gone, not left behind.
            out = await driver.run_ssh(f'echo $$ > "$HOME/pid"; echo begun; {then}', 0.4)
            assert out == "begun\n\n[timed out after 0.4s]"
            assert not alive(number(pid_file)) and driver.left_running == ""
            assert host.terminals[-1] is None  # no terminal: only a signal could end it

            # One that will not be asked: killed after the grace period.
            pid_file.unlink()
            start = time.monotonic()
            out = await driver.run_ssh(f'trap "" TERM; echo $$ > "$HOME/pid"; {then}', 0.4)
            assert out == "[timed out after 0.4s]" and not alive(number(pid_file))
            assert 0.55 <= time.monotonic() - start < 3 and driver.left_running == ""
            await driver.disconnect()

            # The kill switch in the middle of the model's own command.
            pid_file.unlink()
            command = f'echo $$ > "$HOME/pid"; {then}'
            call = ToolCall("ssh_exec", {"command": command, "timeout_s": 60})
            runner, finished, toasts = await runner_on(tmp_path, host, (call,))
            await runner.submit_task("fake", "tidy up", "autonomous")
            pid = await appears(pid_file)
            await runner.kill_all()
            assert not alive(pid)  # when the kill switch returns, not some time after
            assert (finished[0]["status"], finished[0]["summary"]) == (
                "cancelled",
                "Stopped by the kill switch.",
            )
            assert toasts == ["Kill switch: all agent activity stopped"]

            # An SSH server that passes no signals on (OpenSSH before 7.9), and no
            # terminal to hang up: the command runs on, and nobody is told otherwise.
            host.signals = False
            pid_file.unlink()
            out = await driver.run_ssh(command, 0.4)
            assert out == (
                "[timed out after 0.4s; the host did not confirm that the command was "
                "stopped: it may still be running]"
            )
            assert alive(number(pid_file)) and driver.left_running == harness.LEFT_UNKNOWN
            await driver.disconnect()
            pid_file.unlink()
            runner, finished, toasts = await runner_on(tmp_path, host, (call,))
            await runner.submit_task("fake", "tidy up", "autonomous")
            pid = await appears(pid_file)
            await runner.kill_all()
            assert alive(pid) and runner.agent_state == "IDLE"
            assert finished[0]["status"] == "cancelled" and finished[0]["summary"] == (
                "Stopped on this device by the kill switch, but fake did not confirm that "
                "the last command was stopped: it may still be running there."
            )
            assert toasts == ["Kill switch: the last command may still be running on fake"]

    asyncio.run(scenario())


def test_what_a_tool_started_in_sessions_of_their_own_is_stopped_with_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "STOP_GRACE", 0.15)
    strays: list[int] = []

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            host.tool("detacher", "detacher")
            host.tool("actor", "actor")
            (host.home / "python").write_text(sys.executable)
            files = [host.home / name for name in ("pids", "child")]

            async def at_work(tool: Any, name: str) -> tuple[Any, Any, int, int]:
                for path in files:
                    path.unlink(missing_ok=True)
                driver = composite(host, tmp_path / "id")
                run = asyncio.create_task(driver.run_agent(tool, name, tier=FREE, timeout_s=120))
                tool_pid, child = await appears(files[0]), await appears(files[1])
                strays.append(child)
                # A session of its own: no signal to the tool's process group, and no
                # hang-up of its terminal, gets there.
                assert os.getpgid(child) == child != os.getpgid(tool_pid)
                return driver, run, tool_pid, child

            # (The last one on an SSH server that passes no signals on: the tree is
            # stopped over a channel of its own, without that server's help.)
            for name, signals in (("polite", True), ("stubborn", True), ("stubborn", False)):
                host.signals = signals
                driver, run, tool_pid, child = await at_work(DETACHER, name)
                start = time.monotonic()
                run.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await run
                # Gone, with everything it was running, when the cancellation returns.
                assert not alive(tool_pid) and not group_alive(child), (name, signals)
                assert driver.left_running == harness.LEFT_NOTHING
                assert time.monotonic() - start < 2.5
                await driver.disconnect()

            # The time limit stops the same way, and says "stopped" because the host did.
            driver = composite(host, tmp_path / "id")
            slow = await driver.run_agent(DETACHER, "stubborn", tier=FREE, timeout_s=1)
            assert slow.text == (
                "Detacher was stopped after 1 seconds without finishing. Its last output:\nstarted"
            )
            assert not alive(number(files[0])) and not group_alive(number(files[1]))
            await driver.disconnect()

            # A host where the family tree cannot be read (no `ps`, another system): the
            # tool is stopped as before, through its session, and the result says what
            # was not looked for.
            host.signals = True
            monkeypatch.setattr(ssh_driver, "SWEEP", "exit 3")
            driver, run, tool_pid, child = await at_work(DETACHER, "polite")
            run.cancel()
            with pytest.raises(asyncio.CancelledError):
                await run
            assert not alive(tool_pid) and alive(child)
            assert driver.left_running == harness.LEFT_STARTED
            await driver.disconnect()
            os.killpg(child, signal.SIGKILL)
            slow = await driver.run_agent(ACTOR, "polite", tier=FREE, timeout_s=0.4)
            assert slow.is_error and slow.text == (
                "Actor was stopped after 0.4 seconds without finishing, but the programs "
                "it had started on fake could not be checked: some may still be running "
                "there. Its last output:\nstarted"
            )
            await driver.disconnect()
            # No family tree and an SSH server that passes no signals on either: what is
            # left is to close the channel, which hangs up the tool's terminal. A tool
            # that heeds a hang-up ends; the device cannot see that, and claims nothing.
            host.signals = False
            driver, run, tool_pid, child = await at_work(DETACHER, "polite")
            run.cancel()
            with pytest.raises(asyncio.CancelledError):
                await run
            assert driver.left_running == harness.LEFT_UNKNOWN
            await wait_for(lambda: not alive(tool_pid), 2)  # (before any disconnect)
            await driver.disconnect()
            os.killpg(child, signal.SIGKILL)
            host.signals = True
            # ...and the kill switch says the same, in the task's summary and its toast.
            runner, finished, toasts = await runner_on(tmp_path, host)
            for path in files:
                path.unlink(missing_ok=True)
            await runner.submit_task("fake", "polite", FREE, agent=AgentJob(DETACHER))
            tool_pid, child = await appears(files[0]), await appears(files[1])
            strays.append(child)
            await runner.kill_all()
            assert not alive(tool_pid) and alive(child)
            assert finished[0]["status"] == "cancelled" and finished[0]["summary"] == (
                "Stopped by the kill switch, but the programs that Detacher had started on "
                "fake could not be checked: some may still be running there."
            )
            assert toasts == [
                "Kill switch: programs started by Detacher may still be running on fake"
            ]

    try:
        asyncio.run(scenario())
    finally:
        for child in strays:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(child, signal.SIGKILL)


def test_a_command_that_is_nobodys_child_any_more_is_stopped_too(tmp_path: Path) -> None:
    """Where going by the family tree ends. The helper that started the command has
    exited, so the command belongs to init and nothing links it to the tool any more:
    `ps` shows no other lineage, and macOS shows nobody another process's environment.
    It runs on, and the device cannot know. Reported as an expected failure, for
    exactly that; everything before it is asserted as usual."""
    strays: list[int] = []

    async def scenario() -> bool:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            host.tool("detacher", "detacher")
            (host.home / "python").write_text(sys.executable)
            driver = composite(host, tmp_path / "id")
            run = asyncio.create_task(driver.run_agent(ORPHANER, "polite", tier=FREE))
            tool_pid = await appears(host.home / "pids")
            strays.append(await appears(host.home / "child"))
            assert os.getppid() != 1 and alive(tool_pid) and group_alive(strays[0])
            run.cancel()
            with pytest.raises(asyncio.CancelledError):
                await run
            await driver.disconnect()
            assert not alive(tool_pid)
            return group_alive(strays[0])

    try:
        ran_on = asyncio.run(scenario())
    finally:
        for child in strays:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(child, signal.SIGKILL)
    if ran_on:
        pytest.xfail("known limit: a command whose parent had ended before the stop is not found")


def test_the_kill_cannot_miss_a_command_that_is_just_being_started_nor_be_cut_short(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "STOP_GRACE", 0.25)

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            host.authorized.append(blob(tmp_path / "id"))
            host.tool("actor", "actor")
            pids_file = host.home / "pids"

            # The kill lands after the host has started the tool and before the device
            # knows that it did: the answer to its request is still on its way.
            driver = composite(host, tmp_path / "id")
            ssh = await driver._shell()
            real = ssh._conn.create_process

            async def slow(*args: Any, **kwargs: Any) -> Any:
                proc = await real(*args, **kwargs)
                await asyncio.sleep(0.25)  # the round trip of a slow link
                return proc

            ssh._conn.create_process = slow
            for run_it in (
                lambda: driver.run_agent(ACTOR, "stubborn", tier=FREE),
                lambda: driver.run_ssh('echo $$ > "$HOME/pids"; trap "" TERM; sleep 60', 30),
            ):
                pids_file.unlink(missing_ok=True)
                run = asyncio.create_task(run_it())
                await appears(pids_file)
                pids = [int(p) for p in pids_file.read_text().split()]
                run.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await run
                assert not any(alive(p) for p in pids) and driver.left_running == ""
            ssh._conn.create_process = real

            # A second cancellation while the first one is still stopping the tool (it
            # ignores being asked, so that takes the grace period): the stop goes on to
            # its end, and only then does the cancellation go through.
            pids_file.unlink()
            run = asyncio.create_task(driver.run_agent(ACTOR, "stubborn", tier=FREE))
            await appears(pids_file)
            pids = [int(p) for p in pids_file.read_text().split()]
            start = time.monotonic()
            run.cancel()
            await asyncio.sleep(0.1)
            assert not run.done() and all(alive(p) for p in pids)
            run.cancel()
            with pytest.raises(asyncio.CancelledError):
                await run
            assert not any(alive(p) for p in pids) and time.monotonic() - start >= 0.25
            assert driver.left_running == harness.LEFT_NOTHING

            # The same for the model's own command.
            pids_file.unlink()
            run = asyncio.create_task(
                driver.run_ssh('trap "" TERM; echo $$ > "$HOME/pids"; sleep 60', 30)
            )
            pid = await appears(pids_file)
            run.cancel()
            await asyncio.sleep(0.1)
            assert not run.done() and alive(pid)
            run.cancel()
            with pytest.raises(asyncio.CancelledError):
                await run
            assert not alive(pid) and driver.left_running == harness.LEFT_NOTHING
            await driver.disconnect()

    asyncio.run(scenario())


def test_a_link_gone_silent_is_noticed_and_nothing_is_claimed_about_the_host(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "KEEPALIVE_INTERVAL", 0.2)
    monkeypatch.setattr(ssh_driver, "KEEPALIVE_COUNT", 2)
    monkeypatch.setattr(ssh_driver, "STOP_GRACE", 0.2)
    monkeypatch.setattr(ssh_driver, "SWEEP_TIMEOUT", 0.4)

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host, Relay(host.port) as relay:
            host.authorized.append(blob(tmp_path / "id"))
            host.tool("actor", "actor")
            pids_file = host.home / "pids"
            config = host.config()
            config.ssh.port = relay.port
            # (A link that is merely quiet is not a dead one: the host answers keepalives.)
            quiet = SshDriver(config, device_key=tmp_path / "id")
            await quiet.connect()
            assert await quiet.run("sleep 1; echo still here", 5) == "still here"
            await quiet.disconnect()
            runner, finished, toasts = await runner_on(tmp_path, host, config=config)

            # The peer falls asleep under a tool that has an hour left: noticed by the
            # keepalive, not at the end of the hour, and said as what it is.
            job = AgentJob(ACTOR, timeout_s=3600)
            seen: list[dict[str, Any]] = []
            runner.emit_event = seen.append
            await runner.submit_task("fake", "polite", FREE, agent=job)
            await appears(pids_file)
            pids = [int(p) for p in pids_file.read_text().split()]
            await wait_for(lambda: seen[-1]["summary"] == "started")  # its output got through
            relay.frozen = True
            start = time.monotonic()
            await wait_for(lambda: finished, 5)
            assert time.monotonic() - start < 3
            assert (finished[0]["status"], finished[0]["summary"]) == (
                "failed",
                "The connection to fake closed before Actor finished: it may still be "
                "running there. Its last output:\nstarted",
            )
            assert all(alive(p) for p in pids)  # ...as it is: nothing reached the host

        async with FakeHost(tmp_path / "2") as host, Relay(host.port) as relay:
            host.authorized.append(blob(tmp_path / "2" / "id"))
            host.tool("actor", "actor")
            pids_file = host.home / "pids"
            config = host.config()
            config.ssh.port = relay.port
            # The kill switch on a link that has just died (the keepalive has not
            # noticed yet): back soon, and with no claim that the tool was stopped.
            monkeypatch.setattr(ssh_driver, "KEEPALIVE_INTERVAL", 30)
            runner, finished, toasts = await runner_on(tmp_path / "2", host, config=config)
            await runner.submit_task("fake", "polite", FREE, agent=AgentJob(ACTOR, timeout_s=3600))
            await appears(pids_file)
            pids = [int(p) for p in pids_file.read_text().split()]
            relay.frozen = True
            start = time.monotonic()
            await runner.kill_all()
            assert time.monotonic() - start < 2 and runner.agent_state == "IDLE"
            assert (finished[0]["status"], finished[0]["summary"]) == (
                "cancelled",
                "Stopped on this device by the kill switch, but fake did not confirm that "
                "Actor was stopped: it may still be running there.",
            )
            assert toasts == ["Kill switch: Actor may still be running on fake"]
            assert all(alive(p) for p in pids)

    asyncio.run(scenario())


def test_two_logins_authorising_at_once_do_not_trip_over_the_missing_folder(
    tmp_path: Path,
) -> None:
    """The script itself, with a `mkdir` that loses the race: ~/.ssh appears between the
    look and the making, as it does when another login is a moment ahead."""
    home, bin_dir = tmp_path / "home", tmp_path / "bin"
    home.mkdir()
    bin_dir.mkdir()
    racing = bin_dir / "mkdir"
    racing.write_text('#!/bin/sh\n/bin/mkdir "$@" 2>/dev/null\nexec /bin/mkdir "$@"\n')
    racing.chmod(0o755)
    key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA device@lapis"
    done = subprocess.run(
        ["/bin/sh", "-c", ssh_driver.AUTHORIZE, "sh", key.rsplit(" ", 1)[0], key],
        env={"HOME": str(home), "PATH": f"{bin_dir}:/usr/bin:/bin"},
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert (done.returncode, done.stdout, done.stderr) == (0, "LAPIS-KEY-ADDED\n", "")
    assert (home / ".ssh" / "authorized_keys").read_text() == key + "\n"
    assert file_mode(home / ".ssh") == 0o700
