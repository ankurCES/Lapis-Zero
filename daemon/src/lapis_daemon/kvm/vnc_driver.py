"""Tailscale VNC capture, downscale, coordinate mapping, and 1-bit preview (SPEC 7.2).

Pure helpers (downscale_dims, to_vnc_coords, compute_crop_box, build_preview_1bit,
pack_bits_msb) keep the coordinate math and thumbnail packing unit-testable without a
live VNC server; `VncDriver` imports `asyncvnc` and `Pillow` lazily via the live methods.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import io
import math
import os
import time
from contextlib import AsyncExitStack
from typing import Any

from lapis_daemon.agent.dhash import dhash_from_image
from lapis_daemon.kvm.base import Frame


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


def preview_from_image(image: Any, cx: int, cy: int) -> str:
    """Pillow path of the approval preview: 240x136 crop -> 120x68 -> inverted
    crosshair -> 1-bit dither, packed MSB-first (15 bytes x 68 rows = 1020), base64.

    Low-memory variant of SPEC 7.2.4: the crop comes from the already-downscaled
    frame the model saw (the native buffer is freed right after encoding), so
    `cx, cy` are screenshot-space coordinates.
    """
    from PIL import Image, ImageChops

    box = compute_crop_box(cx, cy, image.width, image.height)
    thumb = image.crop(box).convert("L").resize((_THUMB_W, _THUMB_H), Image.Resampling.BILINEAR)
    # Where the click landed inside the crop, in thumbnail pixels.
    tx = round((cx - box[0]) * _THUMB_W / max(1, box[2] - box[0]))
    ty = round((cy - box[1]) * _THUMB_H / max(1, box[3] - box[1]))
    mask = Image.new("L", thumb.size, 0)
    half, reach = _CROSS_T // 2, _CROSS_LEN + _CROSS_T
    mask.paste(255, (tx - reach, ty - half, tx + reach + 1, ty + half + 1))
    mask.paste(255, (tx - half, ty - reach, tx + half + 1, ty + reach + 1))
    thumb = Image.composite(ImageChops.invert(thumb), thumb, mask)
    packed = thumb.convert("1").tobytes()
    assert len(packed) == 1020, f"unexpected packed size {len(packed)}"
    return base64.b64encode(packed).decode()


CONNECT_TIMEOUT_S = 15
# A frame is given up when the host sends nothing for this long, or when the whole
# frame takes longer than the second figure. A big desktop over the device's Wi-Fi
# can take tens of seconds (two displays are 31 MB of pixels), so no short fixed limit.
FRAME_IDLE_TIMEOUT_S = 20
FRAME_TIMEOUT_S = 120
# Once an update has arrived that does not cover the whole desktop, how long to wait for
# more before taking the frame as it is. Displays of different sizes leave part of the
# combined desktop belonging to no display, and the host never sends pixels for it
# (found on the first hardware host with two displays: it sent 3 MB and went quiet,
# and waiting for "every pixel" never ended).
FRAME_SETTLE_S = 0.5
# Pixels are scaled into the small image this many bytes at a time, as they arrive, so
# a frame costs a few MB of RAM whatever the size of the desktop (512 MB device; the
# first hardware runs read whole frames and timed out on a 5360x1440 desktop).
STRIPE_BYTES = 2 * 1024 * 1024
# asyncvnc's name for the pixel layout on the wire -> Pillow's raw mode for it.
RAW_MODES = {"rgba": "RGBX", "bgra": "BGRX", "argb": "XRGB", "abgr": "XBGR"}
ENCODINGS = {0: "raw", 6: "zlib"}
# Grayscale thumbnail kept per frame so "did the screen change?" sees small edits
# (a typed line) that a 64-bit dHash is too coarse to notice.
FINE_SIZE = (128, 72)
BUTTONS = {"left": 0, "middle": 1, "right": 2}
# Tool-surface key names (any case) -> asyncvnc / X11 keysym names.
KEY_ALIASES = {
    "ctrl": "Ctrl", "control": "Ctrl", "shift": "Shift", "alt": "Alt", "option": "Alt",
    "cmd": "Cmd", "command": "Cmd", "super": "Super", "meta": "Super", "win": "Super",
    "enter": "Return", "return": "Return", "esc": "Esc", "escape": "Esc", "tab": "Tab",
    "space": " ", "backspace": "BackSpace", "delete": "Delete", "del": "Delete",
    "up": "Up", "down": "Down", "left": "Left", "right": "Right", "home": "Home",
    "end": "End", "pageup": "Page_Up", "pagedown": "Page_Down", "insert": "Insert",
}  # fmt: skip
MODIFIERS = {"shift", "ctrl", "control", "alt", "option", "meta", "super", "cmd", "command", "win"}
# Released unconditionally on disconnect (HID safety, SPEC 7.2.5).
MODIFIER_KEYSYMS = ("Shift_L", "Shift_R", "Control_L", "Control_R", "Alt_L", "Alt_R", "Super_L")


def parse_combo(combo: str) -> list[str]:
    """'ctrl+shift+t' -> ['Ctrl', 'Shift', 't'] (keysym names asyncvnc understands)."""
    keys = []
    for part in combo.replace("-", "+").split("+") if len(combo) > 1 else [combo]:
        token = part.strip()
        if not token:
            continue
        low = token.lower()
        if low in KEY_ALIASES:
            keys.append(KEY_ALIASES[low])
        elif len(low) > 1 and low[0] == "f" and low[1:].isdigit():
            keys.append(low.upper())
        else:
            keys.append(token if len(token) == 1 else token.capitalize())
    return keys


def pressed_mods(combo: str) -> set[str]:
    """Lowercase modifier tokens present in a combo (e.g. 'ctrl+shift+t' -> {ctrl, shift})."""
    parts = [p.strip().lower() for p in combo.split("+")]
    return {p for p in parts if p in MODIFIERS}


def up_combo(mod: str) -> str:
    return f"{mod}-up"


def _env_secret(name: str) -> str | None:
    return os.environ.get(name) or None if name else None


def _scale_into(
    out: Any, data: bytes, box: tuple[int, int, int, int], scale: tuple[float, float], raw: str
) -> None:
    """Scale one block of native pixels (x, y, w, h) into its place in the small image.

    The target box is rounded outwards, so neighbouring blocks overlap by at most a
    pixel instead of leaving a gap.
    """
    from PIL import Image

    x, y, w, h = box
    # The small margin keeps 42.9999999 (an exact 43 but for float error) from
    # being rounded the wrong way.
    left, top = math.floor(x * scale[0] + 1e-6), math.floor(y * scale[1] + 1e-6)
    right = min(math.ceil((x + w) * scale[0] - 1e-6), out.width)
    bottom = min(math.ceil((y + h) * scale[1] - 1e-6), out.height)
    if right <= left or bottom <= top:
        return
    tile = Image.frombuffer("RGBX", (w, h), data, "raw", raw, 0, 1)
    if (right - left, bottom - top) != (w, h):
        # `reducing_gap` box-reduces first, which keeps LANCZOS fast on the CM0.
        tile = tile.resize(
            (right - left, bottom - top), resample=Image.Resampling.LANCZOS, reducing_gap=2.0
        )
    out.paste(tile.convert("RGB"), (left, top))


class VncDriver:
    """On-demand asyncvnc session: capture -> downscale -> JPEG, coord map, HID injection."""

    def __init__(
        self,
        host: Any,
        *,
        llm_max_width: int = 1280,
        llm_max_height: int = 720,
        jpeg_quality: int = 75,
    ) -> None:
        self.host = host
        self.llm_max_width = llm_max_width
        self.llm_max_height = llm_max_height
        self.jpeg_quality = jpeg_quality
        self._stack: AsyncExitStack | None = None
        self._client: Any = None
        self._native = (0, 0)
        self._scaled = (0, 0)
        self._image: Any = None  # last downscaled frame, kept for approval previews
        self.last_frame_hash: int | None = None
        self._wire = 0  # bytes of the frame being read, for the capture note and errors
        self._encodings = ""
        self._covered = 1.0  # share of the desktop the host sent pixels for

    @property
    def connected(self) -> bool:
        return self._client is not None

    async def connect(self) -> None:
        import asyncvnc  # lazy: only a live GUI task needs it (and numpy)

        vnc = self.host.vnc
        where = f"{self.host.tailscale_ip}:{vnc.port}"
        who = f"user {vnc.username!r}" if vnc.username else "the VNC password"
        stack = AsyncExitStack()
        try:
            self._client = await asyncio.wait_for(
                stack.enter_async_context(
                    asyncvnc.connect(
                        self.host.tailscale_ip,
                        vnc.port,
                        vnc.username or None,
                        _env_secret(vnc.password_env),
                    )
                ),
                CONNECT_TIMEOUT_S,
            )
        except TimeoutError as exc:
            await stack.aclose()
            raise ConnectionError(f"VNC connect to {where} timed out") from exc
        except (asyncio.IncompleteReadError, ConnectionResetError, PermissionError) as exc:
            # macOS Screen Sharing answers a login it refuses by closing the connection
            # (seen on macOS 26 with a wrong account: "0 bytes read on a total of 4
            # expected bytes"), which says nothing to the owner. Say what it means.
            await stack.aclose()
            hint = (
                " and that this user is allowed under System Settings > General > Sharing > "
                "Screen Sharing"
                if getattr(self.host, "os", "") == "macos"
                else ""
            )
            raise PermissionError(
                f"{where} refused the screen login for {who}: check the user name and "
                f"password in this host's form{hint}"
            ) from exc
        self._stack = stack

    async def disconnect(self) -> None:
        client, stack = self._client, self._stack
        self._client = self._stack = self._image = None
        if client is not None:
            # Key-up for every modifier, whether or not we think it is held: a
            # stuck Ctrl/Cmd on the user's machine is worse than 7 no-op events.
            # Best effort: the connection may already be dying.
            with contextlib.suppress(Exception):
                import asyncvnc

                for name in MODIFIER_KEYSYMS:
                    code = asyncvnc.key_codes[name].to_bytes(4, "big")
                    client.keyboard.writer.write(b"\x04\x00\x00\x00" + code)
                await asyncio.wait_for(client.drain(), 3)
        if stack is not None:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(stack.aclose(), 5)

    def _to_native(self, x: int, y: int) -> tuple[int, int]:
        if not self._scaled[0]:
            return x, y
        nx, ny = to_vnc_coords(x, y, *self._native, *self._scaled)
        return (
            max(0, min(nx, self._native[0] - 1)),
            max(0, min(ny, self._native[1] - 1)),
        )

    async def _recv(self, n: int) -> bytes | bytearray:
        """Exactly `n` bytes from the host, or TimeoutError when it has gone quiet."""
        reader = self._client.reader
        if n <= 65536:
            async with asyncio.timeout(FRAME_IDLE_TIMEOUT_S):
                data = await reader.readexactly(n)
            self._wire += n
            return data
        # A stripe of pixels: collected in place from small reads. `readexactly` would
        # buffer all of it first and then copy it, twice the stripe at the peak.
        block = bytearray(n)
        view, got = memoryview(block), 0
        while got < n:
            async with asyncio.timeout(FRAME_IDLE_TIMEOUT_S):
                chunk = await reader.read(min(65536, n - got))
            if not chunk:
                raise asyncio.IncompleteReadError(bytes(view[:got]), n)
            view[got : got + len(chunk)] = chunk
            got += len(chunk)
            self._wire += len(chunk)
        return block

    async def _recv_int(self, n: int) -> int:
        return int.from_bytes(await self._recv(n), "big")

    async def _read_frame(self) -> Any:
        """Request the whole screen and build the downscaled image while it arrives.

        Replaces `asyncvnc.Client.screenshot()` for two reasons. asyncvnc 1.3.0 has
        the Bell (2) and ServerCutText (3) message types swapped, so the first
        clipboard or bell notice from the server desynchronises its stream: this loop
        follows RFC 6143 section 7.6 for every server-to-client message. And asyncvnc
        holds a whole frame three times over (wire bytes, decompressed copy, pixel
        array): here each rectangle is scaled into the small image a stripe at a
        time, so the full-resolution frame never exists in memory.
        """
        from PIL import Image

        video = self._client.video
        native = (video.width, video.height)
        scaled = downscale_dims(*native, self.llm_max_width, self.llm_max_height)
        scale = (scaled[0] / native[0], scaled[1] / native[1])
        raw = RAW_MODES[video.mode]
        out = Image.new("RGB", scaled)
        self._wire, encodings, todo, updates = 0, set(), native[0] * native[1], 0
        video.data = None  # asyncvnc's flag for a non-incremental request: the whole screen
        video.refresh()
        while todo > 0:
            try:
                # Between messages only: a one-byte read loses nothing when it times out.
                async with asyncio.timeout(FRAME_SETTLE_S if updates else FRAME_IDLE_TIMEOUT_S):
                    kind = (await self._client.reader.readexactly(1))[0]
            except TimeoutError:
                if not updates:
                    raise
                break  # the host has sent all it has; what no display covers stays black
            self._wire += 1
            if kind == 0:  # FramebufferUpdate
                updates += 1
                await self._recv(1)
                for _ in range(await self._recv_int(2)):
                    x, y, w, h = [await self._recv_int(2) for _ in range(4)]
                    encoding = await self._recv_int(4)
                    if encoding not in ENCODINGS:
                        raise ConnectionError(f"unsupported VNC encoding {encoding}")
                    encodings.add(ENCODINGS[encoding])
                    await self._read_rect(out, (x, y, w, h), encoding, scale, raw)
                    todo -= w * h
            elif kind == 1:  # SetColourMapEntries
                await self._recv(3)
                await self._recv(6 * await self._recv_int(2))
            elif kind == 2:  # Bell
                pass
            elif kind == 3:  # ServerCutText
                await self._recv(3)
                await self._recv(await self._recv_int(4))
            else:
                raise ConnectionError(f"unsupported VNC server message type {kind}")
        self._native, self._scaled = native, scaled
        self._encodings = "+".join(sorted(encodings))
        self._covered = 1 - max(todo, 0) / (native[0] * native[1])
        return out

    async def _read_rect(
        self,
        out: Any,
        box: tuple[int, int, int, int],
        encoding: int,
        scale: tuple[float, float],
        raw: str,
    ) -> None:
        """One rectangle of the update, decoded and scaled a stripe at a time."""
        x, y, w, h = box
        if not w or not h:
            return
        inflate = self._client.video.decompress.__self__  # one zlib stream per connection
        packed = await self._recv_int(4) if encoding == 6 else 0

        async def pixels(n: int) -> bytes | bytearray:
            nonlocal packed
            if encoding == 0:
                return await self._recv(n)
            # Inflated straight into its place: the output is capped at what this
            # stripe still needs, and zlib keeps the rest of the input for the next call.
            block = bytearray(n)
            view, got = memoryview(block), 0
            while got < n:
                if inflate.unconsumed_tail:
                    chunk = inflate.unconsumed_tail
                elif packed:
                    chunk = await self._recv(min(packed, 65536))
                    packed -= len(chunk)
                else:
                    raise ConnectionError("VNC zlib rectangle ended before its last pixel")
                piece = inflate.decompress(chunk, min(n - got, 262144))
                view[got : got + len(piece)] = piece
                got += len(piece)
            return block

        rows_at_once = max(1, STRIPE_BYTES // (w * 4))
        # Stripes that are a whole number of rows in the small image too scale without
        # any stretch. Common screens allow it (1440 rows shown as 344: every 180 rows
        # are 43); otherwise a stripe is rounded outwards, at most a pixel off.
        native_h = self._client.video.height
        unit = native_h // math.gcd(native_h, out.height)
        if y % unit == 0 and unit * w * 4 <= 4 * STRIPE_BYTES:
            rows_at_once = max(unit, rows_at_once // unit * unit)
        done = 0
        while done < h:
            rows = min(rows_at_once, h - done)
            data = await pixels(rows * w * 4)
            # In a thread: the resize is the CPU-heavy part, and the daemon keeps
            # answering the phone and the handheld's screen meanwhile.
            await asyncio.to_thread(_scale_into, out, data, (x, y + done, w, rows), scale, raw)
            del data  # not kept alive while the next stripe is collected
            done += rows
        # Whatever is left of this rectangle's compressed bytes (the flush marker)
        # belongs to it: consumed here so the next rectangle starts in step.
        while encoding == 6 and (packed or inflate.unconsumed_tail):
            if inflate.unconsumed_tail:
                inflate.decompress(inflate.unconsumed_tail, 65536)
            else:
                chunk = await self._recv(min(packed, 65536))
                packed -= len(chunk)
                inflate.decompress(chunk, 65536)

    async def screenshot(self) -> Frame:
        """Capture, downscaled while it is received; encode JPEG."""
        started = time.monotonic()
        try:
            img = await asyncio.wait_for(self._read_frame(), FRAME_TIMEOUT_S)
        except (TimeoutError, ConnectionError, asyncio.IncompleteReadError) as exc:
            # A half-read frame leaves the stream unusable: reconnect on next use.
            sent = getattr(self, "_wire", 0)
            await self.disconnect()
            if isinstance(exc, TimeoutError):
                # A bare TimeoutError has no text: the owner saw "TimeoutError: ".
                raise TimeoutError(
                    f"the host stopped sending its screen after {sent / 1e6:.1f} MB "
                    f"and {time.monotonic() - started:.0f} s"
                ) from exc
            raise

        def _encode() -> tuple[bytes, bytes]:
            from PIL import Image

            buf = io.BytesIO()
            img.save(buf, format="JPEG", quality=self.jpeg_quality)
            fine = img.convert("L").resize(FINE_SIZE, Image.Resampling.BILINEAR).tobytes()
            return buf.getvalue(), fine

        jpeg, fine = await asyncio.to_thread(_encode)
        self._image = img
        self.last_frame_hash = dhash_from_image(img)
        native, scaled = self._native, self._scaled
        note = (
            f"{native[0]}x{native[1]} shown as {scaled[0]}x{scaled[1]}, {self._encodings}, "
            f"{self._wire / 1e6:.1f} MB in {time.monotonic() - started:.1f} s"
        )
        if self._covered < 0.995:
            # Usually several displays of different sizes: say so, the picture is small.
            note += f", displays cover {self._covered:.0%} of that area"
        return Frame(jpeg, scaled[0], scaled[1], self.last_frame_hash, fine, note)

    async def click(self, x: int, y: int, button: str = "left", clicks: int = 1) -> None:
        self._client.mouse.move(*self._to_native(x, y))
        for _ in range(max(1, min(clicks, 3))):
            self._client.mouse.click(BUTTONS.get(button, 0))
        await self._client.drain()

    async def move(self, x: int, y: int) -> None:
        self._client.mouse.move(*self._to_native(x, y))
        await self._client.drain()

    async def drag(self, x1: int, y1: int, x2: int, y2: int) -> None:
        sx, sy = self._to_native(x1, y1)
        ex, ey = self._to_native(x2, y2)
        mouse = self._client.mouse
        mouse.move(sx, sy)
        with mouse.hold():
            for i in range(1, 9):  # a few intermediate points so drop targets react
                mouse.move(sx + (ex - sx) * i // 8, sy + (ey - sy) * i // 8)
                await self._client.drain()
                await asyncio.sleep(0.02)
        await self._client.drain()

    async def type_text(self, text: str) -> None:
        import asyncvnc

        keyboard = self._client.keyboard
        for i, ch in enumerate(text):
            key = {"\n": "Return", "\t": "Tab"}.get(ch, ch)
            if key in asyncvnc.key_codes:
                keyboard.press(key)
            if i % 32 == 31:
                await self._client.drain()
        await self._client.drain()

    async def key_press(self, combo: str) -> None:
        import asyncvnc

        keys = parse_combo(combo)
        unknown = [k for k in keys if k not in asyncvnc.key_codes]
        if unknown or not keys:
            raise ValueError(f"unknown key(s) in combo {combo!r}: {unknown or combo}")
        self._client.keyboard.press(*keys)  # press all, release in reverse: nothing stays held
        await self._client.drain()

    async def scroll(self, x: int, y: int, dx: int, dy: int) -> None:
        mouse = self._client.mouse
        mouse.move(*self._to_native(x, y))
        if dy:
            (mouse.scroll_down if dy > 0 else mouse.scroll_up)(min(abs(dy), 20))
        for _ in range(min(abs(dx), 20)):
            mouse.click(6 if dx > 0 else 5)  # X11 horizontal wheel buttons
        await self._client.drain()

    def preview(self, x: int, y: int) -> str | None:
        return preview_from_image(self._image, x, y) if self._image is not None else None
