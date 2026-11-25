"""The agent-tool table: command lines, the quoting boundary, reading results, config."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from sshhost import FAKEBIN

from lapis_daemon.agent import harness
from lapis_daemon.agent.harness import BUILTIN, TIERS, Answer, answer, command, custom
from lapis_daemon.config import CoreConfig, load_config
from lapis_daemon.kvm.ssh_driver import OUTPUT_KEEP, SWEEP, _Collector, login_shell_command

# Everything a message can do to a shell: substitution, quotes of both kinds, escapes,
# separators, redirection, globs, history, options, newlines (also trailing), other
# scripts, control characters.
HOSTILE = [
    "$(touch PWNED) `touch PWNED` ; touch PWNED & touch PWNED | touch PWNED > PWNED",
    "it's \"quoted\" \\ \\' \\\\ '\\'' $HOME ${HOME} ~ * ? [a-z] !! !$ %s %d \\n \\101 #x",
    "line one\nline two\r\n\ttabbed\n\n\n",
    "--yolo",
    "--dangerously-skip-permissions --model=x",
    "-p -- ; rm -rf /",
    "naïve café 日本語 🙂 ‮\u0007\u001b[31m",
    "'; touch PWNED; echo '",
    "\\'; touch PWNED; echo \\'",
    "x" * 5000 + "'\"$(touch PWNED)",
    "",
]
SHELLS = ("/bin/zsh", "/bin/bash", "/bin/dash", "/bin/ksh", "/bin/tcsh", "/bin/csh")
SHELLS = [s for s in SHELLS if os.path.exists(s)] or ["/bin/sh"]


def unprintf(fmt: str) -> bytes:
    """What printf prints for one of our formats: the check that nothing else is in it."""
    assert re.fullmatch(r"(?:[A-Za-z0-9 ]|\\[0-7]{3})*", fmt), fmt
    return re.sub(rb"\\([0-7]{3})", lambda m: bytes([int(m[1], 8)]), fmt.encode())


def test_a_message_is_written_so_that_no_shell_has_anything_to_read() -> None:
    every_byte = bytes(range(1, 256)).decode("latin-1")
    for text in [*HOSTILE, every_byte]:
        assert unprintf(harness.printf_format(text)) == text.encode()
        for tool in (BUILTIN["claude"], BUILTIN["freya"]):
            script = command(tool, text, tier="autonomous", workdir=text[:40])
            line = login_shell_command(script, "n0")
            # Three shells read this line. What the message becomes holds no quote, no
            # $, no backtick and no backslash other than printf's own \ooo; and no
            # backslash ever stands before a quote or another backslash, the two places
            # where shells disagree about single-quoted text.
            assert "PWNED)" not in line and "`" not in line
            assert "\\'" not in line and "\\\\" not in line
            assert "!" not in line  # csh would look it up in its history
    # The script that stops a run crosses the account's shell the same way.
    assert "\\" not in SWEEP and "!" not in SWEEP and "\n" not in SWEEP
    # A lone surrogate (JSON can carry one) is replaced, never an exception.
    assert harness.printf_format("\ud83d") == "\\077"


def test_hostile_messages_arrive_byte_for_byte_through_real_login_shells(tmp_path: Path) -> None:
    """sshd runs `<login shell> -c <command>`: the same here, with zsh, bash, dash, ksh and
    (they take no `-lic`: their `-c` it is) tcsh and csh."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for tool in (BUILTIN["claude"], BUILTIN["freya"]):
        (bin_dir / tool.binary).symlink_to(FAKEBIN / "claude")  # records what it is given

    def check(case: tuple[int, str, str, str]) -> None:
        n, shell, tool_id, text = case
        tool = BUILTIN[tool_id]
        home, out = tmp_path / f"home{n}", tmp_path / f"out{n}"
        home.mkdir()
        out.mkdir()
        env = {"HOME": str(home), "SHELL": shell, "OUT": str(out)}
        line = login_shell_command(command(tool, text, session="s-0", tier="autonomous"), "abc")
        done = subprocess.run(
            [shell, "-c", line],
            env={**env, "PATH": f"{bin_dir}:/usr/bin:/bin"},
            cwd=home,
            capture_output=True,
            timeout=30,
        )
        assert b"LAPIS-END-abc 0" in done.stdout, done
        # The start marker names the shell that runs the script: what a stop goes by.
        assert re.search(rb"^LAPIS-BEGIN-abc [1-9][0-9]*$", done.stdout, re.M), done
        args = (out / "args").read_bytes().split(b"\0")[:-1]
        if tool.prompt:  # one argument, guarded against being read as an option
            guarded = (" " + text if text.startswith("-") else text).encode()
            assert args == [b"agent", b"--yolo", guarded], (shell, text)
            assert (out / "stdin").read_bytes() == b""
        else:
            assert (out / "stdin").read_bytes() == text.encode(), (shell, text)
            flags = "-p --output-format json --permission-mode bypassPermissions --resume s-0"
            assert args == flags.encode().split()
        assert not list(home.rglob("PWNED"))

    cases = [(s, t, m) for s in SHELLS for t in ("claude", "freya") for m in HOSTILE]
    # Each case is three short-lived shells; side by side they take well under a second.
    with ThreadPoolExecutor(8) as pool:
        list(pool.map(check, [(n, *case) for n, case in enumerate(cases)]))
    assert not list(tmp_path.rglob("PWNED"))


