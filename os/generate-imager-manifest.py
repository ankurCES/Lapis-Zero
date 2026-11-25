#!/usr/bin/env python3
"""Generate an M5 Imager-compatible `os-list.json` for Lapis-Zero OS.

The manifest conforms to CardputerZero/m5stack-imager's
`m5stack/os-list-template.json` schema: an `imager` object with a `devices`
list, and an `os_list` array whose entries carry byte-exact `extract_size`,
`extract_sha256`, `image_download_size`, and `image_download_sha256`.

Usage:
    os/generate-imager-manifest.py --self-test          # schema/fixture check
    os/generate-imager-manifest.py -o os-list.json      # needs LAPIS_IMG_XZ (+ RAW)
    LAPIS_IMG_XZ=lapis-zero-arm64.img.xz \
    LAPIS_IMG_RAW=lapis-zero-arm64.img \
      os/generate-imager-manifest.py

Environment:
    LAPIS_IMG_XZ    path to the compressed download (.img.xz)
    LAPIS_IMG_RAW   path to the uncompressed raw image (for extract_size/…
                    extract_sha256). Optional: if unset, `extract_size` and
                    `extract_sha256` are left at 0/"" (stock template does the
                    same for the CardputerZero entry).
    LAPIS_RELEASE_URL  base URL for the download `url` field
                    (default: https://example.invalid/lapis-zero/…)
    LAPIS_RELEASE_DATE release_date (default: today UTC)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from datetime import datetime, timezone

# Capitals so shell / config wiring stays obvious.
DEVICE_NAME = "CardputerZero"
DEVICE_ICON = "https://cardputer-zero-repo.oss-cn-shenzhen.aliyuncs.com/icons/cardputerzero.png"
DEVICE_TAGS = ["m5-cardputerzero", "pi3-64bit"]
DEVICE_ARCH = "armv8"
DEVICE_CAPABILITIES = ["i2c", "spi", "serial", "usb_otg"]
OS_NAME = "Lapis-Zero OS (Trixie arm64)"
OS_DESCRIPTION = (
    "Dedicated monochrome agent OS with Option C Tailscale KVM for CardputerZero."
)
DEFAULT_RELEASE_URL = (
    "https://example.invalid/lapis-zero/releases/lapis-zero-arm64.img.xz"
)


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def build_manifest(
    xz_path: str,
    raw_path: str | None,
    release_url: str,
    release_date: str,
) -> dict:
    xz_size = os.path.getsize(xz_path)
    xz_sha = sha256_file(xz_path)

    extract_size = 0
    extract_sha = ""
    if raw_path and os.path.exists(raw_path):
        extract_size = os.path.getsize(raw_path)
        extract_sha = sha256_file(raw_path)

    return {
        "imager": {
            "latest_version": "1.0.0",
            "url": "https://github.com/CardputerZero/m5stack-imager/releases",
            "devices": [
                {
                    "name": DEVICE_NAME,
                    "description": "M5Stack pocket Raspberry Pi CM0 computer.",
                    "icon": DEVICE_ICON,
                    "tags": list(DEVICE_TAGS),
                    "default": True,
                    "matching_type": "exclusive",
                    "architecture": DEVICE_ARCH,
                    "capabilities": list(DEVICE_CAPABILITIES),
                }
            ],
        },
        "os_list": [
            {
                "name": OS_NAME,
                "description": OS_DESCRIPTION,
                "icon": DEVICE_ICON,
                "url": release_url,
                "extract_size": extract_size,
                "extract_sha256": extract_sha,
                "image_download_size": xz_size,
                "image_download_sha256": xz_sha,
                "release_date": release_date,
                "init_format": "systemd",
                "devices": list(DEVICE_TAGS),
                "architecture": DEVICE_ARCH,
                "capabilities": list(DEVICE_CAPABILITIES),
            }
        ],
    }


def validate_schema(manifest: dict) -> list[str]:
    """Checks the fields the m5stack-imager template relies on."""
    errors: list[str] = []

    if set(manifest) != {"imager", "os_list"}:
        errors.append("top-level keys must be exactly {imager, os_list}")

    imager = manifest.get("imager", {})
    if not isinstance(imager, dict) or "devices" not in imager:
        errors.append("imager.devices missing")
    else:
        for dev in imager["devices"]:
            for key in (
                "name",
                "description",
                "tags",
                "default",
                "matching_type",
                "architecture",
                "capabilities",
            ):
                if key not in dev:
                    errors.append(f"imager.devices[*].{key} missing")

    os_list = manifest.get("os_list", [])
    if not isinstance(os_list, list) or not os_list:
        errors.append("os_list must be a non-empty list")
    else:
        required = {
            "name",
            "description",
            "url",
            "extract_size",
            "extract_sha256",
            "image_download_size",
            "image_download_sha256",
            "release_date",
            "init_format",
            "devices",
            "architecture",
            "capabilities",
        }
        for entry in os_list:
            missing = required - set(entry)
            if missing:
                errors.append(f"os_list entry missing keys: {sorted(missing)}")
            if entry.get("init_format") not in ("systemd", "sysvinit"):
                errors.append("init_format must be 'systemd' (m5stack-imager)")
            # image_download_* must be a positive size and 64-hex hash.
            if not isinstance(entry.get("image_download_size"), int):
                errors.append("image_download_size must be an integer (bytes)")
            elif entry["image_download_size"] <= 0:
                errors.append("image_download_size must be > 0")
            sha = entry.get("image_download_sha256", "")
            if not (isinstance(sha, str) and len(sha) == 64):
                errors.append("image_download_sha256 must be 64 hex chars")

    return errors


def self_test(tmpdir: str | None = None) -> bool:
    """Create a tiny fake image pair and validate a manifest end-to-end."""
    import tempfile

    with tempfile.TemporaryDirectory() as td:
        raw = os.path.join(td, "fake.img")
        xz = os.path.join(td, "fake.img.xz")
        with open(raw, "wb") as f:
            f.write(b"\x00" * 1024)
        # A valid xz stream (empty). hashing works regardless of payload.
        import lzma

        with lzma.open(xz, "wb") as f:
            f.write(os.urandom(4096))

        manifest = build_manifest(
            xz, raw, DEFAULT_RELEASE_URL, datetime.now(timezone.utc).date().isoformat()
        )
        errors = validate_schema(manifest)
        if errors:
            print("self-test FAILED:")
            for e in errors:
                print("  -", e)
            return False
        # Round-trip JSON.
        json.dumps(manifest)
        assert manifest["os_list"][0]["extract_size"] == 1024
        assert len(manifest["os_list"][0]["image_download_sha256"]) == 64
        assert len(manifest["os_list"][0]["extract_sha256"]) == 64
    print("self-test: OK (schema valid, SHA256/size fields populated)")
    return True


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Emit an M5 Imager-compatible os-list.json for Lapis-Zero OS."
    )
    parser.add_argument("-o", "--output", default="os-list.json")
    parser.add_argument("--self-test", action="store_true", help="run internal schema check")
    parser.add_argument("--release-url", default=None)
    parser.add_argument("--release-date", default=None)
    parser.add_argument("--validate-only", action="store_true", help="validate an existing manifest")
    parser.add_argument("file", nargs="?", help="(validate-only) path to an existing os-list.json")
    args = parser.parse_args(argv)

    if args.self_test:
        return 0 if self_test() else 1

    if args.validate_only:
        if not args.file:
            print("--validate-only requires a file path", file=sys.stderr)
            return 2
        with open(args.file, encoding="utf-8") as f:
            manifest = json.load(f)
        errors = validate_schema(manifest)
        if errors:
            print("schema validation failed:")
            for e in errors:
                print("  -", e)
            return 1
        print("schema validation: OK")
        return 0

    xz_path = os.environ.get("LAPIS_IMG_XZ", "")
    if not xz_path or not os.path.exists(xz_path):
        print(
            "LAPIS_IMG_XZ not set or missing (the compressed .img.xz); "
            "run `make build-os` first, or use --self-test",
            file=sys.stderr,
        )
        return 2
    raw_path = os.environ.get("LAPIS_IMG_RAW", "") or None

    release_url = args.release_url or os.environ.get("LAPIS_RELEASE_URL", DEFAULT_RELEASE_URL)
    release_date = args.release_date or os.environ.get(
        "LAPIS_RELEASE_DATE", datetime.now(timezone.utc).date().isoformat()
    )

    manifest = build_manifest(xz_path, raw_path, release_url, release_date)
    errors = validate_schema(manifest)
    if errors:
        print("internal schema validation failed:", file=sys.stderr)
        for e in errors:
            print("  -", e, file=sys.stderr)
        return 1

    with open(args.output, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print(f"wrote {args.output}")

    entry = manifest["os_list"][0]
    print(f"  image_download_size : {entry['image_download_size']} bytes")
    print(f"  image_download_sha256: {entry['image_download_sha256']}")
    print(f"  extract_size         : {entry['extract_size']} bytes")
    print(f"  extract_sha256       : {entry['extract_sha256']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
