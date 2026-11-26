from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import tomli_w

from lapis_harness import config as cfg
from lapis_harness import main as harness_main

EXAMPLES = Path(__file__).resolve().parents[1] / "config"


def test_example_tomls_match_mock_defaults() -> None:
    core = cfg.load_config(EXAMPLES / "lapis.example.toml")
    hosts = cfg.load_hosts(EXAMPLES / "hosts.example.toml")
    assert core == cfg.mock_config()
    assert hosts == cfg.mock_hosts()


def test_the_examples_show_the_settings_of_tool_chats_and_what_they_show_loads(
    tmp_path: Path,
) -> None:
    """They are commented out (the examples equal the defaults); uncommented, they must
    be settings the daemon takes."""
    text = (EXAMPLES / "lapis.example.toml").read_text()
    assert "\nagent_run_timeout_sec = 1800\n" in text
    shown = text[text.index("# [agents.aider]") : text.index("[cron]")]
    path = tmp_path / "lapis.toml"
    path.write_text("".join(line.removeprefix("# ") + "\n" for line in shown.splitlines()))
    aider = cfg.load_config(path).agents["aider"]
    assert (aider.name, aider.command, aider.tiers) == (
        "Aider",
        "aider --yes --message {prompt}",
        ["autonomous"],
    )
    hosts = (EXAMPLES / "hosts.example.toml").read_text()
    assert hosts.count("# password_env = ") == 2
    path = tmp_path / "hosts.toml"
    path.write_text(hosts.replace("# password_env = ", "password_env = "))
    loaded = cfg.load_hosts(path).hosts
    assert loaded["macbook-pro"].ssh.password_env == loaded["macbook-pro"].vnc.password_env
    assert loaded["arch-dev"].ssh.password_env == "LAPIS_SSH_PASSWORD_arch_dev"


def test_mock_defaults() -> None:
    core = cfg.mock_config()
    assert core.llm.max_steps == 25
    assert [(s.name, s.enabled) for s in core.cron.schedules] == [("Daily Digest", False)]
    hosts = cfg.mock_hosts().hosts
    assert {name: h.policy for name, h in hosts.items()} == {
        "macbook-pro": "balanced",
        "arch-dev": "strict",
    }


def test_invalid_policy_rejected(tmp_path: Path) -> None:
    bad = tmp_path / "hosts.toml"
    bad.write_text('[hosts.x]\nhostname = "x"\ntailscale_ip = "100.64.0.1"\npolicy = "yolo"\n')
    with pytest.raises(ValueError):
        cfg.load_hosts(bad)


def test_effective_paths_and_the_cli_defaults(tmp_path: Path) -> None:
    paths = cfg.effective_paths(tmp_path / "etc", tmp_path / "state")
    assert paths == cfg.EffectivePaths(
        tmp_path / "etc" / "lapis.toml",
        tmp_path / "etc" / "hosts.toml",
        tmp_path / "state" / "lapis.db",
        tmp_path / "etc" / "secrets.env",
        tmp_path / "state" / "frames",
    )
    assert paths.ssh_key == tmp_path / "state" / "ssh" / "id_ed25519"

    parse = harness_main.build_parser().parse_args
    paths, sock, token = harness_main.resolve(parse([]))
    assert (paths.config, paths.db_path) == (
        Path("/etc/lapis/lapis.toml"),
        Path("/var/lib/lapis/harness/lapis.db"),
    )
    assert (sock, token) == (Path("/run/lapis/harness.sock"), Path("/var/lib/lapis/local.token"))
    # --mock: everything under the temp dir the daemon's --mock-hw uses too.
    paths, sock, token = harness_main.resolve(parse(["--mock"]))
    mock = cfg.mock_dir()
    assert (sock, token, paths.db_path) == (
        mock / "harness.sock",
        mock / "local.token",
        mock / "harness" / "lapis.db",
    )
    custom = ["--state-dir", str(tmp_path), "--config", str(tmp_path / "c"), "--socket", "/x/s"]
    paths, sock, _ = harness_main.resolve(parse(["--mock", *custom]))
    assert (paths.db_path, paths.hosts, sock) == (
        tmp_path / "lapis.db",
        tmp_path / "c" / "hosts.toml",
        Path("/x/s"),
    )


def test_the_mock_dir_is_the_one_rust_computes() -> None:
    """Rust's `std::env::temp_dir()` is $TMPDIR, else /tmp: so is `tempfile.gettempdir()`."""
    import os
    import tempfile

    rust = Path(os.environ.get("TMPDIR") or "/tmp")
    assert Path(tempfile.gettempdir()) == rust
    assert cfg.mock_dir() == rust / "lapis-deck-mock"


