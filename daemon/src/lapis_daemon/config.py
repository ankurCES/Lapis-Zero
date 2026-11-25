"""Configuration models, TOML loaders, mock defaults, and effective paths (SPEC 7.1)."""

from __future__ import annotations

import os
import re
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from itertools import pairwise
from pathlib import Path
from typing import Literal
from urllib.parse import urlsplit

import tomli_w
from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator

DEFAULT_CONFIG = "/etc/lapis/lapis.toml"
DEFAULT_HOSTS = "/etc/lapis/hosts.toml"
DEFAULT_SECRETS = "/etc/lapis/secrets.env"
DEFAULT_SOCKET = "/run/lapis/lapisd.sock"
DEFAULT_DB = "/var/lib/lapis/lapis.db"
DEFAULT_FRAMES = "/var/lib/lapis/frames"

# The control plane is loopback-only; `tailscale serve` is the sole way out
# (ANDROID_COMPANION_SPEC 1.2). Not configurable from lapis.toml on purpose.
API_BIND_HOST = "127.0.0.1"
API_DEFAULT_PORT = 8421

PolicyName = Literal["strict", "balanced", "autonomous"]

SECRET_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$")
# A secret is exported into the daemon's own environment under its name, so a
# secret called PATH or LD_PRELOAD would replace what the process itself runs on.
RESERVED_NAMES = {"PATH", "HOME", "USER", "SHELL", "LANG", "TZ", "LAPIS_CONFIG_DIR"}
RESERVED_PREFIXES = ("LD_", "PYTHON", "DYLD_")
# No whitespace, credentials (@), query or fragment; host and port: see below.
_BASE_URL_RE = re.compile(r"https?://[^\s\x00-\x1f\x7f@?#]+", re.IGNORECASE)


def is_reserved_name(name: str) -> bool:
    return name.upper() in RESERVED_NAMES or name.upper().startswith(RESERVED_PREFIXES)


_KEY_PREFIXES = ("sk-", "gsk_", "aiza", "eyj", "tskey-", "tskey_auth")  # compared casefolded
_NAME_SEPARATORS = re.compile(r"[_\-. ]")


def _alternations(text: str) -> int:
    """How often `text` switches between digits and other characters: `key2025` once."""
    return sum(a.isdecimal() != b.isdecimal() for a, b in pairwise(text))


def looks_like_secret_value(text: str) -> bool:
    """A key or token where its NAME belongs.

    Many key formats are valid names (Groq, Mistral, Gemini, Together ...), and a name
    is readable: lapis.toml, GET /v1/config, the secrets list, error messages. A
    heuristic. What tells a key from a name is digits scattered through it: a name has
    words and perhaps a number at the end (`minimaxapikey2025`, `EASTUS2`), a key
    switches between letters and digits all the time. So: a known key prefix, or one
    run between separators (12 characters or more) that switches three times or more,
    or the whole text with everything but letters and digits removed (20 or more, which
    covers keys written in groups such as a UUID) switching six times or more.

    This is the Android app's rule (ConfigRules.looksLikeSecretValue) and the one the
    device enforces: a change to either belongs in both. tests/test_key_names.py reads
    the app's own lists of keys and names and holds this function to them.
    """
    text = text.strip()
    compact = "".join(c for c in text if c.isalpha() or c.isdecimal())
    return (
        text.casefold().startswith(_KEY_PREFIXES)
        or (len(compact) >= 20 and _alternations(compact) >= 6)
        or any(len(run) >= 12 and _alternations(run) >= 3 for run in _NAME_SEPARATORS.split(text))
    )


def is_key_name(value: str) -> bool:
    """Whether `value` may be stored and shown as the name of the LLM API key."""
    return (
        SECRET_NAME_RE.fullmatch(value) is not None
        and not is_reserved_name(value)
        and not looks_like_secret_value(value)
    )


def _is_plain_http_url(value: str) -> bool:
    try:
        url = urlsplit(value)
        return bool(_BASE_URL_RE.fullmatch(value) and url.hostname and url.port != 0)
    except ValueError:  # a malformed IPv6 literal, or a port that is not one
        return False


# Field defaults mirror config/lapis.example.toml and config/hosts.example.toml.