def test_each_tool_gets_its_own_flags_for_tier_and_session() -> None:
    def tail(tool_id: str, **kw: str) -> str:
        run = command(BUILTIN[tool_id], "do it", **kw).split("; then ", 1)[1].rsplit("; else", 1)[0]
        return run.rsplit("; fi; ", 1)[-1]  # past Claude Code's token file (see below)

    assert tail("claude", tier="strict") == (
        'printf %s "$p" | "$t" -p --output-format json --permission-mode plan'
    )
    assert tail("claude", tier="balanced", session="0a1b-2c").endswith(
        "--permission-mode acceptEdits --resume 0a1b-2c"
    )
    assert tail("claude", tier="autonomous").endswith("--permission-mode bypassPermissions")
    assert tail("opencode", tier="autonomous", session="ses_9") == (
        'printf %s "$p" | "$t" run --format json --dangerously-skip-permissions -s ses_9'
    )
    assert tail("freya", tier="balanced", session="ignored") == '"$t" agent "$p"'
    assert tail("freya", tier="autonomous") == '"$t" agent --yolo "$p"'
    assert tail("codex", tier="autonomous") == '"$t" exec "$p"'
    assert tail("gemini", tier="strict") == '"$t" -o json --approval-mode plan -p "$p"'
    assert tail("gemini", tier="balanced") == '"$t" -o json --approval-mode auto_edit -p "$p"'
    assert tail("gemini", tier="autonomous") == '"$t" -o json --approval-mode yolo -p "$p"'
    assert tail("cursor", tier="strict") == (
        '"$t" -p --output-format text --trust --mode plan "$p"'
    )
    assert tail("cursor", tier="autonomous").endswith('--trust --force "$p"')
    # A session id that is not a plain token is not put on a command line at all, and
    # neither is one that the tool would read as an option of its own.
    for odd in ("x; touch PWNED", "--dangerously-skip-permissions", "-c", ""):
        assert "--resume" not in tail("claude", tier="strict", session=odd), odd
        assert harness._session(odd) == ""
    assert harness._session("0a1b-2c.d:e_f") == "0a1b-2c.d:e_f"
    # The program is looked up by name, then in the places its installer uses.
    assert "t=$(w claude .local/bin/claude .claude/local/claude)" in command(
        BUILTIN["claude"], "", tier="strict"
    )
    assert set(BUILTIN) == {"claude", "opencode", "freya", "codex", "gemini", "cursor"}


def test_a_tool_is_never_given_a_command_line_for_a_tier_it_cannot_be_held_to() -> None:
    # Which tool can be held to which of the host's policy tiers, by flags of its own.
    assert {i: [t for t in TIERS if t in tool.tiers] for i, tool in BUILTIN.items()} == {
        "claude": ["strict", "balanced", "autonomous"],
        "opencode": ["autonomous"],
        "freya": ["balanced", "autonomous"],
        "codex": ["autonomous"],
        "gemini": ["strict", "balanced", "autonomous"],
        "cursor": ["strict", "autonomous"],
    }
    aider = custom("aider", "Aider", "aider --yes --message {prompt}")
    vouched = custom("safe", "Safe", "safe --read-only {prompt}", ["strict", "balanced"])
    assert list(aider.tiers) == ["autonomous"] and list(vouched.tiers) == ["strict", "balanced"]
    for tool in (*BUILTIN.values(), aider, vouched):
        for tier in (*TIERS, "", "yolo"):
            if tier in tool.tiers:
                assert command(tool, "do it", tier=tier)
            else:  # no flags of its own for it: not "the same line as for autonomous"
                with pytest.raises(ValueError, match="cannot be held to"):
                    command(tool, "do it", tier=tier)

    # What the owner is told instead: under what it can run, and which tools keep to this.
    tools = harness.all_tools({})
    assert harness.refusal(BUILTIN["opencode"], "balanced", "macbook-pro", tools) == (
        "opencode was not started: it cannot be held to the balanced policy that applies to "
        "this chat on macbook-pro. It can be run under autonomous: set that as this chat's "
        "policy to allow it, or pick a tool that keeps to balanced: Claude Code, Freya, "
        "Gemini CLI."
    )
    said = harness.refusal(BUILTIN["cursor"], "balanced", "mac", {"cursor": BUILTIN["cursor"]})
    assert said.endswith("It can be run under strict or autonomous: set that as this chat's "
                         "policy to allow it.")  # fmt: skip


