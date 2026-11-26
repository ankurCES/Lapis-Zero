"""The device's own SSH key: made on first need, public half shown, private half never.

An ed25519 key in OpenSSH format, written with `cryptography` (a core dependency, so
the key and its endpoint also exist in `--mock` without the `kvm` extra, and showing it
does not load asyncssh). The private half stays in a 0700 directory next to the
database; no endpoint, log line or error carries it.
"""

from __future__ import annotations

import base64
import contextlib
import hashlib
import os
import re
import socket
import tempfile
from pathlib import Path


def ensure_device_key(path: Path) -> Path:
    """Create the private key at `path` unless one is there; returns `path`. Blocking."""
    if path.exists():
        return path
    from cryptography.hazmat.primitives import serialization as ser
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    pem = Ed25519PrivateKey.generate().private_bytes(
        ser.Encoding.PEM, ser.PrivateFormat.OpenSSH, ser.NoEncryption()
    )
    fd, tmp = tempfile.mkstemp(prefix=".key.", dir=path.parent)  # created 0600
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(pem)
            f.flush()
            os.fsync(f.fileno())
        # Linked into place, not renamed over it: a key that appeared meanwhile is kept,
        # because it may already be authorised on a host.
        with contextlib.suppress(FileExistsError):
            os.link(tmp, path)
    finally:
        os.unlink(tmp)
    return path


def public_key(path: Path) -> dict[str, str]:
    """`{"public_key": <one authorized_keys line>, "fingerprint": "SHA256:..."}`. Blocking."""
    from cryptography.hazmat.primitives import serialization as ser

    key = ser.load_ssh_private_key(ensure_device_key(path).read_bytes(), None).public_key()
    line = key.public_bytes(ser.Encoding.OpenSSH, ser.PublicFormat.OpenSSH).decode()
    digest = hashlib.sha256(base64.b64decode(line.split()[1])).digest()
    # The comment is reduced to characters no shell reads as anything else.
    host = re.sub(r"[^A-Za-z0-9._-]", "-", socket.gethostname()) or "device"
    return {
        "public_key": f"{line} lapis-deck@{host}",
        "fingerprint": "SHA256:" + base64.b64encode(digest).decode().rstrip("="),
    }
