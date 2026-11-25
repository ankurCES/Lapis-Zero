"""Agent command-line tools on a host (Claude Code, opencode, Freya, ...) that run a chat.

One table entry per tool: how to run one prompt headless, how to continue its session,
which of the host's policy tiers it can be held to (and by which of its own permission
flags), and how to read what it printed. `command()` turns an entry and a message into
the script the SSH driver runs under the account's login shell; `answer()` turns what
came back into the chat's reply. Nothing here imports a driver, so the table is testable
on its own.
"""

from __future__ import annotations

import json
import re
import shlex
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from typing import Any

LAPIS = "lapis"  # the device's own agent: the LLM loop in runner.py
PROMPT = "{prompt}"
SESSION = "{session}"
ANSWER_MAX = 16_000  # characters of a tool's answer kept as the chat reply
TAIL_MAX = 1_500  # characters of output quoted when a run fails
PROGRESS_MAX = 160
ID_RE = re.compile(r"[a-z][a-z0-9_-]{0,31}")
# A session id is stored and later placed on a command line: only the plain ones are
# kept, and none that starts with a dash (the tool would read it as an option).
SESSION_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}")
TIERS = ("strict", "balanced", "autonomous")  # the host policy tiers, the tightest first
# What may still be running on the host after a command was stopped (the kill switch, a
# time limit), as far as the host itself confirmed it. The SSH driver reports one of these.
LEFT_NOTHING = ""  # the host reported the end of the command and of all that descends from it
LEFT_STARTED = "started"  # the command ended; what it had started was not looked for
LEFT_UNKNOWN = "unknown"  # the host confirmed nothing: the command itself may run on
KEPT = "64 KB"  # of one run's output (ssh_driver.OUTPUT_KEEP), for the sentence below
_ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[@-Z\\-_])")


@dataclass
class Answer:
    """What one run of a tool came to."""

    text: str
    session: str = ""  # the tool's own session id, when it reports one
    is_error: bool = False
    lost_session: bool = False  # the tool no longer knows the session it was asked to continue
    # A tool that answers in JSON printed none that could be read: `text` is its output as
    # it came (it said in plain words why it stopped, or the answer was cut: see `answer`).
    raw: bool = False


def clean(text: str) -> str:
    """Terminal output as plain text: no escape sequences, no carriage returns."""
    return _ANSI.sub("", text).replace("\r", "").strip()


def _session(value: Any) -> str:
    return value if isinstance(value, str) and SESSION_RE.fullmatch(value) else ""


def _json_lines(output: str) -> list[Any]:
    found = []
    for line in output.splitlines():
        line = line.strip()
        if line[:1] in ("{", "["):
            try:
                found.append(json.loads(line))
            except (ValueError, RecursionError):  # cut by the 64 KB limit, or not JSON at all
                continue
    return found


def read_text(output: str) -> Answer:
    return Answer(clean(output))


def read_claude(output: str) -> Answer:
    """`claude -p --output-format json`: one object with `result`, `session_id`, `is_error`."""
    # What Claude Code 2.1 prints when `--resume` names a session it no longer has.
    lost = "No conversation found with session ID" in output
    for data in reversed(_json_lines(output)):
        if isinstance(data, list):  # the verbose form: every message, the result last
            data = next(
                (d for d in reversed(data) if isinstance(d, dict) and d.get("type") == "result"),
                None,
            )
        if isinstance(data, dict) and (data.get("type") == "result" or "session_id" in data):
            text = data.get("result") if isinstance(data.get("result"), str) else ""
            return Answer(
                text.strip() or str(data.get("subtype") or ""),
                _session(data.get("session_id")),
                bool(data.get("is_error")),
                lost,
            )
    # No result object: it stopped before answering and said why in plain words.
    return Answer(clean(output), lost_session=lost, raw=True)


def read_opencode(output: str) -> Answer:
    """`opencode run --format json`: one event per line, each carrying `sessionID`.

    Read from the `run` command's source in opencode 1.4.3: events are
    `{type, timestamp, sessionID, ...}` with type `text` (`part.text` is a finished piece
    of the answer), `tool_use`, `step_start`, `step_finish`, `reasoning` or `error`
    (`error.data.message`, else `error.name`).
    """
    texts: list[str] = []
    errors: list[str] = []
    session = ""
    for event in _json_lines(output):
        if not isinstance(event, dict):
            continue
        session = session or _session(event.get("sessionID"))
        part, error = event.get("part"), event.get("error")
        if event.get("type") == "text" and isinstance(part, dict):
            texts.append(str(part.get("text") or "").strip())
        elif event.get("type") == "error" and isinstance(error, dict):
            data = error.get("data")
            message = data.get("message") if isinstance(data, dict) else None
            errors.append(str(message or error.get("name") or "error"))
    if not (session or texts or errors):
        return Answer(clean(output), raw=True)  # not the event stream: show what it printed
    said = "\n\n".join(t for t in texts if t)
    return Answer("\n".join(errors) if errors else said, session, bool(errors))


