"""Paired-device bearer tokens and one-time pairing nonces.

Only SHA-256 hashes of tokens are stored. A token is minted in exactly two
places, both requiring physical proximity to the device: the BLE PIN handshake
and the exchange of a QR nonce that was displayed on the LCD.
"""

from __future__ import annotations

import hashlib
import secrets
import time
from typing import Any

from lapis_daemon.db import LapisDb

NONCE_TTL_SEC = 180
MAX_EXCHANGE_FAILURES = 5  # per rolling minute, across all clients
SEEN_WRITE_INTERVAL = 60.0


def hash_token(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


def mint_token() -> str:
    """256 bits of CSPRNG output, URL-safe."""
    return secrets.token_urlsafe(32)


def register_device_sync(db: LapisDb, name: str, model: str) -> tuple[str, str]:
    """Blocking variant for the BLE provisioning process; returns (device_id, token)."""
    device_id, token = f"dev_{secrets.token_hex(4)}", mint_token()
    db.run_sync(
        "INSERT INTO paired_devices(id, name, model, token_hash, created_at)"
        " VALUES (?, ?, ?, ?, ?)",
        (device_id, name[:64] or "Android", model[:64], hash_token(token), time.time()),
    )
    return device_id, token


class Auth:
    def __init__(self, db: LapisDb) -> None:
        self.db = db
        self._nonces: dict[str, float] = {}  # nonce -> expiry (monotonic)
        self._failures: list[float] = []
        self._seen: dict[str, float] = {}

    async def register(self, name: str, model: str = "") -> tuple[str, str]:
        device_id, token = f"dev_{secrets.token_hex(4)}", mint_token()
        await self.db.run(
            "INSERT INTO paired_devices(id, name, model, token_hash, created_at)"
            " VALUES (?, ?, ?, ?, ?)",
            (device_id, name[:64] or "Android", model[:64], hash_token(token), time.time()),
        )
        return device_id, token

    async def register_fixed(self, device_id: str, name: str, token: str) -> None:
        """Development token for `--mock --dev-pair-token` (idempotent)."""
        await self.db.run("DELETE FROM paired_devices WHERE id = ?", (device_id,))
        await self.db.run(
            "INSERT OR REPLACE INTO paired_devices(id, name, model, token_hash, created_at)"
            " VALUES (?, ?, 'dev', ?, ?)",
            (device_id, name, hash_token(token), time.time()),
        )

    async def verify(self, token: str) -> dict[str, Any] | None:
        """The paired device for a bearer token, or None (unknown or revoked)."""
        if not token:
            return None
        rows = await self.db.run(
            "SELECT id, name, model FROM paired_devices WHERE token_hash = ? AND revoked = 0",
            (hash_token(token),),
        )
        if not rows:
            return None
        device = rows[0]
        now = time.time()
        if now - self._seen.get(device["id"], 0) > SEEN_WRITE_INTERVAL:  # spare the SD card
            self._seen[device["id"]] = now
            await self.db.run(
                "UPDATE paired_devices SET last_seen_at = ? WHERE id = ?", (now, device["id"])
            )
        return device

    async def devices(self) -> list[dict[str, Any]]:
        rows = await self.db.run(
            "SELECT id, name, model, created_at, last_seen_at, revoked FROM paired_devices"
            " ORDER BY created_at"
        )
        return [
            {
                **r,
                "revoked": bool(r["revoked"]),
                "created_at": int(r["created_at"]),
                "last_seen_at": int(r["last_seen_at"]) if r["last_seen_at"] else None,
            }
            for r in rows
        ]

    async def count(self) -> int:
        rows = await self.db.run("SELECT COUNT(*) AS n FROM paired_devices WHERE revoked = 0")
        return int(rows[0]["n"])

    async def revoke(self, device_id: str) -> bool:
        rows = await self.db.run("SELECT id FROM paired_devices WHERE id = ?", (device_id,))
        if not rows:
            return False
        await self.db.run("UPDATE paired_devices SET revoked = 1 WHERE id = ?", (device_id,))
        return True

    # -- QR pairing nonces -----------------------------------------------------

    def new_nonce(self) -> str:
        """Single-use, 32 random bytes, expires after 180 s."""
        self._prune()
        nonce = secrets.token_urlsafe(32)
        self._nonces[nonce] = time.monotonic() + NONCE_TTL_SEC
        return nonce

    def cancel_nonces(self) -> None:
        self._nonces.clear()

    def _prune(self) -> None:
        now = time.monotonic()
        self._nonces = {n: exp for n, exp in self._nonces.items() if exp > now}
        self._failures = [t for t in self._failures if now - t < 60]

    @property
    def locked_out(self) -> bool:
        self._prune()
        return len(self._failures) >= MAX_EXCHANGE_FAILURES

    def consume_nonce(self, nonce: str) -> bool:
        """True exactly once per live nonce; failures count toward the lockout."""
        self._prune()
        for known in list(self._nonces):
            if secrets.compare_digest(known, nonce or ""):
                del self._nonces[known]
                return True
        self._failures.append(time.monotonic())
        return False
