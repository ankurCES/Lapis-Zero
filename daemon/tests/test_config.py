from __future__ import annotations

from pathlib import Path

import pytest

from lapis_daemon import config as cfg

EXAMPLES = Path(__file__).resolve().parents[2] / "config"


def test_example_tomls_match_mock_defaults() -> None:
    core = cfg.load_config(EXAMPLES / "lapis.example.toml")
    hosts = cfg.load_hosts(EXAMPLES / "hosts.example.toml")
    assert core == cfg.mock_config()
    assert hosts == cfg.mock_hosts()


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


def test_effective_paths(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.delenv("LAPIS_CONFIG_DIR", raising=False)
    paths = cfg.effective_paths()
    assert (str(paths.config), str(paths.hosts), str(paths.db_path), str(paths.socket_path)) == (
        cfg.DEFAULT_CONFIG,
        cfg.DEFAULT_HOSTS,
        cfg.DEFAULT_DB,
        cfg.DEFAULT_SOCKET,
    )

    monkeypatch.setenv("LAPIS_CONFIG_DIR", str(tmp_path))
    paths = cfg.effective_paths()
    assert paths == cfg.EffectivePaths(
        tmp_path / "lapis.toml",
        tmp_path / "hosts.toml",
        tmp_path / "lapis.db",
        tmp_path / "lapisd.sock",
    )
    assert cfg.effective_paths("/x/custom.toml").config == Path("/x/custom.toml")