def test_reading_what_claude_code_printed() -> None:
    line = json.dumps({"type": "result", "is_error": False, "result": " Done. ", "session_id": "u"})
    assert harness.read_claude(f"warning: slow\n{line}\n") == Answer("Done.", "u")
    # The verbose form is a list of messages; the result is the last of its kind.
    listed = json.dumps([{"type": "system"}, {"type": "result", "result": "A", "session_id": "v"}])
    assert harness.read_claude(listed) == Answer("A", "v")
    failed = json.dumps({"type": "result", "subtype": "error_max_turns", "is_error": True})
    assert harness.read_claude(failed) == Answer("error_max_turns", "", True)
    # A session id that is not a plain token is dropped rather than stored.
    odd = json.dumps({"result": "x", "session_id": "a b;c"})
    assert harness.read_claude(odd).session == ""
    # No result object: its words are the answer; a deep or cut JSON line is just text.
    plain = harness.read_claude("\x1b[31mNot logged in\x1b[0m\r\n")
    assert plain == Answer("Not logged in", raw=True)  # its words, not a result
    assert harness.read_claude('{"result": "cut he').text == '{"result": "cut he'
    assert harness.read_claude("[" * 100_000).text.startswith("[[[")
    gone = harness.read_claude("No conversation found with session ID: 0a1b\n")
    assert gone.lost_session and not harness.read_claude(line).lost_session


def test_reading_what_gemini_cli_printed() -> None:
    # As Gemini CLI 0.71 printed it under the device's TERM=dumb, through a terminal.
    warned = (
        "Warning: Basic terminal detected (TERM=dumb). Visual rendering will be limited.\r\n"
        "Warning: 256-color support not detected.\r\n"
        "Ripgrep is not available. Falling back to GrepTool.\r\n"
    )
    shown = json.dumps(
        {"session_id": "fe9d-c640", "response": " ok ", "stats": {"models": {}}}, indent=2
    )
    assert harness.read_gemini(warned + shown.replace("\n", "\r\n")) == Answer("ok", "fe9d-c640")
    failed = json.dumps(
        {"error": {"type": "Error", "message": "Please set an Auth method", "code": 41}}
    )
    assert harness.read_gemini(warned + failed) == Answer("Please set an Auth method", "", True)
    # No answer object (an older version without -o json, or cut): what it printed.
    assert harness.read_gemini("Unknown argument: o\n") == Answer("Unknown argument: o", raw=True)
    assert harness.read_gemini(warned + '{\n  "response": "cut by the 64').raw
    assert BUILTIN["gemini"].show(warned.splitlines()[0]) == ""  # no warning as a ticker line


def test_claude_code_gets_the_token_file_and_says_how_to_make_one() -> None:
    script = command(BUILTIN["claude"], "hi", tier="strict")
    assert (
        'if [ -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] && [ -r "$HOME/".claude/lapis-token ]' in script
    )
    other = command(BUILTIN["gemini"], "hi", tier="strict")
    assert "lapis-token" not in other and "OAUTH" not in other
    run = SimpleNamespace(
        started=True, timed_out=False, cut=False, left="", exit_status=1,
        output="Not logged in \xb7 Please run /login\r\n",
    )  # fmt: skip
    said = answer(BUILTIN["claude"], run, "mac", 60)
    assert said.is_error and said.text.startswith(
        "Claude Code failed (exit status 1). Its last output:\nNot logged in · Please run /login"
    )
    assert "run `claude setup-token`" in said.text and "~/.claude/lapis-token" in said.text
    assert "setup-token" not in answer(BUILTIN["gemini"], run, "mac", 60).text