def show_line(line: str) -> str:
    return clean(line)[:PROGRESS_MAX]


def show_nothing(line: str) -> str:
    """For a tool that prints only its final answer (as JSON): nothing worth a ticker line."""
    return ""


def show_opencode(line: str) -> str:
    """The tool being used, or the start of what is being said."""
    for event in _json_lines(line):
        part = event.get("part") if isinstance(event, dict) else None
        if not isinstance(part, dict):
            continue
        if event.get("type") == "tool_use":
            state = part.get("state")
            title = state.get("title") if isinstance(state, dict) else ""
            return clean(f"{part.get('tool') or 'tool'} {title or ''}")[:PROGRESS_MAX]
        if event.get("type") == "text":
            return clean(str(part.get("text") or "")).split("\n")[0][:PROGRESS_MAX]
    return ""


@dataclass(frozen=True)
class Harness:
    """One agent tool. The command line is `binary run... tier... resume... prompt...`."""

    id: str
    name: str
    binary: str
    run: tuple[str, ...] = ()  # arguments of every headless run
    prompt: tuple[str, ...] = ()  # how the message is passed (PROMPT marks it); () = on stdin
    resume: tuple[str, ...] = ()  # continuing a session (SESSION marks its id); () = it cannot
    # The host policy tiers this tool can be held to, each with the permission flags of
    # its own that hold it there (none, where its behaviour without flags already does).
    # It is never run under a tier that is not listed: see `refusal`.
    tiers: dict[str, tuple[str, ...]] = field(default_factory=dict)
    read: Callable[[str], Answer] = read_text
    show: Callable[[str], str] = show_line  # one line of output -> a progress line ("" = none)
    # Where the program also lives when it is not on the PATH, relative to the home folder.
    places: tuple[str, ...] = ()


_CLAUDE_TIERS = {
    "strict": ("--permission-mode", "plan"),
    "balanced": ("--permission-mode", "acceptEdits"),
    "autonomous": ("--permission-mode", "bypassPermissions"),
}
_GEMINI_TIERS = {
    "strict": ("--approval-mode", "plan"),
    "balanced": ("--approval-mode", "auto_edit"),
    "autonomous": ("--approval-mode", "yolo"),
}

# Flags are the ones each tool's own `--help` lists (Claude Code 2.1, opencode 1.4,
# Gemini CLI, Cursor Agent) or its source (Freya's main.go). Codex is not installed
# where this was written, so it gets its documented `exec` subcommand and nothing else.
#
# A tier is listed only where the tool's help or source says what holds it there. What
# it does without such a flag is unknown or too much, so "autonomous" (where anything
# goes) is the one tier every tool can be given, and for some the only one:
# - opencode 1.4.3 has no flag for less. `run` rejects what its configuration would ask
#   about, but its built-in agent allows edits and shell commands outright. The
#   OPENCODE_PERMISSION variable (JSON merged into the `permission` of its config) could
#   deny them, but a version that does not know the variable ignores it in silence, and
#   an `agent.<name>.permission` in the host's own opencode.json is merged after it and
#   wins: this device could not know that the tier is kept.
# - Freya without `--yolo` runs reads, writes and shell commands and blocks only what it
#   classes as destructive (headless/agent.go): that is "balanced", not "strict".
# - Cursor Agent's help says of `-p`: "Has access to all tools, including write and
#   shell", and names `--mode plan` (read-only) and `--force`; nothing in between.
# - Codex could not be looked at.
BUILTIN: dict[str, Harness] = {
    h.id: h
    for h in (
        Harness(
            "claude",
            "Claude Code",
            "claude",
            run=("-p", "--output-format", "json"),
            resume=("--resume", SESSION),
            tiers=_CLAUDE_TIERS,
            read=read_claude,
            show=show_nothing,
            places=(".local/bin/claude", ".claude/local/claude"),
        ),
        Harness(
            "opencode",
            "opencode",
            "opencode",
            # The message goes on stdin: given as an argument, `opencode run` wraps any
            # message containing a space in quotes of its own.
            run=("run", "--format", "json"),
            resume=("-s", SESSION),
            # With the flag `run` approves whatever its own config would ask about.
            tiers={"autonomous": ("--dangerously-skip-permissions",)},
            read=read_opencode,
            show=show_opencode,
            places=(".opencode/bin/opencode",),
        ),
        Harness(
            "freya",
            "Freya",
            "lumi-freya",
            run=("agent",),
            prompt=(PROMPT,),
            tiers={"balanced": (), "autonomous": ("--yolo",)},
        ),
        Harness(
            "codex", "Codex", "codex", run=("exec",), prompt=(PROMPT,), tiers={"autonomous": ()}
        ),
        Harness("gemini", "Gemini CLI", "gemini", prompt=("-p", PROMPT), tiers=_GEMINI_TIERS),
        Harness(
            "cursor",
            "Cursor Agent",
            "cursor-agent",
            run=("-p", "--output-format", "text", "--trust"),
            prompt=(PROMPT,),
            tiers={"strict": ("--mode", "plan"), "autonomous": ("--force",)},
        ),
    )
}


