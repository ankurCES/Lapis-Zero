"""Tailscale VNC capture, downscale, coordinate mapping, and 1-bit preview (SPEC 7.2).

Pure helpers (downscale_dims, to_vnc_coords, compute_crop_box, build_preview_1bit,
pack_bits_msb) keep the coordinate math and thumbnail packing unit-testable without a
live VNC server; `VncDriver` imports `asyncvnc` and `Pillow` lazily via the live methods.
"""

from __future__ import annotations

import asyncio
import base64
import io
import os
import time
from typing import Any

from lapis_daemon.agent.dhash import dhash_from_image


def downscale_dims(
    native_w: int, native_h: int, max_w: int = 1280, max_h: int = 720
) -> tuple[int, int]:
    """Scale a native frame to fit within max_w x max_h, aspect ratio preserved."""
    if native_w <= max_w and native_h <= max_h:
        return native_w, native_h
    scale = min(max_w / native_w, max_h / native_h)
    return max(1, round(native_w * scale)), max(1, round(native_h * scale))


def to_vnc_coords(
    x_llm: float, y_llm: float, native_w: int, native_h: int, scaled_w: int, scaled_h: int
) -> tuple[int, int]:
    """Deterministic LLM(space) -> native(VNC) coordinate mapping (SPEC 7.2.3)."""
    return round(x_llm * native_w / scaled_w), round(y_llm * native_h / scaled_h)


_CROP_W, _CROP_H = 240, 136
_THUMB_W, _THUMB_H = 120, 68
_CROSS_T, _CROSS_LEN = 7, 5  # 7px arm thickness, 5px reach: cross is 7 x 15 at centre