def test_reading_what_opencode_printed() -> None:
    def event(kind: str, **rest: object) -> str:
        return json.dumps({"type": kind, "timestamp": 1, "sessionID": "ses_42", **rest})

    stream = "\n".join(
        [
            "noise before",
            event("step_start", part={"type": "step-start"}),
            event(
                "tool_use", part={"tool": "bash", "state": {"status": "completed", "title": "ls"}}
            ),
            event("text", part={"text": " First part. ", "time": {"end": 2}}),
            '{"type": "text", "sessionID": "ses_42", "part": {"text": "cut by the 64 KB li',
            event("text", part={"text": "Second part."}),
            event("step_finish", part={"type": "step-finish"}),
        ]
    )
    assert harness.read_opencode(stream) == Answer("First part.\n\nSecond part.", "ses_42")
    broken = stream + "\n" + event("error", error={"name": "APIError", "data": {"message": "401"}})
    assert harness.read_opencode(broken) == Answer("401", "ses_42", True)
    named = event("error", error={"name": "ProviderAuthError"})
    assert harness.read_opencode(named).text == "ProviderAuthError"
    # Not the event stream at all: what it printed is shown as it is.
    assert harness.read_opencode("Error: no providers configured\n") == Answer(
        "Error: no providers configured", raw=True
    )
    # Progress lines for the ticker: the tool in use, or the start of what is said.
    lines = stream.splitlines()
    assert [harness.show_opencode(x) for x in lines[:4]] == ["", "", "bash ls", "First part."]
    assert BUILTIN["claude"].show(lines[2]) == ""  # Claude Code prints its answer only
    assert BUILTIN["freya"].show("\x1b[2mstep 3\x1b[0m reading files\r") == "step 3 reading files"


def test_what_the_chat_is_told_about_a_run() -> None:
    def run(output: str = "", status: int | None = 0, **kw: Any) -> SimpleNamespace:
        base = {"started": True, "timed_out": False, "cut": False, "left": harness.LEFT_NOTHING}
        return SimpleNamespace(output=output, exit_status=status, **{**base, **kw})

    freya, claude = BUILTIN["freya"], BUILTIN["claude"]
    assert answer(freya, run("All done.\n"), "mac", 60) == Answer("All done.")
    assert answer(freya, run(""), "mac", 60).text == "Freya finished without printing an answer."
    long = answer(freya, run("x" * 50_000 + "THE END"), "mac", 60)
    assert len(long.text) == harness.ANSWER_MAX and long.text.endswith("THE END")

    missing = answer(claude, run("", 127), "mac", 60)
    assert missing.is_error and missing.text == (
        "Claude Code is not installed on mac: no `claude` on the PATH of the login shell there."
    )
    failed = answer(freya, run('lumi: no API key for provider "x"\n', 1), "mac", 60)
    assert failed.is_error and failed.text == (
        'Freya failed (exit status 1). Its last output:\nlumi: no API key for provider "x"'
    )
    said = json.dumps({"is_error": True, "result": "Credit balance is too low", "session_id": "k"})
    errored = answer(claude, run(said), "mac", 60)
    assert errored == Answer(
        "Claude Code reported an error. Its last output:\nCredit balance is too low", "k", True
    )
    slow = answer(freya, run("step 1\nstep 2\n", None, timed_out=True), "mac", 1800)
    assert slow.is_error and slow.text == (
        "Freya was stopped after 30 minutes without finishing. Its last output:\nstep 1\nstep 2"
    )
    assert "after 90 seconds without" in answer(freya, run("", None, timed_out=True), "m", 90).text
    assert (
        "after 240 minutes without" in answer(freya, run("", None, timed_out=True), "m", 14400).text
    )
    dropped = answer(freya, run("half", None), "mac", 60)
    assert dropped.is_error and dropped.text == (
        "The connection to mac closed before Freya finished. Its last output:\nhalf"
    )
    # Stopped is only said of what the host confirmed (the time limit, a lost connection).
    late = {"status": None, "timed_out": True}
    unknown = answer(freya, run("step 1\n", left=harness.LEFT_UNKNOWN, **late), "mac", 1800)
    assert unknown.is_error and unknown.text == (
        "Freya did not finish within 30 minutes, and mac did not confirm that it was "
        "stopped: it may still be running there. Its last output:\nstep 1"
    )
    partly = answer(freya, run("", left=harness.LEFT_STARTED, **late), "mac", 90)
    assert partly.is_error and partly.text == (
        "Freya was stopped after 90 seconds without finishing, but the programs it had "
        "started on mac could not be checked: some may still be running there."
    )
    gone = answer(freya, run("half", None, left=harness.LEFT_UNKNOWN), "mac", 60)
    assert gone.text == (
        "The connection to mac closed before Freya finished: it may still be running "
        "there. Its last output:\nhalf"
    )
    unstarted = answer(freya, run("", None, started=False), "mac", 60)
    assert unstarted.is_error and "login shell" in unstarted.text
    # Only the end of a long failure is quoted.
    assert len(answer(freya, run("e" * 9000, 2), "mac", 60).text) < harness.TAIL_MAX + 100

    # Claude Code's answer is one line of JSON. Longer than the buffer, only its end is
    # left: no `is_error`, no session, and a piece of JSON is not an answer either.
    whole = json.dumps({"is_error": True, "result": "word " * 20_000, "session_id": "sess-9"})
    assert len(whole) > OUTPUT_KEEP and f"{OUTPUT_KEEP // 1024} KB" == harness.KEPT
    cut = answer(claude, run(whole[-OUTPUT_KEEP:], cut=True), "mac", 60)
    assert cut.is_error and cut.session == "" and cut.text.startswith(
        "Claude Code printed more than the 64 KB this device keeps of one run, so its "
        "answer could not be read and whether it succeeded is not known. Its last output:\n"
    )  # fmt: skip
    assert len(cut.text) < harness.TAIL_MAX + 200
    # Plain text that was cut is still an answer (its end), and so is JSON that fits.
    assert answer(freya, run("x" * 9 + "THE END", cut=True), "mac", 60).text.endswith("THE END")
    fits = json.dumps({"result": "fine", "session_id": "s1"})
    assert answer(claude, run("noise\n" + fits, cut=True), "mac", 60) == Answer("fine", "s1")