def parse_command(command: str) -> tuple[str, ...]:
    """The words of an `[agents.<id>] command`; ValueError says what is wrong with it."""
    command = command.strip()
    if not command or len(command) > 500 or not command.isprintable():
        raise ValueError("must be one line of at most 500 printable characters")
    try:
        words = shlex.split(command)
    except ValueError as exc:
        raise ValueError(f"is not a command line ({exc})") from None
    if words[:1] == [PROMPT] or [w for w in words if PROMPT in w] != [PROMPT]:
        raise ValueError(
            "must be a program and its arguments, with {prompt} once as a whole word "
            "of its own, e.g. `aider --yes --message {prompt}`"
        )
    return tuple(words)


def check_custom_id(agent_id: str) -> None:
    if not ID_RE.fullmatch(agent_id) or agent_id == LAPIS or agent_id in BUILTIN:
        raise ValueError(
            "an agent id is 1 to 32 lowercase letters, digits, _ or -, starts with a "
            f"letter, and is none of the built-in ones ({', '.join([LAPIS, *BUILTIN])})"
        )


def custom(
    agent_id: str, name: str, command: str, tiers: Iterable[str] = ("autonomous",)
) -> Harness:
    """A tool from `[agents.<id>]` in lapis.toml: plain-text answer, no session.

    Its one command line is all this device knows of it, so it runs under the
    autonomous policy only, unless the owner lists other tiers in the entry and so
    vouches that the command line as written keeps the tool within them.
    """
    words = parse_command(command)
    return Harness(agent_id, name, words[0], prompt=words[1:], tiers={t: () for t in tiers})


def all_tools(configured: dict[str, Any]) -> dict[str, Harness]:
    """Built-in tools plus the `[agents]` table of the config."""
    return BUILTIN | {i: custom(i, a.name, a.command, a.tiers) for i, a in configured.items()}


def refusal(tool: Harness, tier: str, host: str, tools: dict[str, Harness]) -> str:
    """Why `tool` is not started under the policy tier `tier`, and what the owner can do.

    A tool on the host is not put up for approval step by step: its own permission
    settings have to keep it within the tier. Where it has none for a tier, running it
    anyway would let it edit files and run commands that the policy reserves for the
    owner's approval, so it is not run at all.
    """
    can = [t for t in TIERS if t in tool.tiers]
    others = [t.name for t in tools.values() if tier in t.tiers and t.id != tool.id]
    text = (
        f"{tool.name} was not started: it cannot be held to the {tier} policy that applies "
        f"to this chat on {host}. It can be run under {' or '.join(can)}: set that as this "
        "chat's policy to allow it"
    )
    if others:
        text += f", or pick a tool that keeps to {tier}: {', '.join(others)}"
    return text + "."


# -- the command line: a security boundary ---------------------------------------

_PLAIN = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 ")
_BARE = re.compile(r"[A-Za-z0-9_@%+=:,./-]+")
# Prints where a program is: on the PATH, else the first of the given places under $HOME.
_WHERE = (
    'w() { command -v "$1" 2>/dev/null && return; shift; '
    'for c in "$@"; do [ -x "$HOME/$c" ] && { printf "%s\\n" "$HOME/$c"; return; }; done; '
    "return 1; }"
)


def printf_format(text: str) -> str:
    """`text` as a printf format of letters, digits, spaces and `\\ooo` escapes only.

    The message is the owner's, but it may hold anything: quotes, `$(...)`, backticks,
    newlines, `;`. It crosses three shells on its way (the account's login shell twice,
    then /bin/sh), and quoting rules differ between them (fish reads `\\'` inside single
    quotes). Written this way there is nothing in it for any of them to interpret: only
    printf, at the far end, turns it back into the bytes it was.
    """
    return "".join(chr(b) if b in _PLAIN else f"\\{b:03o}" for b in text.encode(errors="replace"))


def _word(text: str) -> str:
    """`text` (which has no trailing newline) as one word of a POSIX sh command."""
    return text if _BARE.fullmatch(text) else f"\"$(printf '{printf_format(text)}')\""


def _where(tool: Harness) -> str:
    return " ".join(["w", _word(tool.binary), *(_word(p) for p in tool.places)])


