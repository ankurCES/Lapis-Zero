"""A loopback SSH host for the tests: a real asyncssh server whose sessions run real commands.

It listens on 127.0.0.1 on an ephemeral port with throwaway keys and behaves like sshd
where the driver depends on it:

* logins by public key (an in-memory list plus the account's own
  `~/.ssh/authorized_keys`, so authorising the device can be proven) or by password;
* every command runs as `/bin/sh -c <command>` in a temporary home, with the bare
  system PATH and `$SHELL` set to a stand-in for the account's login shell: a script
  (`fakebin/loginshell`) that prints start-up noise and only then puts `~/bin` on the
  PATH;
* a session that asks for a terminal gets a real pseudo-terminal, so closing its
  channel hangs up the process group exactly as the kernel does for sshd;
* `signal` requests are delivered to the session's process group (`signals = False`:
  dropped, as OpenSSH before 7.9 does).

`Relay` stands between a driver and the host and can go silent: a link that is dead
without either end being told (a peer that fell asleep or left the network).
"""

from __future__ import annotations

import asyncio
import contextlib
import fcntl
import os
import pty
import signal
import socket
import stat
import termios
from pathlib import Path
from typing import Any

import asyncssh

from lapis_harness.config import HostConfig, HostSsh, HostVnc

USER = "tester"
# Stand-ins that are run as programs: `loginshell` (the account's login shell), `claude`
# (records what it is given, answers like Claude Code), `actor` (tools that misbehave)
# and `detacher` (a tool whose own command runs in a session of its own). They are
# files of the repository, and tests link to them: macOS checks every new executable
# on its first run, a fifth of a second each time.
FAKEBIN = Path(__file__).parent / "fakebin"


def _take_terminal() -> None:
    """In the child, after setsid: make the pseudo-terminal the controlling one."""
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


class FakeHost:
    def __init__(self, root: Path, password: str | None = None, asks: bool = False) -> None:
        self.home = root / "home"
        (self.home / "bin").mkdir(parents=True)
        self.shell = FAKEBIN / "loginshell"
        self.password = password
        # True: the password is asked for in a dialogue ("keyboard-interactive", what PAM
        # does on a Mac) instead of being taken by the plain `password` method.
        self.asks = asks
        self.authorized: list[str] = []  # key blobs accepted besides ~/.ssh/authorized_keys
        self.reads_key_file = True  # False: an sshd that keeps its keys somewhere else
        self.signals = True  # False: an sshd that does not pass `signal` requests on
        self.offered: list[str] = []  # key blobs offered by clients, in order
        self.passwords: list[str] = []  # passwords tried by clients
        self.logins = 0  # connections that got past authentication
        self.commands: list[str] = []
        self.terminals: list[str | None] = []  # the terminal type of each session
        self.port = 0
        self.closed_port = 0
        self._server: Any = None
        self._groups: set[int] = set()
        self._sessions: set[asyncio.Future[None]] = set()

    async def __aenter__(self) -> FakeHost:
        self._server = await asyncssh.create_server(
            lambda: _Server(self),
            "127.0.0.1",
            0,
            server_host_keys=[asyncssh.generate_private_key("ssh-ed25519")],
            encoding=None,
        )
        self.port = self._server.sockets[0].getsockname()[1]
        with socket.create_server(("127.0.0.1", 0)) as unused:  # a port nothing listens on
            self.closed_port = unused.getsockname()[1]
        return self

    async def __aexit__(self, *exc: object) -> None:
        self._server.close()
        for group in self._groups:  # whatever a test left running
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(group, signal.SIGKILL)
        with contextlib.suppress(Exception):
            await asyncio.wait_for(asyncio.gather(*self._sessions, return_exceptions=True), 2)
            await asyncio.wait_for(self._server.wait_closed(), 2)

    def config(self, **ssh: Any) -> HostConfig:
        """A host entry pointing at this server."""
        return HostConfig(
            hostname="fake",
            tailscale_ip="127.0.0.1",
            os="macos",
            vnc=HostVnc(port=self.closed_port),  # no screen sharing here: refused at once
            ssh=HostSsh(port=self.port, user=USER, **ssh),
        )

    def tool(self, name: str, stand_in: str = "claude") -> Path:
        """Install a stand-in as `name` in ~/bin: on the PATH of the login shell only."""
        path = self.home / "bin" / name
        path.symlink_to(FAKEBIN / stand_in)
        return path

    def authorized_keys(self) -> Path:
        return self.home / ".ssh" / "authorized_keys"

    def accepts(self, blob: str) -> bool:
        with contextlib.suppress(OSError):
            if self.reads_key_file and blob in self.authorized_keys().read_text():
                return True
        return blob in self.authorized


class _Server(asyncssh.SSHServer):
    def __init__(self, host: FakeHost) -> None:
        self.host = host

    def begin_auth(self, username: str) -> bool:
        return True

    def auth_completed(self) -> None:
        self.host.logins += 1

    def public_key_auth_supported(self) -> bool:
        return True

    def validate_public_key(self, username: str, key: Any) -> bool:
        blob = key.export_public_key().decode().split()[1]
        if self.host.offered[-1:] != [blob]:  # asked twice per key: offer, then signature
            self.host.offered.append(blob)
        return username == USER and self.host.accepts(blob)

    def password_auth_supported(self) -> bool:
        return self.host.password is not None and not self.host.asks

    def validate_password(self, username: str, password: str) -> bool:
        self.host.passwords.append(password)
        return username == USER and password == self.host.password

    def kbdint_auth_supported(self) -> bool:
        return self.host.password is not None and self.host.asks

    def get_kbdint_challenge(self, username: str, lang: str, submethods: str) -> Any:
        return "", "", "", [("Password:", False)]

    def validate_kbdint_response(self, username: str, responses: Any) -> bool:
        return self.validate_password(username, responses[0] if responses else "")

    def session_requested(self) -> Any:
        return _Session(self.host)


