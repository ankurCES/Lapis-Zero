"""Configuration models, TOML loaders, mock defaults, and effective paths (SPEC 7.1)."""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, Field

DEFAULT_CONFIG = "/etc/lapis/lapis.toml"
DEFAULT_HOSTS = "/etc/lapis/hosts.toml"
DEFAULT_SOCKET = "/run/lapis/lapisd.sock"
DEFAULT_DB = "/var/lib/lapis/lapis.db"

PolicyName = Literal["strict", "balanced", "autonomous"]

# Field defaults mirror config/lapis.example.toml and config/hosts.example.toml.


class LLMConfig(BaseModel):
    provider: Literal["anthropic", "openai", "openrouter"] = "anthropic"
    model: str = "claude-sonnet-4-20250514"
    api_key_env: str = "ANTHROPIC_API_KEY"
    max_steps: int = Field(default=25, ge=1)
    request_timeout_sec: int = Field(default=60, ge=1)


class KvmConfig(BaseModel):
    llm_max_width: int = 1280
    llm_max_height: int = 720
    jpeg_quality: int = Field(default=75, ge=1, le=100)
    vnc_idle_disconnect_sec: int = 60
    stuck_threshold: int = 3
    stuck_hamming_max: int = 2
    ssh_timeout_sec: int = 30
    ssh_output_truncate: int = 4096


class VoiceConfig(BaseModel):
    enabled: bool = False
    sample_rate: int = 16000
    channels: int = 1


class UiConfig(BaseModel):
    theme: Literal["paper", "dark"] = "paper"
    frame_fps_active: int = 60
    frame_fps_agent: int = 12
    frame_fps_idle: int = 1


class SecurityConfig(BaseModel):
    approval_default_tier: PolicyName = "balanced"


class CronSchedule(BaseModel):
    name: str
    schedule: str
    host: str
    prompt: str
    enabled: bool = False


class CronConfig(BaseModel):
    enabled: bool = True
    schedules: list[CronSchedule] = []


class CoreConfig(BaseModel):
    llm: LLMConfig = LLMConfig()
    kvm: KvmConfig = KvmConfig()
    voice: VoiceConfig = VoiceConfig()
    ui: UiConfig = UiConfig()
    security: SecurityConfig = SecurityConfig()
    cron: CronConfig = CronConfig()


class HostVnc(BaseModel):
    port: int = 5900
    username: str = ""
    password_env: str = ""


class HostSsh(BaseModel):
    port: int = 22
    user: str = ""
    identity_file: str | None = None


class HostConfig(BaseModel):
    hostname: str
    tailscale_ip: str
    policy: PolicyName = "balanced"
    vnc: HostVnc = HostVnc()
    ssh: HostSsh = HostSsh()


class HostsConfig(BaseModel):
    hosts: dict[str, HostConfig] = {}


def _read_toml(path: str | Path) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


def load_config(path: str | Path) -> CoreConfig:
    """Load and validate lapis.toml."""
    return CoreConfig.model_validate(_read_toml(path))


def load_hosts(path: str | Path) -> HostsConfig:
    """Load and validate hosts.toml."""
    return HostsConfig.model_validate(_read_toml(path))


def mock_config() -> CoreConfig:
    """Core config equal to config/lapis.example.toml."""
    return CoreConfig(
        cron=CronConfig(
            enabled=True,
            schedules=[
                CronSchedule(
                    name="Daily Digest",
                    schedule="0 8 * * *",
                    host="macbook-pro",
                    prompt="Check overnight CI runs and summarize any failures.",
                    enabled=False,
                )
            ],
        )
    )


def mock_hosts() -> HostsConfig:
    """Hosts config equal to config/hosts.example.toml."""
    return HostsConfig(
        hosts={
            "macbook-pro": HostConfig(
                hostname="macbook-pro",
                tailscale_ip="100.64.10.21",
                policy="balanced",
                vnc=HostVnc(username="alice", password_env="LAPIS_VNC_PASSWORD_macbook_pro"),
                ssh=HostSsh(user="alice"),
            ),
            "arch-dev": HostConfig(
                hostname="arch-dev",
                tailscale_ip="100.64.20.42",
                policy="strict",
                vnc=HostVnc(username="dev", password_env="LAPIS_VNC_PASSWORD_arch_dev"),
                ssh=HostSsh(user="dev", identity_file="/etc/lapis/lapis_ed25519"),
            ),
        }
    )


@dataclass(frozen=True)
class EffectivePaths:
    config: Path
    hosts: Path
    db_path: Path
    socket_path: Path


def effective_paths(config: str | None = None) -> EffectivePaths:
    """Resolve paths; LAPIS_CONFIG_DIR relocates everything for rootless dev and tests."""
    base = os.environ.get("LAPIS_CONFIG_DIR")
    if base:
        d = Path(base)
        paths = EffectivePaths(
            d / "lapis.toml", d / "hosts.toml", d / "lapis.db", d / "lapisd.sock"
        )
    else:
        paths = EffectivePaths(
            Path(DEFAULT_CONFIG), Path(DEFAULT_HOSTS), Path(DEFAULT_DB), Path(DEFAULT_SOCKET)
        )
    if config:  # explicit --config wins over both defaults
        paths = EffectivePaths(Path(config), paths.hosts, paths.db_path, paths.socket_path)
    return paths
