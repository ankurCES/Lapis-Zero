from __future__ import annotations

import base64

from lapis_daemon.kvm.vnc_driver import (
    compute_crop_box,
    downscale_dims,
    pack_bits_msb,
    pressed_mods,
    to_vnc_coords,
    up_combo,
)


def test_downscale_within_target_preserves_aspect() -> None:
    assert downscale_dims(1920, 1080) == (1280, 720)
    assert downscale_dims(1600, 1200) == (960, 720)
    assert downscale_dims(1024, 600) == (1024, 600)  # smaller than target: no upscale


def test_to_vnc_coords_rounds_deterministically() -> None:
    # 1920x1080 native scaled to 1280x720; LLM (840, 412) maps exactly.
    assert to_vnc_coords(840, 412, 1920, 1080, 1280, 720) == (1260, 618)
    # same scales: identity
    assert to_vnc_coords(55, 33, 1280, 720, 1280, 720) == (55, 33)


def test_compute_crop_box_clamps_to_edges() -> None:
    assert compute_crop_box(160, 24, 1920, 1080) == (40, 0, 280, 136)
    # click near a corner clamps without going negative or past the frame
    assert compute_crop_box(5, 5, 1920, 1080) == (0, 0, 240, 136)
    assert compute_crop_box(1919, 1079, 1920, 1080) == (1680, 944, 1920, 1080)


def test_pack_bits_msb() -> None:
    assert pack_bits_msb([1, 0, 1, 0, 0, 0, 0, 0]) == bytes([0xA0])
    # MSB-first: first bit is the most significant bit
    assert pack_bits_msb([1]) == bytes([0x80])
    assert pack_bits_msb([0, 0, 0, 0, 0, 0, 0, 1]) == bytes([0x01])


def test_preview_1020_bytes_msb_first() -> None:
    from lapis_daemon.kvm.vnc_driver import build_preview_1bit

    # solid 240x136 crop so packing is deterministic: 68 rows x 120 px -> 15 bytes/row.
    native_w, native_h = 1920, 1080
    rgb = [[(0, 0, 0)] * native_w for _ in range(native_h)]
    b64 = build_preview_1bit(rgb, 960, 540, native_w, native_h)
    packed = base64.b64decode(b64)
    assert len(packed) == 1020
    # dark background away from the crosshair packs to 0x00
    assert packed[0:15] == b"\x00" * 15
    assert packed[-15:] == b"\x00" * 15

    # solid bright (255) -> 0xFF rows away from the crosshair
    rgb = [[(255, 255, 255)] * native_w for _ in range(native_h)]
    b64 = build_preview_1bit(rgb, 960, 540, native_w, native_h)
    packed = base64.b64decode(b64)
    assert packed[0:15] == b"\xff" * 15
    assert packed[-15:] == b"\xff" * 15


def test_preview_crosshair_inverts_center() -> None:
    from lapis_daemon.kvm.vnc_driver import build_preview_1bit

    native_w, native_h = 240, 136  # crop == whole frame, no downscale distortion
    rgb = [[(255, 255, 255)] * native_w for _ in range(native_h)]
    b64 = build_preview_1bit(rgb, 120, 68, native_w, native_h)
    packed = base64.b64decode(b64)
    # bright background packs to 0xFF rows; the inverted horizontal crosshair bar flips to 0.
    # centre row (y=34) flips every pixel the crosshair covers -> at least one clear bit.
    centre_row = packed[34 * 15 : 35 * 15]
    assert centre_row != b"\xff" * 15  # horizontal bar inverted against bright background
    # a far-from-center row is still all bright
    assert packed[0:15] == b"\xff" * 15


def test_modifier_parsing_and_up_combos() -> None:
    assert pressed_mods("ctrl+shift+t") == {"ctrl", "shift"}
    assert pressed_mods("cmd+space") == {"cmd"}
    assert pressed_mods("a") == set()
    assert up_combo("shift") == "shift-up"
