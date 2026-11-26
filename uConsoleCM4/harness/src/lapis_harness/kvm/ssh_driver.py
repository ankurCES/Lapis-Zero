"""SSH to a host: login without thought, short commands, and long runs of an agent tool.

Login tries the host's identity file, then the device's own key, then (only when the
owner named one) the account's password. `run` is the model's `ssh_exec`: a short
command, the first 4 KB of its output (SPEC 7.2). `login_shell` runs a script under the
account's own login shell for as long as an agent tool needs and keeps the last 64 KB of
what it prints. Both read the output as it arrives into a bounded buffer, and both stop
what they started on the host before they return: at the time limit, and when the task
is cancelled (the kill switch). `left` says how much of that the host confirmed.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import secrets
import shlex
import time
from collections.abc import Callable, Coroutine
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from lapis_harness.agent.harness import LEFT_NOTHING, LEFT_STARTED, LEFT_UNKNOWN
from lapis_harness.config import HostConfig
from lapis_harness.kvm.device_key import ensure_device_key

TRUNCATE_MARKER = "\n… (truncated to {limit} bytes)"
LOGIN_METHODS = ("identity file", "device key", "password")
OUTPUT_KEEP = 64 * 1024  # bytes of a long run's output held in RAM: the newest
PROGRESS_INTERVAL = 0.5  # seconds between two progress lines
STOP_GRACE = 1.0  # seconds a stopped command gets to end by itself before it is killed
SWEEP_TIMEOUT = 2.0  # seconds one step of SWEEP may take; past that the host is not answering
START_WAIT = 10.0  # seconds to wait for a command whose start was cancelled, to stop it
# A host that goes silent (asleep, off the network) is noticed when COUNT keepalives in a
# row went unanswered, a minute at most, not at the end of a run's time limit.
KEEPALIVE_INTERVAL = 15
KEEPALIVE_COUNT = 3
# Most of what a Linux kernel accepts as one argument (128 KiB): the command is one.
COMMAND_MAX = 100_000
# Adds this device's key to ~/.ssh/authorized_keys. $1: key type and blob, to look for;
# $2: the whole line. Appends only: no other line is rewritten, nothing that exists is
# re-moded; what it creates is private (umask 077: ~/.ssh 0700, the file 0600).
AUTHORIZE = (
    'umask 077; d="$HOME/.ssh"; f="$d/authorized_keys"; '
    # -p: two calls at once both find no ~/.ssh, and the slower one must not fail on it.
    'mkdir -p "$d" || exit 1; '
    'if [ -f "$f" ] && grep -qF -- "$1" "$f"; then echo LAPIS-KEY-PRESENT; exit 0; fi; '
    # A last line without its newline would swallow the key.
    'if [ -s "$f" ] && [ -n "$(tail -c 1 "$f")" ]; then echo >> "$f" || exit 1; fi; '
    'printf "%s\\n" "$2" >> "$f" && echo LAPIS-KEY-ADDED'
)
# One step of stopping a run: a /bin/sh script for a channel of its own. $1 is the step,
# the rest are the process ids known so far (at first the one the wrapper reported).
#   ask: they and every process descended from them are sent TERM; the whole list is
#        printed, the deepest first.
#   end: what is left of them, and what they started meanwhile, is frozen (nothing forks
#        or slips away while it is counted), then killed, the deepest first; what is
#        still there a moment later is printed (ordinarily nothing).
# The family tree is read from `ps`. A tool starts its shell commands in sessions of
# their own, where no signal to the tool's session reaches them, but they keep their
# parent. A process handed to init because its parent died first is found only if it
# was listed before that; one made to escape (a double fork) is not found at all.
# ponytail: process ids are trusted for the second between the two steps. One that
# ended and was given to an unrelated program of the same account within it would be
# killed; start times would tell, if that ever matters.
# No backslash and no "!" in it: the account's shell (zsh, fish, csh ...) reads this
# line once, quoted, before /bin/sh does.
SWEEP = (
    'm=$1; shift; tree() { l=$(ps -A -o pid=,ppid=) && [ -n "$l" ] || return 1; '
    'echo "$l" | awk -v known="$*" \''
    'BEGIN { n = split(known, k, " "); for (i = 1; i <= n; i++) at[k[i]] = 1 } '
    "{ up[$1] = $2 } "
    "END { do { grew = 0; for (p in up) if ((p in at) == 0 && (up[p] in at)) "
    "{ at[p] = at[up[p]] + 1; if (at[p] > deep) deep = at[p]; grew = 1 } } while (grew); "
    'for (d = deep; d > 1; d--) for (p in at) if (at[p] == d) printf "%s ", p; '
    'for (i = 1; i <= n; i++) printf "%s ", k[i] }\'; }; '
    "case $m in "
    'ask) t=$(tree "$@") || exit 3; set -- $t; kill -s TERM "$@" 2>/dev/null; '
    'echo "LAPIS-TREE $*";; '
    "end) n=0; i=0; while [ $i -lt 9 ]; do i=$((i + 1)); "
    't=$(tree "$@") || exit 3; set -- $t; kill -s STOP "$@" 2>/dev/null; '
    "[ $# -eq $n ] && break; n=$#; done; "
    'kill -s KILL "$@" 2>/dev/null; i=0; while :; do '
    'l=$(ps -A -o pid=) || exit 3; l=" $(echo $l) "; left=; '
    'for p in "$@"; do case $l in *" $p "*) left="$left $p";; esac; done; i=$((i + 1)); '
    '[ -z "$left" ] || [ $i -gt 10 ] && break; sleep 0.05 2>/dev/null || sleep 1; done; '
    'echo "LAPIS-LEFT$left";; '
    "esac"
)
SWEEP_ANSWER = {"ask": "LAPIS-TREE", "end": "LAPIS-LEFT"}


def truncate_output(text: str, limit: int = 4096, more: bool = False) -> str:
    """Combined stdout/stderr frame; keep the head and append a truncation marker.

    `more`: the command printed more than `text` (the rest was never held in memory).
    """
    if len(text) <= limit and not more:
        return text
    return text[:limit] + TRUNCATE_MARKER.format(limit=limit)


async def _to_the_end(stopping: Coroutine[Any, Any, None]) -> None:
    """Await `stopping` to its end, whatever happens meanwhile to the task that waits.

    Stopping a remote command is a few messages with pauses between them. A cancellation
    that lands in the middle (the kill switch pressed again, the daemon shutting down)
    would leave the command running, so it is held back and raised once the stop is done.
    """
    task = asyncio.ensure_future(stopping)
    cancelled = False
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError:
            cancelled = True
    if cancelled:
        raise asyncio.CancelledError
    task.result()


# How the account's own shell is started for its environment: as an interactive login
# shell, running the one command line in $1. csh and tcsh take -l only when it stands
# alone ("Unknown option: -lic"); their -c reads ~/.cshrc all the same, though not ~/.login.
_LOGIN = 'case $SHELL in *csh) exec "$SHELL" -c "$1";; *) exec "$SHELL" -lic "$1";; esac'


def login_shell_command(script: str, nonce: str) -> str:
    """The command line that runs `script` (POSIX sh) under the account's login shell.

    A plain `ssh host command` gets the bare system PATH, and tools such as claude or
    opencode live in ~/.local/bin or under nvm, which only the interactive login shell
    adds. So that shell is started for its environment and at once replaced by /bin/sh,
    which runs the script: the account's shell (zsh, bash, fish, csh ...) only ever
    parses two fixed command lines whose every variable part is one quoted word.

    Start-up files print things, so the script's own output sits between two marker
    lines carrying `nonce`. The first one ends with the process id of the shell that
    runs the script: the ancestor of everything the script starts, which is what a stop
    goes by (see SWEEP). The second one ends with the script's exit status. Everything
    reads /dev/null: a start-up file that asks a question, or one that replaces the
    shell with another, ends at once instead of waiting for a keyboard.
    """
    # The markers are printed in two halves: a shell that echoes its command line
    # (xtrace, a hook that sets the window title) must not print a marker itself.
    # `trap : TERM`, twice (a subshell starts without its parent's traps): stopping a
    # run signals the whole process group, and these two shells have to outlive that,
    # or the channel closes with them and a tool that ignores TERM can no longer be
    # killed through it. A handler, not "ignore": the tool itself starts with TERM as
    # it was.
    marked = (
        f"trap : TERM; printf '\\n%s%s %s\\n' LAPIS- BEGIN-{nonce} \"$$\"; "
        f"{{ trap : TERM; {script}; printf '\\n%s%s %s\\n' LAPIS- END-{nonce} \"$?\"; }}"
        " 2>&1 | cat"
    )
    inner = f"exec /bin/sh -c {shlex.quote(marked)}"
    return (
        f"exec env TERM=dumb NO_COLOR=1 /bin/sh -c {shlex.quote(_LOGIN)} sh "
        f"{shlex.quote(inner)} </dev/null"
    )


@dataclass
class ShellRun:
    """What a `login_shell` script printed between its markers, and how it ended."""

    output: str = ""
    exit_status: int | None = None  # None: the end marker never came
    started: bool = False  # the start marker came: the login shell did run the script
    timed_out: bool = False
    cut: bool = False  # output before the last OUTPUT_KEEP bytes was dropped
    # After a stop (the time limit, a lost connection): what of it may still be running
    # on the host, one of harness.LEFT_...
    left: str = LEFT_NOTHING


class _Collector:
    """Keeps the newest `keep` bytes printed between the two marker lines."""

    def __init__(self, nonce: str, keep: int = OUTPUT_KEEP) -> None:
        self.begin = f"LAPIS-BEGIN-{nonce}".encode()
        self.end = f"LAPIS-END-{nonce} ".encode()
        self.keep = keep
        self.buf = bytearray()
        self.carry = b""  # a line still waiting for its newline
        self.started = self.cut = False
        self.root = 0  # the process id the wrapper reported with its start marker
        self.exit_status: int | None = None
        self.last_line = b""

    def _add(self, data: bytes) -> None:
        self.buf += data
        if len(self.buf) > self.keep:
            del self.buf[: len(self.buf) - self.keep]
            self.cut = True

    def feed(self, data: bytes) -> None:
        *lines, self.carry = (self.carry + data).split(b"\n")
        for raw in lines:
            line = raw.rstrip(b"\r")  # the remote terminal ends lines with \r\n
            if not self.started:
                marker, _, pid = line.partition(b" ")
                self.started = marker == self.begin
                if self.started and pid.isdigit() and int(pid) > 1:
                    self.root = int(pid)
            elif line.startswith(self.end):
                status = line[len(self.end) :]
                self.exit_status = int(status) if status.isdigit() else 1
                self.carry = b""
                return
            else:
                self._add(raw + b"\n")
                self.last_line = line.strip() or self.last_line
        if len(self.carry) > self.keep:  # one endless line is output like any other
            self.finish()

    def finish(self) -> None:
        """The stream ended (or a line outgrew the buffer): what is left over counts too."""
        if self.started and self.exit_status is None:
            self._add(self.carry)
        self.carry = b""

    def result(self, timed_out: bool, left: str = LEFT_NOTHING) -> ShellRun:
        self.finish()
        text = bytes(self.buf).decode(errors="replace").replace("\r\n", "\n")
        return ShellRun(text, self.exit_status, self.started, timed_out, self.cut, left)


class SshDriver:
    """One SSH connection to a host over `asyncssh`, lazily imported."""

    def __init__(
        self,
        host: HostConfig,
        timeout_s: int = 30,
        truncate: int = 4096,
        *,
        device_key: Path | None = None,
        methods: tuple[str, ...] = LOGIN_METHODS,
    ) -> None:
        self.host = host
        self.timeout_s = timeout_s
        self.truncate = truncate
        self.device_key = device_key
        self.methods = methods
        self.login = ""  # how the current login succeeded: one of LOGIN_METHODS
        # After the last command was stopped (time limit, kill switch): what of it may
        # still be running on the host, one of harness.LEFT_...
        self.left = LEFT_NOTHING
        self._conn: Any = None

    @property
    def connected(self) -> bool:
        return self._conn is not None

    def _keys(self) -> list[tuple[str, Any]]:
        """The private keys to offer, in order, each with the name of its method."""
        import asyncssh

        keys = []
        for method, path in (
            ("identity file", self.host.ssh.identity_file),
            ("device key", self.device_key),
        ):
            if not path or method not in self.methods:
                continue
            # A key that cannot be made or read (a missing identity file, one with a
            # passphrase, a read-only disk) is skipped: the next method gets its turn.
            with contextlib.suppress(OSError, ValueError):
                if method == "device key":
                    ensure_device_key(Path(path))
                keys.append((method, asyncssh.read_private_key(str(path))))
        return keys

    async def connect(self) -> None:
        import asyncssh  # lazy: only the live driver needs it

        ssh = self.host.ssh
        keys = await asyncio.to_thread(self._keys)
        # Only a password the owner configured is ever sent: some hosts ban an address
        # after a few wrong ones.
        password = (
            os.environ.get(ssh.password_env) or None
            if ssh.password_env and "password" in self.methods
            else None
        )
        tried: list[str] = []  # the methods offered so far; the last one is what got in

        class Login(asyncssh.SSHClient):
            def public_key_auth_requested(self) -> Any:
                if not keys:
                    return None
                method, key = keys.pop(0)
                tried.append(method)
                return key

        def give_password() -> str | None:
            tried.append("password")
            return password

        kwargs: dict[str, Any] = {
            "host": self.host.tailscale_ip,
            "port": ssh.port,
            # ponytail: host keys are not pinned; the transport is already a
            # WireGuard tunnel to a tailnet peer. Pin via known_hosts if hosts
            # outside the tailnet are ever allowed.
            "known_hosts": None,
            "connect_timeout": 15,
            "keepalive_interval": KEEPALIVE_INTERVAL,
            "keepalive_count_max": KEEPALIVE_COUNT,
            # Exactly the login order above: no ~/.ssh/config, default keys or agent of
            # whoever runs the daemon.
            "config": None,
            "client_factory": Login,
            "client_keys": None,
            "agent_path": None,
            "preferred_auth": ("publickey", "keyboard-interactive", "password")
            if password
            else ("publickey",),
        }
        if password:
            kwargs["password"] = give_password
        if ssh.user:
            kwargs["username"] = ssh.user
        self._conn = await asyncssh.connect(**kwargs)
        self.login = tried[-1] if tried else "no credentials asked"

    async def disconnect(self) -> None:
        conn, self._conn = self._conn, None
        if conn is not None:
            conn.close()
            with contextlib.suppress(Exception):
                await asyncio.wait_for(conn.wait_closed(), 5)

    async def run(self, command: str, timeout_s: float | None = None) -> str:
        """The model's `ssh_exec`: the head of what `command` prints, and how it ended.

        The output is read as it arrives and only the first `truncate` bytes of each
        stream are kept: a `yes` or a `find /` must not be collected whole on a 512 MB
        device and cut afterwards. At the time limit, and when the task is cancelled
        (the kill switch), the command is stopped on the host before this returns.
        """
        if self._conn is None:
            raise RuntimeError("SSH not connected")
        import asyncssh

        timeout = timeout_s if timeout_s is not None else self.timeout_s
        self.left = LEFT_NOTHING
        proc = await self._start(command, stdin=asyncssh.DEVNULL, encoding=None)
        out, err = bytearray(), bytearray()
        over = False  # it ended by itself
        try:
            async with asyncio.timeout(timeout):
                await asyncio.gather(self._head(proc.stdout, out), self._head(proc.stderr, err))
                await proc.wait_closed()
            over = True
        except TimeoutError:
            pass
        finally:
            if not over:
                await _to_the_end(self._stop(proc))
        stdout, stderr = (bytes(b[: self.truncate]).decode(errors="replace") for b in (out, err))
        combined = stdout if not stderr else f"{stdout}\n{stderr}"
        more = max(len(out), len(err)) > self.truncate
        shown = truncate_output(combined.strip(), self.truncate, more)
        # Said after the cut, not before it: a long output must not hide how it ended.
        note = f"[exit status {proc.exit_status}]" if proc.exit_status else ""
        if not over:
            note = f"[timed out after {timeout:g}s]"
            if self.left:  # only what the host confirmed is claimed
                note = (
                    f"[timed out after {timeout:g}s; the host did not confirm that the "
                    "command was stopped: it may still be running]"
                )
        return f"{shown}\n\n{note}" if shown and note else shown or note

    async def _head(self, reader: Any, kept: bytearray) -> None:
        """Read a stream to its end, keeping its first `truncate` bytes and one more
        (which says that there were more)."""
        while data := await reader.read(8192):
            kept += data[: max(0, self.truncate + 1 - len(kept))]

    async def authorize(self, public_key: str) -> bool:
        """Put `public_key` (one authorized_keys line) on the host; True if it was there."""
        kind, blob = public_key.split()[:2]
        # /bin/sh, whatever the account's shell is; the key is made of letters, digits
        # and "+/=@.-_" only, so quoting it is the same in every shell.
        out = await self.run(
            f"/bin/sh -c {shlex.quote(AUTHORIZE)} sh "
            f"{shlex.quote(f'{kind} {blob}')} {shlex.quote(public_key)}",
            20,
        )
        if "LAPIS-KEY-PRESENT" in out:
            return True
        if "LAPIS-KEY-ADDED" in out:
            return False
        raise OSError(f"could not write ~/.ssh/authorized_keys there: {out[-200:] or 'no output'}")

    async def login_shell(
        self,
        script: str,
        timeout_s: float,
        on_progress: Callable[[str], None] | None = None,
    ) -> ShellRun:
        """Run `script` under the account's login shell (see `login_shell_command`).

        Returns when the script ends or `timeout_s` passes. Either way, and when the
        calling task is cancelled (the kill switch), what it started on the host is
        stopped first; `left` (here and in the result) says what the host confirmed.
        """
        if self._conn is None:
            raise RuntimeError("SSH not connected")
        import asyncssh

        nonce = secrets.token_hex(8)
        command = login_shell_command(script, nonce)
        if len(command) > COMMAND_MAX:
            raise ValueError("the message is too long to hand over on a command line")
        out = _Collector(nonce)
        self.left = LEFT_NOTHING
        # A terminal on the far side: when this channel closes, the host hangs up every
        # process started in it. Without one they would run on with nobody listening.
        proc = await self._start(command, term_type="dumb", encoding=None, stderr=asyncssh.STDOUT)
        timed_out = False
        shown, shown_at = b"", 0.0
        try:
            async with asyncio.timeout(timeout_s):
                while out.exit_status is None and (data := await proc.stdout.read(8192)):
                    out.feed(data)
                    now = time.monotonic()
                    if (
                        on_progress is not None
                        and out.last_line != shown
                        and now - shown_at >= PROGRESS_INTERVAL
                    ):
                        shown, shown_at = out.last_line, now
                        on_progress(shown.decode(errors="replace"))
        except TimeoutError:
            timed_out = True
        except (asyncssh.Error, OSError):
            pass  # the connection went away under the run: said as that (no end marker)
        finally:
            await _to_the_end(
                # Before the start marker nothing that could tidy up is running yet.
                self._stop(
                    proc, finished=out.exit_status is not None, root=out.root, ask=out.started
                )
            )
        return out.result(timed_out, self.left)

    async def _start(self, command: str, **options: Any) -> Any:
        """Start `command` on the host; returns its handle.

        A cancellation that lands while the request is on its way (the kill switch) must
        not leave the command running there with nobody holding it: its handle is waited
        for and it is stopped like any other, then the cancellation goes on.
        """
        opening = asyncio.ensure_future(self._conn.create_process(command, **options))
        try:
            return await asyncio.shield(opening)
        except asyncio.CancelledError:
            await _to_the_end(self._abandon(opening))
            raise

    async def _abandon(self, opening: asyncio.Future[Any]) -> None:
        """Stop a command whose start nobody waits for any more."""
        try:
            proc = await asyncio.wait_for(opening, START_WAIT)
        except Exception:  # noqa: BLE001 - no handle, no way to stop it or to know
            self.left = LEFT_UNKNOWN
            return
        await self._stop(proc, ask=False)

    async def _stop(
        self, proc: Any, *, finished: bool = False, root: int = 0, ask: bool = True
    ) -> None:
        """End the remote command; `left` then says what of it may still be running."""
        # Set here, not by the caller: the caller may be cancelled again while it waits.
        self.left = await self._end(proc, finished, root, ask)

    async def _end(self, proc: Any, finished: bool, root: int, ask: bool) -> str:
        """The steps of `_stop`; returns one of harness.LEFT_...

        It is asked to end (TERM), given STOP_GRACE, killed, and then the channel is
        closed, which hangs up a session that has a terminal. With `root`, the process id
        the wrapper reported, the asking and the killing go to every process descended
        from it (SWEEP): a tool starts its shell commands in sessions of their own, where
        no signal sent to this session reaches them. Without it, or on a host where that
        cannot be done, the SSH server is asked to signal the session, as far as it
        passes signals on (OpenSSH does from 7.9), and what the tool had started is not
        looked for: the result says so. `finished`: the command has ended by itself and
        only the channel is left to close. `ask=False`: straight to the kill.
        """

        def ended() -> bool:  # the host said how the session's own process ended
            return proc.exit_status is not None or proc.exit_signal is not None

        async def closes() -> bool:  # the channel closes within the grace period
            with contextlib.suppress(Exception):
                await asyncio.wait_for(proc.wait_closed(), STOP_GRACE)
                return True
            return False

        def send(signal: Callable[[], None]) -> None:
            with contextlib.suppress(Exception):  # the channel may be gone already
                signal()

        if finished:
            if not await closes():  # something it left behind holds the session open
                send(proc.kill)
                if not await closes():
                    send(proc.close)
            return LEFT_NOTHING
        tree: list[int] | None = None
        answering = True
        if root:
            try:
                tree = await self._sweep("ask", [root])
            except TimeoutError:
                # Not a word from the host: nothing below can be waited for either, and
                # the kill switch must not hang on a dead link.
                answering = False
        if tree is None and ask:
            send(proc.terminate)
        over = answering and (ask or tree is not None) and await closes()
        swept = False
        if tree and answering:
            with contextlib.suppress(Exception):  # TimeoutError too: then nobody confirmed
                swept = await self._sweep("end", tree) == []
        if not over:
            send(proc.kill)
            over = answering and await closes()
        if not over:
            send(proc.close)
        if swept:
            return LEFT_NOTHING
        if over and ended():
            return LEFT_STARTED if root else LEFT_NOTHING
        return LEFT_UNKNOWN

    async def _sweep(self, step: str, pids: list[int]) -> list[int] | None:
        """One step of SWEEP on the host: the process ids it answered with.

        None when the host cannot do it (no second channel, no `ps`, another kind of
        system); TimeoutError when it does not answer at all.
        """
        import asyncssh

        command = f"/bin/sh -c {shlex.quote(SWEEP)} sh {step} {' '.join(map(str, pids))}"
        try:
            done = await asyncio.wait_for(
                self._conn.run(command, stdin=asyncssh.DEVNULL), SWEEP_TIMEOUT
            )
        except TimeoutError:
            raise
        except Exception:  # noqa: BLE001 - the caller falls back to signalling the session
            return None
        for line in str(done.stdout or "").splitlines():
            answer, _, found = line.partition(" ")
            if answer == SWEEP_ANSWER[step] and all(f.isdigit() for f in found.split()):
                return [int(f) for f in found.split() if int(f) > 1]
        return None