def command(tool: Harness, message: str, *, tier: str, session: str = "", workdir: str = "") -> str:
    """The POSIX sh script that runs `tool` once on `message`; its exit status is the tool's.

    127 with nothing printed means the tool is not installed. ValueError for a policy
    tier the tool cannot be held to: no command line is ever built for one.
    """
    if tier not in tool.tiers:
        raise ValueError(f"it cannot be held to the {tier or 'unnamed'} policy")
    message = message.replace("\x00", "")  # cannot be passed in an argument; means nothing
    args = [*tool.run, *tool.tiers[tier]]
    if session and tool.resume and SESSION_RE.fullmatch(session):
        args += [session if a == SESSION else a for a in tool.resume]
    words = [_word(a) for a in args]
    if tool.prompt:
        # One argument, whatever it holds. A message starting with "-" would be read as
        # an option of the tool (`--yolo` raises Freya's own permissions): a leading
        # space keeps it a message for every argument parser.
        if message.startswith("-"):
            message = " " + message
        words += ['"$p"' if a == PROMPT else _word(a) for a in tool.prompt]
        run = " ".join(['"$t"', *words])
    else:
        run = " ".join(["printf %s", '"$p"', "|", '"$t"', *words])
    go = ""
    if workdir:
        go = (
            f"d={_word(workdir)}; "
            "case $d in '~') d=$HOME;; '~/'*) d=$HOME/${d#'~/'};; esac; "
            '{ cd -- "$d" 2>/dev/null || '
            "{ printf 'Folder not found: %s\\n' \"$d\"; false; }; } && "
        )
    return (
        # The x keeps the message's own trailing newlines through $(...).
        f"p=$(printf '{printf_format(message)}x'); p=${{p%x}}; {_WHERE}; "
        f"if t=$({_where(tool)}); then {go}{run}; else (exit 127); fi"
    )


def detect_command(tools: dict[str, Harness]) -> str:
    """One script that prints `<id> <path>` for every tool installed on the host."""
    found = [f"t=$({_where(t)}) && printf '%s %s\\n' {t.id} \"$t\"" for t in tools.values()]
    return "; ".join([_WHERE, *found, ":"])


def detected(tools: dict[str, Harness], output: str) -> list[dict[str, str]]:
    """`detect_command`'s output as the list `GET /v1/hosts/{id}/agents` returns."""
    found = []
    for line in clean(output).splitlines():
        agent_id, _, path = line.strip().partition(" ")
        if agent_id in tools and path.startswith("/"):
            found.append({"id": agent_id, "name": tools[agent_id].name, "path": path})
    return found


def _quoted(text: str) -> str:
    text = clean(text)[-TAIL_MAX:].strip()
    return f" Its last output:\n{text}" if text else ""


def answer(tool: Harness, run: Any, host: str, timeout_s: float) -> Answer:
    """What the chat is told about one finished run (`run` is the SSH driver's ShellRun)."""
    if not run.started:
        return Answer(
            f"{tool.name} could not be started on {host}: the login shell of the account "
            "there did not run the command.",
            is_error=True,
        )
    read = tool.read(run.output)
    last = _quoted(read.text or run.output)
    if run.timed_out:
        limit = f"{timeout_s:g} seconds" if timeout_s < 120 else f"{timeout_s / 60:g} minutes"
        # Only what the host confirmed is claimed (see LEFT_...).
        if run.left == LEFT_UNKNOWN:
            text = (
                f"{tool.name} did not finish within {limit}, and {host} did not confirm "
                f"that it was stopped: it may still be running there.{last}"
            )
        elif run.left == LEFT_STARTED:
            text = (
                f"{tool.name} was stopped after {limit} without finishing, but the programs "
                f"it had started on {host} could not be checked: some may still be running "
                f"there.{last}"
            )
        else:
            text = f"{tool.name} was stopped after {limit} without finishing.{last}"
    elif run.exit_status is None:
        still = ": it may still be running there." if run.left else "."
        text = f"The connection to {host} closed before {tool.name} finished{still}{last}"
    elif run.exit_status == 127 and not clean(run.output):
        text = (
            f"{tool.name} is not installed on {host}: no `{tool.binary}` on the PATH of the "
            "login shell there."
        )
    elif run.exit_status:
        text = f"{tool.name} failed (exit status {run.exit_status}).{last}"
    elif run.cut and read.raw:
        # Its JSON did not fit: neither `is_error` nor the session id can be read from a
        # piece of it, and the piece is not an answer.
        text = (
            f"{tool.name} printed more than the {KEPT} this device keeps of one run, so its "
            f"answer could not be read and whether it succeeded is not known.{last}"
        )
        return Answer(text, is_error=True)
    elif read.is_error:
        text = f"{tool.name} reported an error.{last}"
    else:
        text = read.text[-ANSWER_MAX:] or f"{tool.name} finished without printing an answer."
        return Answer(text, read.session)
    return Answer(text, read.session, True, read.lost_session)