class LLMConfig(BaseModel):
    provider: Literal["anthropic", "openai", "openrouter"] = "anthropic"
    model: str = "claude-sonnet-5-5"
    api_key_env: str = "ANTHROPIC_API_KEY"
    # Thinking depth / token spend per turn on models that take `effort`.
    effort: Literal["low", "medium", "high", "xhigh", "max"] = "medium"
    # OpenAI-compatible endpoint override (openai / openrouter providers only).
    base_url: str = ""
    max_steps: int = Field(default=25, ge=1, le=200)
    request_timeout_sec: int = Field(default=120, ge=1)

    # Both fields are echoed by GET /v1/config and stored 0644, so neither may
    # carry a key: a key pasted where its name belongs, or credentials in a URL.
    @field_validator("api_key_env")
    @classmethod
    def _secret_name(cls, value: str) -> str:
        value = value.strip()
        if not is_key_name(value):
            raise ValueError(
                "must be the name of a secret stored under Config > Secrets "
                "(letters, digits and _, e.g. MINIMAX_API_KEY), not the key itself"
            )
        return value

    @field_validator("base_url")
    @classmethod
    def _plain_url(cls, value: str) -> str:
        value = value.strip()
        if value and not _is_plain_http_url(value):
            raise ValueError(
                "must be empty or an http(s) URL such as https://api.example.com/v1, "
                "without credentials, ? or #"
            )
        return value


class KvmConfig(BaseModel):
    llm_max_width: int = 1280
    llm_max_height: int = 720
    jpeg_quality: int = Field(default=75, ge=1, le=100)
    vnc_idle_disconnect_sec: int = 60
    stuck_threshold: int = 3
    stuck_hamming_max: int = 2
    ssh_timeout_sec: int = 30
    ssh_output_truncate: int = 4096
    # How long one message may keep an agent tool on a host (Claude Code, opencode, ...)
    # busy before it is stopped. `llm.max_steps` does not apply to those runs.
    agent_run_timeout_sec: int = Field(default=1800, ge=10, le=4 * 3600)


class VoiceConfig(BaseModel):
    enabled: bool = False
    sample_rate: int = 16000
    channels: int = 1
    capture_device: str = "default"
    stt_api_url: str = ""
    stt_model: str = "whisper-1"
    stt_api_key_env: str = "OPENAI_API_KEY"
    stt_timeout_sec: int = 30
    audio_cues: bool = True
    max_capture_bytes: int = 512 * 1024


class UiConfig(BaseModel):
    theme: Literal["paper", "dark"] = "paper"
    brightness: int = Field(default=100, ge=0, le=100)
    mute_audio: bool = False
    # IANA name ("America/Los_Angeles"); empty means whatever the OS is set to.
    timezone: str = ""
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


class ApiConfig(BaseModel):
    enabled: bool = True
    port: int = Field(default=API_DEFAULT_PORT, ge=1024, le=65535)
    # LRU cap for /var/lib/lapis/frames (ANDROID_COMPANION_SPEC 5.1).
    frames_cap_mb: int = Field(default=25, ge=1, le=512)


class AgentTool(BaseModel):
    """`[agents.<id>]`: an agent command-line tool on the hosts besides the built-in ones."""

    name: str
    # One command line: the program and its arguments, with `{prompt}` as one whole word
    # where the owner's message goes. No shell syntax; see agent/harness.py.
    command: str
    # The host policy tiers under which this command line may be run. A tool in a chat
    # is not put up for approval step by step, and this device knows nothing of the
    # tool but this line: so only "autonomous", unless the owner lists more here and
    # with that vouches that the line as written keeps the tool within them.
    tiers: list[PolicyName] = ["autonomous"]

    @field_validator("tiers")
    @classmethod
    def _some_tiers(cls, value: list[str]) -> list[str]:
        if not value:
            raise ValueError("must name at least one of strict, balanced, autonomous")
        return [t for t in ("strict", "balanced", "autonomous") if t in value]

    @field_validator("name")
    @classmethod
    def _shown_name(cls, value: str) -> str:
        value = value.strip()
        if not value or len(value) > 40 or not value.isprintable():
            raise ValueError("must be 1 to 40 printable characters")
        return value

    @field_validator("command")
    @classmethod
    def _command_line(cls, value: str) -> str:
        from lapis_daemon.agent.harness import parse_command

        parse_command(value)  # ValueError names what is wrong
        return value.strip()