def test_missing_files_mean_defaults_and_toml_round_trips(tmp_path: Path) -> None:
    assert cfg.load_config(tmp_path / "nope.toml") == cfg.CoreConfig()
    assert cfg.load_hosts(tmp_path / "nope.toml").hosts == {}
    assert cfg.CoreConfig().llm.model == "claude-sonnet-5-5"
    cfg.save_toml(tmp_path / "hosts.toml", cfg.mock_hosts())
    assert cfg.load_hosts(tmp_path / "hosts.toml") == cfg.mock_hosts()
    cfg.save_toml(tmp_path / "lapis.toml", cfg.mock_config())
    assert cfg.load_config(tmp_path / "lapis.toml") == cfg.mock_config()
    assert not list(tmp_path.glob(".*"))  # no temp files left behind


def test_secrets_file_is_0600_and_masked(tmp_path: Path) -> None:
    path = tmp_path / "secrets.env"
    cfg.write_env_file(path, {"ANTHROPIC_API_KEY": "sk-ant-abcdefgh1234", "B": "two words"})
    assert path.stat().st_mode & 0o777 == 0o600
    path.write_text(path.read_text() + '# comment\nQUOTED="v"\nnot a pair\n1BAD=x\n')
    assert cfg.read_env_file(path) == {
        "ANTHROPIC_API_KEY": "sk-ant-abcdefgh1234",
        "B": "two words",
        "QUOTED": "v",
    }
    assert cfg.mask_secret("sk-ant-abcdefgh1234") == "...1234"
    assert cfg.mask_secret("short") == "..."  # never most of a short secret
    for bad in ({"has space": "x"}, {"OK": "two\nlines"}):
        with pytest.raises(ValueError):
            cfg.write_env_file(path, bad)


# --- a lapis.toml written before the rules for llm.api_key_env / llm.base_url ------


def stored_by_an_older_daemon(path: Path, **llm: str) -> dict[str, Any]:
    """Write a file with the owner's settings all over it, plus `llm`; returns its content."""
    stored = cfg.mock_config().model_dump()
    stored["api"]["port"] = 9000
    stored["ui"]["theme"] = "dark"
    stored["kvm"]["jpeg_quality"] = 60
    stored["llm"] |= {"provider": "openai", "model": "MiniMax-M2", "max_steps": 60} | llm
    stored["cron"]["schedules"][0]["enabled"] = True
    path.write_bytes(tomli_w.dumps(stored).encode())
    return stored


@pytest.mark.parametrize(
    "llm",
    [
        {"api_key_env": "sk-test-pasted-by-mistake-9999"},
        {"api_key_env": "gsk_" + "aB3dE5gH" * 6 + "wxyz"},  # a valid name by its characters
        {"api_key_env": ""},
        {"api_key_env": "MINIMAX-API-KEY"},
        {"base_url": "https://example.invalid/v1?api_key=sk-test-embedded"},
        {"base_url": "api.minimax.io/v1"},
        {
            "api_key_env": "sk-test-pasted-by-mistake-9999",
            "base_url": "https://user:sk-test-embedded@example.invalid/v1",
        },
    ],
)
def test_a_stored_file_keeps_everything_but_an_llm_setting_refused_today(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    llm: dict[str, str],
) -> None:
    path = tmp_path / "lapis.toml"
    stored = stored_by_an_older_daemon(path, **llm)
    loaded = cfg.load_config(path)
    # Only the refused setting is back at its default; the rest is the owner's.
    for key in llm:
        assert getattr(loaded.llm, key) == getattr(cfg.LLMConfig(), key)
    kept = loaded.model_dump()
    assert {k: v for k, v in kept.items() if k != "llm"} == {
        k: v for k, v in stored.items() if k != "llm"
    }
    assert kept["llm"] | llm == stored["llm"]
    assert (loaded.api.port, loaded.llm.model, loaded.cron.schedules[0].enabled) == (
        9000,
        "MiniMax-M2",
        True,
    )
    # It says which setting and why, never the value (which may be a key).
    printed = capsys.readouterr().err
    for key, value in llm.items():
        assert f"ignoring llm.{key}: " in printed
        assert not value or value not in printed
    assert "sk-test" not in printed and "gsk_" not in printed

    # The harness's own start-up gets the same.
    loader = harness_main._load
    assert loader(path, cfg.load_config, cfg.mock_config, cfg.CoreConfig, False) == loaded
    # Where the setting is entered (PUT /v1/config validates the model) it is still refused.
    with pytest.raises(ValueError):
        cfg.CoreConfig.model_validate(stored)


def test_a_stored_file_broken_elsewhere_is_still_refused_whole(tmp_path: Path) -> None:
    path = tmp_path / "lapis.toml"
    stored = stored_by_an_older_daemon(path, api_key_env="sk-test-pasted-by-mistake-9999")
    stored["api"]["port"] = 80  # not a setting any daemon accepted
    path.write_bytes(tomli_w.dumps(stored).encode())
    with pytest.raises(ValueError) as err:
        cfg.load_config(path)
    assert "api.port" in str(err.value) and "sk-test" not in str(err.value)
    path.write_text('[llm]\napi_key_env = "MINIMAX_API_KEY"\neffort = "extreme"\n')
    with pytest.raises(ValueError, match="llm.effort"):
        cfg.load_config(path)
