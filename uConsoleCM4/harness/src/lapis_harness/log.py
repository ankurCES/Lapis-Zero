"""The harness's journal lines: every one passes through `redact` first (AGENTS.md invariant 4).

`log()` is what the harness's own code calls. `install()` also routes the `logging`
records of the libraries it uses (aiohttp, asyncssh) through the same redaction.
"""

from __future__ import annotations

import logging
import sys

from lapis_harness.redact import redact

PREFIX = "lapis-harness"


def log(message: str, *, error: bool = False) -> None:
    """One journal line (stdout, or stderr for an error), redacted."""
    print(redact(f"{PREFIX}: {message}"), file=sys.stderr if error else sys.stdout, flush=True)


class RedactingFormatter(logging.Formatter):
    """A formatter whose every line, traceback included, is redacted."""

    def format(self, record: logging.LogRecord) -> str:
        return redact(super().format(record))


def install(level: int = logging.WARNING) -> None:
    """Send library logging to stderr through `RedactingFormatter` (idempotent)."""
    root = logging.getLogger()
    for handler in root.handlers:
        if isinstance(handler.formatter, RedactingFormatter):
            return
    handler = logging.StreamHandler(sys.stderr)
    handler.setFormatter(RedactingFormatter(f"{PREFIX}: %(name)s: %(message)s"))
    root.addHandler(handler)
    root.setLevel(level)