class CoreConfig(BaseModel):
    # A rejected value may be a key pasted into the wrong field: validation errors
    # (shown by the app and printed at startup) name the field, never its input.
    model_config = ConfigDict(hide_input_in_errors=True)

    api: ApiConfig = ApiConfig()
    llm: LLMConfig = LLMConfig()
    kvm: KvmConfig = KvmConfig()
    voice: VoiceConfig = VoiceConfig()
    ui: UiConfig = UiConfig()
    security: SecurityConfig = SecurityConfig()
    cron: CronConfig = CronConfig()
    agents: dict[str, AgentTool] = {}

    @field_validator("agents")
    @classmethod
    def _agent_ids(cls, value: dict[str, AgentTool]) -> dict[str, AgentTool]:
        from lapis_daemon.agent.harness import check_custom_id

        for agent_id in value:
            check_custom_id(agent_id)
        return value


def _password_name(value: str) -> str:
    """A host's `password_env`: empty, or the NAME of a secret. Echoed by GET /v1/hosts
    and listed by GET /v1/secrets/status, so it must never be the password itself."""
    value = value.strip()
    # The rule write_env_file applies to a new secret's name.
    named = SECRET_NAME_RE.fullmatch(value) is not None and not is_reserved_name(value)
    if value and not (named and (value.startswith("LAPIS_") or not looks_like_secret_value(value))):
        raise ValueError(
            "must be empty or the name of a secret stored under Config > Secrets "
            "(letters, digits and _), not the password itself"
        )
    return value


class HostVnc(BaseModel):
    port: int = 5900
    username: str = ""
    # NAME of the vault secret holding the Screen Sharing / VNC password ("" = none).
    password_env: str = ""

    _name = field_validator("password_env")(_password_name)


class HostSsh(BaseModel):
    port: int = 22
    user: str = ""
    identity_file: str | None = None
    # NAME of the vault secret holding the account's password ("" = keys only). On a Mac
    # the Screen Sharing and Remote Login accounts are one, so this is usually the name
    # in `vnc.password_env`.
    password_env: str = ""

    _name = field_validator("password_env")(_password_name)


class HostConfig(BaseModel):
    hostname: str
    tailscale_ip: str  # Tailscale IPv4 or MagicDNS name
    os: Literal["macos", "linux", "windows"] = "macos"
    policy: PolicyName = "balanced"
    vnc: HostVnc = HostVnc()
    ssh: HostSsh = HostSsh()


class HostsConfig(BaseModel):
    # As for CoreConfig: a refused value (a password typed where its name belongs) is
    # never repeated in the error the app shows.
    model_config = ConfigDict(hide_input_in_errors=True)

    hosts: dict[str, HostConfig] = {}


def _read_toml(path: str | Path) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


# Fields whose rules were tightened after devices had stored them; see load_config.
_TOLERATED_ON_LOAD = {("llm", "api_key_env"), ("llm", "base_url")}
# The same in hosts.toml, under `hosts.<id>`; see load_hosts.
_TOLERATED_IN_A_HOST = {("vnc", "password_env"), ("ssh", "password_env")}


def load_config(path: str | Path) -> CoreConfig:
    """Load and validate lapis.toml; a missing file means built-in defaults.

    A file an earlier daemon wrote may hold an `llm.api_key_env` or `llm.base_url`
    that is refused today (a pasted key, a URL with a query). That setting never
    worked, so it alone falls back to its default and the rest of the owner's file is
    kept; PUT /v1/config validates the model directly and stays strict.
    """
    if not Path(path).exists():
        return CoreConfig()
    data = _read_toml(path)
    try:
        return CoreConfig.model_validate(data)
    except ValidationError as exc:
        errors = {tuple(e["loc"]): e["msg"] for e in exc.errors()}
        if not set(errors) <= _TOLERATED_ON_LOAD:
            raise
        for (table, key), why in errors.items():
            del data[table][key]
            # The message names the rule, never the stored value (it may be a key).
            print(f"lapis: {path}: ignoring {table}.{key}: {why}", file=sys.stderr, flush=True)
        return CoreConfig.model_validate(data)