def test_the_collector_keeps_what_is_between_the_markers() -> None:
    begin, end = b"\nLAPIS-BEGIN-ab\r\n", b"\r\nLAPIS-END-ab 3\r\n"
    noise = b"Last login\r\nLAPIS-BEGIN-zz\r\nLAPIS-END-ab 9 (echoed command line)"
    stream = noise + begin + b"one\r\ntwo \xe2\x9c\x93\r\n" + end + b"trailing"
    for size in (1, 2, 7, len(stream)):  # however the bytes are cut into packets
        c = _Collector("ab")
        for i in range(0, len(stream), size):
            c.feed(stream[i : i + size])
        got = c.result(False)
        assert (got.output, got.exit_status, got.started, got.cut) == (
            "one\ntwo ✓\n\n",
            3,
            True,
            False,
        )
        assert c.last_line == b"two \xe2\x9c\x93" and c.root == 0

    # The start marker carries the process id of the shell that runs the script; only a
    # plain number above 1 is taken for one (it is put on a command line to stop the run).
    for said, root in ((b" 4242", 4242), (b" 1", 0), (b" 0", 0), (b" 12; rm x", 0), (b" -9", 0)):
        c = _Collector("ab")
        c.feed(b"LAPIS-BEGIN-ab" + said + b"\r\nhello\r\n")
        assert (c.started, c.root) == (True, root), said

    # Never started: start-up noise is not output. No end marker: no exit status.
    c = _Collector("ab")
    c.feed(noise)
    assert (c.result(False).output, c.result(False).started) == ("", False)
    c = _Collector("ab")
    c.feed(begin + b"partial line without its newline")
    got = c.result(True)
    assert (got.output, got.exit_status, got.timed_out) == (
        "partial line without its newline", None, True,
    )  # fmt: skip

    # RAM: the newest 64 KB, whether it comes as many lines or as one endless one.
    for chunk in (b"0123456789abcde\n" * 4096, b"z" * 65536):
        c = _Collector("ab")
        c.feed(begin)
        for _ in range(5):
            c.feed(chunk)
            assert len(c.buf) <= OUTPUT_KEEP and len(c.carry) <= OUTPUT_KEEP
        c.feed(b"\nlast words" + end)
        got = c.result(False)
        assert got.cut and got.exit_status == 3 and len(got.output) <= OUTPUT_KEEP
        assert got.output.endswith("last words\n")


