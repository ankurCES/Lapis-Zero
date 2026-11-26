#!/usr/bin/env python3
"""Raspberry Pi Imager `os-list.json` for a Lapis-Deck image (CM4 = the pi4 64-bit family).

`imager-manifest.py --image X.img.xz --url URL -o os-list.json`; `--self-test` checks itself.
Sizes and SHA-256s are computed by streaming (the raw image is never written to disk).
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import lzma
import pathlib
import sys
import tempfile


def digest(path: pathlib.Path, decompress: bool) -> tuple[int, str]:
    h, n = hashlib.sha256(), 0
    opener = lzma.open if decompress else open
    with opener(path, "rb") as f:  # type: ignore[operator]
        while chunk := f.read(1 << 20):
            h.update(chunk)
            n += len(chunk)
    return n, h.hexdigest()


def manifest(image: pathlib.Path, url: str) -> dict[str, object]:
    raw_size, raw_sha = digest(image, decompress=True)
    xz_size, xz_sha = digest(image, decompress=False)
    return {
        "os_list": [
            {
                "name": "Lapis-Deck (ClockworkPi uConsole CM4)",
                "description": "Headless Debian 13 trixie arm64 cyberdeck OS: DRM/KMS HUD, SDR, "
                "Meshtastic, GPS, NetOps and a policy-gated agent harness. No X11.",
                "url": url,
                "release_date": datetime.date.today().isoformat(),
                "extract_size": raw_size,
                "extract_sha256": raw_sha,
                "image_download_size": xz_size,
                "image_download_sha256": xz_sha,
                "devices": ["pi4-64bit"],
                "init_format": "none",
            }
        ]
    }


def self_test() -> None:
    with tempfile.TemporaryDirectory() as d:
        img = pathlib.Path(d) / "t.img.xz"
        payload = b"lapis" * 1000
        img.write_bytes(lzma.compress(payload))
        m = manifest(img, "file:///t.img.xz")["os_list"][0]  # type: ignore[index]
        assert m["extract_size"] == len(payload)
        assert m["extract_sha256"] == hashlib.sha256(payload).hexdigest()
        assert m["image_download_size"] == img.stat().st_size
    print("imager-manifest self-test: ok")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--image", type=pathlib.Path)
    p.add_argument("--url")
    p.add_argument("-o", "--output", type=pathlib.Path)
    p.add_argument("--self-test", action="store_true")
    a = p.parse_args()
    if a.self_test:
        self_test()
        return 0
    if not (a.image and a.url and a.output):
        p.error("--image, --url and -o are required")
    a.output.write_text(json.dumps(manifest(a.image, a.url), indent=2) + "\n")
    print(f"wrote {a.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