class _Session(asyncssh.SSHServerSession):
    def __init__(self, host: FakeHost) -> None:
        self.host = host
        self.chan: Any = None
        self.command = ""
        self.term: str | None = None
        self.proc: Any = None
        self.master: int | None = None  # our side of the session's terminal, if it has one
        self.closed: Any = None  # resolved when that terminal is closed

    def connection_made(self, chan: Any) -> None:
        self.chan = chan

    def pty_requested(self, term_type: str, term_size: Any, term_modes: Any) -> bool:
        self.term = term_type
        return True

    def exec_requested(self, command: str) -> bool:
        self.command = command
        return True

    def shell_requested(self) -> bool:
        return False

    def session_started(self) -> None:
        self.host.commands.append(self.command)
        self.host.terminals.append(self.term)
        self.host._sessions.add(asyncio.ensure_future(self._run()))

    async def _run(self) -> None:
        host = self.host
        env = {
            "HOME": str(host.home),
            "SHELL": str(host.shell),
            "USER": USER,
            "LOGNAME": USER,
            "PATH": "/usr/bin:/bin",
        }
        if self.term is None:
            self.proc = await asyncio.create_subprocess_exec(
                "/bin/sh", "-c", self.command, cwd=host.home, env=env,
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE, start_new_session=True,
            )  # fmt: skip
            host._groups.add(self.proc.pid)
            await asyncio.gather(
                self._pump(self.proc.stdout, self.chan.write),
                self._pump(self.proc.stderr, self.chan.write_stderr),
            )
        else:
            self.master, slave = pty.openpty()
            try:
                self.proc = await asyncio.create_subprocess_exec(
                    "/bin/sh", "-c", self.command, cwd=host.home, env={**env, "TERM": self.term},
                    stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
                    preexec_fn=_take_terminal,
                )  # fmt: skip
            finally:
                os.close(slave)
            host._groups.add(self.proc.pid)
            os.set_blocking(self.master, False)
            self.closed = asyncio.get_running_loop().create_future()
            asyncio.get_running_loop().add_reader(self.master, self._read_terminal)
            await self.closed
        status = await self.proc.wait()
        with contextlib.suppress(Exception):
            self.chan.exit(status if status >= 0 else 128 - status)

    async def _pump(self, reader: Any, write: Any) -> None:
        while data := await reader.read(65536):
            with contextlib.suppress(Exception):  # the client is gone: keep draining
                write(data)

    def _read_terminal(self) -> None:
        assert self.master is not None
        try:
            data = os.read(self.master, 65536)
        except BlockingIOError:
            return
        except OSError:  # Linux reports the last close of the other side as EIO
            data = b""
        if data:
            with contextlib.suppress(Exception):
                self.chan.write(data)
            return
        self._hang_up()

    def _hang_up(self) -> None:
        """Close our side of the terminal: the kernel sends SIGHUP to its foreground group."""
        if self.master is not None:
            asyncio.get_running_loop().remove_reader(self.master)
            os.close(self.master)
            self.master = None
            self.closed.set_result(None)

    def data_received(self, data: bytes, datatype: Any) -> None:
        if self.master is not None:
            os.write(self.master, data)
        elif self.proc is not None and self.proc.stdin is not None:
            self.proc.stdin.write(data)

    def eof_received(self) -> bool:
        if self.master is None and self.proc is not None and self.proc.stdin is not None:
            self.proc.stdin.close()
        return True  # keep the channel open for the output

    def signal_received(self, signal_name: str) -> None:
        if self.proc is not None and self.host.signals:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.proc.pid, getattr(signal, f"SIG{signal_name}"))

    def connection_lost(self, exc: Exception | None) -> None:
        # sshd does the same: a terminal is hung up; plain pipes are just closed, and a
        # command without a terminal runs on.
        self._hang_up()


class Relay:
    """A TCP relay on 127.0.0.1 to `port`. `frozen = True`: nothing passes any more, in
    either direction, and nobody is told."""

    def __init__(self, port: int) -> None:
        self.target = port
        self.frozen = False
        self.port = 0
        self._server: Any = None
        self._pipes: list[asyncio.Future[None]] = []
        self._writers: list[asyncio.StreamWriter] = []

    async def __aenter__(self) -> Relay:
        self._server = await asyncio.start_server(self._accept, "127.0.0.1", 0)
        self.port = self._server.sockets[0].getsockname()[1]
        return self

    async def __aexit__(self, *exc: object) -> None:
        for pipe in self._pipes:
            pipe.cancel()
        for writer in self._writers:  # both ends learn of it now: nothing waits for them
            writer.close()
        self._server.close()

    async def _accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        there_r, there_w = await asyncio.open_connection("127.0.0.1", self.target)
        self._writers += [writer, there_w]
        self._pipes += [
            asyncio.ensure_future(self._pipe(reader, there_w)),
            asyncio.ensure_future(self._pipe(there_r, writer)),
        ]

    async def _pipe(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        with contextlib.suppress(ConnectionError):
            while data := await reader.read(65536):
                while self.frozen:  # swallowed by the dead link
                    await asyncio.sleep(3600)
                writer.write(data)


def file_mode(path: Path) -> int:
    return stat.S_IMODE(path.stat().st_mode)


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True