def test_other_tools_come_from_the_config_file(tmp_path: Path) -> None:
    path = tmp_path / "lapis.toml"
    path.write_text(
        "[kvm]\nagent_run_timeout_sec = 600\n"
        '[agents.aider]\nname = "Aider"\ncommand = "aider --yes --message {prompt} \'two words\'"\n'
    )
    config = load_config(path)
    assert config.kvm.agent_run_timeout_sec == 600
    assert CoreConfig().kvm.agent_run_timeout_sec == 1800 and CoreConfig().agents == {}
    tools = harness.all_tools(config.agents)
    assert list(tools) == [*BUILTIN, "aider"] and tools["aider"].name == "Aider"
    script = command(tools["aider"], "fix it", tier="autonomous", session="s1")
    # It runs like the built-in ones: found on the PATH, the message one argument.
    assert "if t=$(w aider); then" in script
    assert script.endswith(
        """"$t" --yes --message "$p" "$(printf 'two words')"; else (exit 127); fi"""
    )
    assert custom("x", "X", "'/opt/my tools/run' {prompt}").binary == "/opt/my tools/run"
    # Known only by its command line, it runs under the autonomous policy alone, unless
    # the entry lists the tiers the owner vouches for (kept the tightest first).
    assert config.agents["aider"].tiers == ["autonomous"] and list(tools["aider"].tiers) == [
        "autonomous"
    ]
    path.write_text(
        '[agents.reader]\nname = "Reader"\ncommand = "reader --read-only {prompt}"\n'
        'tiers = ["balanced", "strict"]\n'
    )
    reader = harness.all_tools(load_config(path).agents)["reader"]
    assert reader.tiers == {"strict": (), "balanced": ()}
    assert command(reader, "x", tier="strict").endswith(
        '"$t" --read-only "$p"; else (exit 127); fi'
    )

    def refused(text: str) -> str:
        path.write_text(text)
        with pytest.raises(ValueError) as err:
            load_config(path)
        return str(err.value)

    bad_commands = [
        "aider --message",  # no {prompt}
        "{prompt}",  # no program
        "aider {prompt} {prompt}",  # twice
        "aider --message={prompt}",  # not a word of its own
        "aider '{prompt} please'",
        "aider {prompt} 'unbalanced",
        "aider {prompt}\\nrm -rf x",  # TOML newline: a second line
        "x" * 501 + " {prompt}",
        "",
    ]
    for bad in bad_commands:
        said = refused(f'[agents.aider]\nname = "Aider"\ncommand = "{bad}"\n')
        assert "agents.aider.command" in said and "input_value" not in said, bad
    for bad_id in ("claude", "lapis", "Aider", "9lives", "a b", "x" * 33):
        said = refused(f'[agents."{bad_id}"]\nname = "A"\ncommand = "a {{prompt}}"\n')
        assert "agent id" in said, bad_id
    assert "agents.a.name" in refused('[agents.a]\nname = " "\ncommand = "a {prompt}"\n')
    for bad_tiers in ("[]", '["yolo"]', '"strict"'):
        said = refused(f'[agents.a]\nname = "A"\ncommand = "a {{prompt}}"\ntiers = {bad_tiers}\n')
        assert "agents.a.tiers" in said, bad_tiers
    for seconds in (5, 4 * 3600 + 1):  # the hard limits of one run: 10 s .. 4 h
        assert "agent_run_timeout_sec" in refused(f"[kvm]\nagent_run_timeout_sec = {seconds}\n")


def test_detection_lists_only_tools_that_are_there(tmp_path: Path) -> None:
    home, bin_dir = tmp_path / "home", tmp_path / "bin"
    for d in (bin_dir, home / ".opencode" / "bin"):
        d.mkdir(parents=True)
    for path in (bin_dir / "claude", home / ".opencode" / "bin" / "opencode", bin_dir / "codex"):
        path.write_text("#!/bin/sh\n")
        path.chmod(0o755)
    (bin_dir / "codex").chmod(0o644)  # there, but not a program
    mine = SimpleNamespace(name="Mine", command="claude {prompt}", tiers=["autonomous"])
    tools = harness.all_tools({"mine": mine})
    done = subprocess.run(
        ["/bin/sh", "-c", harness.detect_command(tools)],
        env={"HOME": str(home), "PATH": f"{bin_dir}:/usr/bin:/bin"},
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert done.returncode == 0 and done.stderr == ""
    assert harness.detected(tools, "motd\n" + done.stdout + "codex not/absolute\nnope /x\n") == [
        {"id": "claude", "name": "Claude Code", "path": f"{bin_dir}/claude"},
        {"id": "opencode", "name": "opencode", "path": f"{home}/.opencode/bin/opencode"},
        {"id": "mine", "name": "Mine", "path": f"{bin_dir}/claude"},
    ]
    assert shutil.which("sh")  # (the test needs nothing else from the machine)