def compute_crop_box(cx: int, cy: int, native_w: int, native_h: int) -> tuple[int, int, int, int]:
    """Crop a `_CROP_W` x `_CROP_H` box centred on (cx, cy), clamped to the frame."""
    left = max(0, min(cx - _CROP_W // 2, native_w - _CROP_W))
    top = max(0, min(cy - _CROP_H // 2, native_h - _CROP_H))
    # Clamp the box to the frame: a native frame smaller than the crop must not overflow.
    right = min(native_w, left + _CROP_W)
    bottom = min(native_h, top + _CROP_H)
    return left, top, right, bottom


def pack_bits_msb(row_bits: list[int]) -> bytes:
    """Pack a row of 0/1 pixels MSB-first into ceil(len/8) bytes, zero-padded on the right."""
    out = bytearray((len(row_bits) + 7) // 8)
    for i, bit in enumerate(row_bits):
        if bit:
            out[i // 8] |= 0x80 >> (i % 8)
    return bytes(out)


def build_preview_1bit(
    native_rgb: list[list[int]],
    cx: int,
    cy: int,
    native_w: int,
    native_h: int,
    *,
    _thumb_w: int = _THUMB_W,
    _thumb_h: int = _THUMB_H,
) -> bytes:
    """Produce the 120x68 1-bit approval preview with a 7px inverted crosshair.

    The real pipeline crops a Pillow frame, resizes to 120x68, draws the crosshair, then
    dithers to 1-bit. This function accepts a flattened native RGB tile (row-major pixels,
    each an [r,g,b] triple) and is a pure, image-free reference used by the tests. Returns
    exactly `_thumb_w * _thumb_h // 8` == 1020 packed bytes.
    """
    crop_left, crop_top, crop_right, crop_bottom = compute_crop_box(cx, cy, native_w, native_h)
    crop_w, crop_h = crop_right - crop_left, crop_bottom - crop_top

    # Box-filter downsample (nearest-neighbour fallback is fine at these ratios).
    y_coords = [
        min(crop_h - 1, (y * crop_h * 2 + _thumb_h) // (_thumb_h * 2)) for y in range(_thumb_h)
    ]
    x_coords = [
        min(crop_w - 1, (x * crop_w * 2 + _thumb_w) // (_thumb_w * 2)) for x in range(_thumb_w)
    ]
    thumb: list[list[int]] = []
    for ty in range(_thumb_h):
        luma: list[int] = []
        for tx in range(_thumb_w):
            src_x, src_y = x_coords[tx] + crop_left, y_coords[ty] + crop_top
            r0, g0, b0 = native_rgb[src_y][src_x]
            luma.append((r0 * 299 + g0 * 587 + b0 * 114) // 1000)
        thumb.append(luma)

    # Finite plus-sign crosshair (7px arms/thickness) centred on the click point,
    # inverted against the local background (SPEC 7.2.4).
    mid_x, mid_y = _thumb_w // 2, _thumb_h // 2
    reach = _CROSS_LEN + _CROSS_T
    for ty in range(_thumb_h):
        for tx in range(_thumb_w):
            dy = ty - mid_y
            dx = tx - mid_x
            on_h = abs(dy) < _CROSS_T and abs(dx) <= reach
            on_v = abs(dx) < _CROSS_T and abs(dy) <= reach
            if on_h or on_v:
                thumb[ty][tx] ^= 0xFF

    rows: list[bytes] = []
    for ty in range(_thumb_h):
        row = thumb[ty]
        # 1-bit threshold at 50% then pack MSB-first
        bits = [1 if p >= 0x80 else 0 for p in row]
        rows.append(pack_bits_msb(bits))
    packed = b"".join(rows)
    assert len(packed) == 1020, f"unexpected packed size {len(packed)}"
    return base64.b64encode(packed)


class VncDriver:
    """On-demand asyncvnc driver: capture -> downscale -> JPEG, coord map, HID injection."""

    def __init__(
        self,
        host: Any,
        *,
        llm_max_width: int = 1280,
        llm_max_height: int = 720,
        jpeg_quality: int = 75,
        idle_disconnect_sec: int = 60,
        emit: Any = None,
    ) -> None:
        self.host = host
        self.llm_max_width = llm_max_width
        self.llm_max_height = llm_max_height
        self.jpeg_quality = jpeg_quality
        self.idle_disconnect_sec = idle_disconnect_sec
        self.emit = emit
        self._client: Any = None
        self._last_use = 0.0
        self.held_modifiers: set[str] = set()
        self.last_frame_hash: int | None = None

    async def connect(self) -> None:
        import asyncvnc  # lazy

        vnc = self.host.vnc
        self._client = await asyncvnc.connect(
            self.host.tailscale_ip,
            port=vnc.port,
            username=vnc.username or None,
            password=_env_secret(vnc.password_env),
        )
        self._last_use = time.monotonic()

    async def disconnect(self) -> None:
        # HID safety: release every held modifier before tearing down the session.
        if self._client is not None and self.held_modifiers:
            for combo in self._held_up_combos():
                with suppress_exceptions():
                    await self._client.key_press(combo)
            self.held_modifiers.clear()
        if self._client is not None:
            await self._client.close()
            self._client = None

    def _held_up_combos(self) -> list[str]:
        return [up_combo(m) for m in sorted(self.held_modifiers)]

    async def screenshot(self) -> dict[str, Any]:
        """Capture, downscale via Pillow LANCZOS, and encode JPEG, then free buffers."""
        if self._client is None:
            await self.connect()
        frame = self._client.screenshot()
        native_w, native_h = frame.width, frame.height
        scaled_w, scaled_h = downscale_dims(
            native_w, native_h, self.llm_max_width, self.llm_max_height
        )
        pixels = frame.pixels
        del frame  # free the full-resolution buffer immediately (512 MB budget)

        def _encode(pix: Any, nat_w: int, nat_h: int) -> tuple[Any, bytes]:
            from PIL import Image

            img = Image.fromarray(pix)
            del pix
            if (scaled_w, scaled_h) != (nat_w, nat_h):
                img = img.resize((scaled_w, scaled_h), resample=Image.Resampling.LANCZOS)
            buf = io.BytesIO()
            img.save(buf, format="JPEG", quality=self.jpeg_quality)
            return img, buf.getvalue()

        img, jpeg = await asyncio.to_thread(_encode, pixels, native_w, native_h)
        self.last_frame_hash = dhash_from_image(img)
        del img  # free the resized buffer immediately (512 MB budget)
        self._last_use = time.monotonic()
        return {
            "jpeg_b64": base64.b64encode(jpeg).decode(),
            "width": scaled_w,
            "height": scaled_h,
            "native_width": native_w,
            "native_height": native_h,
        }

    async def click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> None:
        if self._client is None:
            await self.connect()
        await self._client.mouse_click(x, y, button=button, clicks=clicks)
        self._last_use = time.monotonic()

    async def type_text(self, text: str) -> None:
        if self._client is None:
            await self.connect()
        await self._client.type_text(text)
        self._last_use = time.monotonic()

    async def key_press(self, combo: str) -> None:
        if self._client is None:
            await self.connect()
        self.held_modifiers |= pressed_mods(combo)
        await self._client.key_press(combo)
        self._last_use = time.monotonic()

    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        if self._client is None:
            await self.connect()
        await self._client.scroll(x, y, dx, dy)
        self._last_use = time.monotonic()

    async def maybe_idle_disconnect(self) -> None:
        if (
            self._client is not None
            and time.monotonic() - self._last_use >= self.idle_disconnect_sec
        ):
            await self.disconnect()


class suppress_exceptions:
    """Tiny context manager; keeps modifier key-up cleanup best-effort without try/pass."""

    def __enter__(self) -> None:
        return None

    def __exit__(self, *exc: object) -> bool:
        return True


def _env_secret(name: str) -> str | None:
    value = os.environ.get(name)
    return value or None


MODIFIERS = {"shift", "ctrl", "control", "alt", "meta", "super", "cmd"}


def pressed_mods(combo: str) -> set[str]:
    """Lowercase modifier tokens present in a combo (e.g. 'ctrl+shift+t' -> {ctrl, shift})."""
    parts = [p.strip().lower() for p in combo.split("+")]
    return {p for p in parts if p in MODIFIERS}


def up_combo(mod: str) -> str:
    return f"{mod}-up"