def load_hosts(path: str | Path) -> HostsConfig:
    """Load and validate hosts.toml; a missing file means no hosts yet.

    As in `load_config`: a stored `password_env` that is refused today (a password
    typed where the name of its secret belongs; `vnc.password_env` had no rule at
    first) never logged anybody in. That one value is dropped, the host and every
    other host are kept, and PUT /v1/hosts stays strict.
    """
    if not Path(path).exists():
        return HostsConfig()
    data = _read_toml(path)
    try:
        return HostsConfig.model_validate(data)
    except ValidationError as exc:
        errors = {tuple(e["loc"]): e["msg"] for e in exc.errors()}
        if not all(len(loc) == 4 and loc[2:] in _TOLERATED_IN_A_HOST for loc in errors):
            raise
        for (_hosts, host, table, key), why in errors.items():
            del data["hosts"][host][table][key]
            # The message names the rule, never the stored value (it may be a password).
            print(
                f"lapis: {path}: ignoring hosts.{host}.{table}.{key}: {why}",
                file=sys.stderr,
                flush=True,
            )
        return HostsConfig.model_validate(data)


def atomic_write(path: str | Path, data: bytes, mode: int = 0o644) -> None:
    """Write-then-rename in the target directory so readers never see half a file."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def save_toml(path: str | Path, model: BaseModel) -> None:
    """Atomically persist a validated config model as TOML (None fields are omitted)."""
    atomic_write(path, tomli_w.dumps(model.model_dump(exclude_none=True)).encode())


# --- /etc/lapis/secrets.env: write-only from the API's point of view ---------


def read_env_file(path: str | Path) -> dict[str, str]:
    """Parse KEY=VALUE lines (systemd EnvironmentFile subset); missing file -> {}."""
    out: dict[str, str] = {}
    try:
        text = Path(path).read_text()
    except OSError:
        return out
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if SECRET_NAME_RE.match(key.strip()):
            out[key.strip()] = value
    return out


def write_env_file(path: str | Path, values: dict[str, str]) -> None:
    """Atomically rewrite the secrets file, always 0600.

    Every writer (REST, BLE provisioning) ends here, so this is where a name
    that would shadow the daemon's own environment is refused, and a new name
    that reads like a key: names are listed in clear, so a swapped name and value
    would show the key. Names already stored stay writable (the whole file is
    rewritten each time), and so do the LAPIS_... names the app derives from a
    host id.
    """
    stored = read_env_file(path)
    for key, value in values.items():
        if key not in stored and not key.startswith("LAPIS_") and looks_like_secret_value(key):
            raise ValueError(
                "a secret name looks like a key or password: give the secret a name such "
                "as MINIMAX_API_KEY and send the key as its value"
            )
        if not SECRET_NAME_RE.fullmatch(key):  # `match` would let "NAME\n" through
            # Not repeated: what is not a name may be a password (a name and its value
            # swapped), and the heuristic above only knows keys.
            raise ValueError(
                "invalid secret name: a name is letters, digits and _ and does not "
                "start with a digit"
            )
        if is_reserved_name(key):
            raise ValueError(
                f"secret name {key} is reserved (it would replace a variable of the device's "
                "own process): use another name, or unset it if it is already stored"
            )
        if "\n" in value or "\r" in value:
            raise ValueError(f"secret {key} must be a single line")
    body = "".join(f"{k}={v}\n" for k, v in sorted(values.items()))
    atomic_write(path, body.encode(), mode=0o600)


def mask_secret(value: str) -> str:
    """`...a9f2`: the last four characters of a long secret, never more."""
    return f"...{value[-4:]}" if len(value) >= 12 else "..."


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
                os="linux",
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
    secrets: Path = Path(DEFAULT_SECRETS)
    frames_dir: Path = Path(DEFAULT_FRAMES)

    @property
    def ssh_key(self) -> Path:
        """The device's own SSH private key: next to the database, in a 0700 directory."""
        return self.db_path.parent / "ssh" / "id_ed25519"


def effective_paths(config: str | None = None) -> EffectivePaths:
    """Resolve paths; LAPIS_CONFIG_DIR relocates everything for rootless dev and tests."""
    base = os.environ.get("LAPIS_CONFIG_DIR")
    if base:
        d = Path(base)
        paths = EffectivePaths(
            d / "lapis.toml",
            d / "hosts.toml",
            d / "lapis.db",
            d / "lapisd.sock",
            d / "secrets.env",
            d / "frames",
        )
    else:
        paths = EffectivePaths(
            Path(DEFAULT_CONFIG), Path(DEFAULT_HOSTS), Path(DEFAULT_DB), Path(DEFAULT_SOCKET)
        )
    if config:  # explicit --config wins over both defaults
        paths = EffectivePaths(
            Path(config),
            paths.hosts,
            paths.db_path,
            paths.socket_path,
            paths.secrets,
            paths.frames_dir,
        )
    return paths
