"""Secret redaction, mirroring `lapis-crypto::redact` rule for rule.

Both are tested against `tests/vectors/redaction_vectors.json`. Everything the harness logs
or sends to the daemon as an event passes through `redact` / `redact_json` first.
"""

from __future__ import annotations

import re
import threading
from typing import Any

MARK = "[REDACTED]"
PEM_MARK = "[REDACTED PRIVATE KEY]"
MIN_KNOWN_LEN = 8

_PEM_BLOCK = re.compile(
    r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----"
)
_PEM_CUT = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*")
_AUTH = re.compile(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}")
_TOKENS = re.compile(
    r"\b(?:sk-ant-[A-Za-z0-9_-]{16,}|sk-[A-Za-z0-9_-]{20,}|AIza[0-9A-Za-z_-]{30,}"
    r"|tskey-[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}"
    r"|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})"
)
_KEY_VALUE = re.compile(
    r"(?i)\b([A-Za-z0-9_.-]*?(?:api[_-]?key|secret|token|password|passwd|psk|authkey|private[_-]?key))"
    r"([\"']?)(\s*[:=]\s*)(\"(?:[^\"\\]|\\.)*\"|'[^']*'|[^\s\"',;}&]+)"
)
_SECRET_KEY = re.compile(
    r"(?i)^[A-Za-z0-9_.-]*(?:api[_-]?key|secret|token|password|passwd|psk|authkey|private[_-]?key)$"
)

_known: list[str] = []
_lock = threading.Lock()


def register_secret(value: str) -> None:
    """Scrub this exact value wherever it appears (loaded API keys, tokens)."""
    value = value.strip()
    if len(value) < MIN_KNOWN_LEN:
        return
    with _lock:
        if value not in _known:
            _known.append(value)
            _known.sort(key=len, reverse=True)


def _key_value(m: re.Match[str]) -> str:
    value = m.group(4)
    if value.lower() in ("null", "true", "false") or value in ('""', "''"):
        shown = value
    elif value.startswith('"'):
        shown = f'"{MARK}"'
    elif value.startswith("'"):
        shown = f"'{MARK}'"
    else:
        shown = MARK
    return f"{m.group(1)}{m.group(2)}{m.group(3)}{shown}"


def redact(text: str) -> str:
    with _lock:
        known = list(_known)
    for secret in known:
        text = text.replace(secret, MARK)
    text = _PEM_BLOCK.sub(PEM_MARK, text)
    text = _PEM_CUT.sub(PEM_MARK, text)
    text = _AUTH.sub(lambda m: f"{m.group(1)} {MARK}", text)
    text = _TOKENS.sub(MARK, text)
    return _KEY_VALUE.sub(_key_value, text)


def redact_json(value: Any) -> Any:
    """A copy of a JSON value with secret-named fields blanked and every string redacted."""
    if isinstance(value, str):
        return redact(value)
    if isinstance(value, list):
        return [redact_json(v) for v in value]
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        for key, v in value.items():
            blank = not (v is None or isinstance(v, bool) or v == "")
            if _SECRET_KEY.match(str(key)) and blank:
                out[key] = MARK
            else:
                out[key] = redact_json(v)
        return out
    return value
