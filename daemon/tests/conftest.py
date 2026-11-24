"""Shared fixtures."""

from __future__ import annotations

import shutil
import tempfile
from collections.abc import Iterator
from pathlib import Path

import pytest


@pytest.fixture
def short_tmp() -> Iterator[Path]:
    """Temp dir with a short path: Unix socket paths cap near 104 bytes on macOS."""
    path = Path(tempfile.mkdtemp(prefix="lapis-", dir="/tmp"))
    yield path
    shutil.rmtree(path, ignore_errors=True)
