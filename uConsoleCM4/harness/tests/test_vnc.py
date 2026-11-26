from __future__ import annotations

import base64

from lapis_harness.kvm.vnc_driver import (
    compute_crop_box,
    downscale_dims,
    pack_bits_msb,
    parse_combo,
    pressed_mods,
    preview_from_image,
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
    from lapis_harness.kvm.vnc_driver import build_preview_1bit

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
    from lapis_harness.kvm.vnc_driver import build_preview_1bit

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


def test_preview_from_image_is_1020_packed_bytes_with_an_inverted_crosshair() -> None:
    from PIL import Image

    white = Image.new("RGB", (1280, 720), (255, 255, 255))
    packed = base64.b64decode(preview_from_image(white, 640, 360))
    assert len(packed) == 1020  # 120 x 68 / 8, 15 bytes per row, MSB first

    def pixel(x: int, y: int) -> int:
        return packed[y * 15 + x // 8] >> (7 - x % 8) & 1

    # White frame -> set bits; the crosshair at the centre is inverted to black.
    assert pixel(2, 2) == 1 and pixel(117, 65) == 1
    assert pixel(60, 34) == 0 and pixel(60 + 10, 34) == 0 and pixel(60, 34 - 10) == 0
    # A click in the corner keeps the crosshair on the click, not on the crop centre.
    corner = base64.b64decode(preview_from_image(white, 3, 3))

    def cpixel(x: int, y: int) -> int:
        return corner[y * 15 + x // 8] >> (7 - x % 8) & 1

    assert cpixel(1, 1) == 0 and cpixel(60, 34) == 1


def test_parse_combo_maps_tool_names_to_keysyms() -> None:
    assert parse_combo("ctrl+shift+t") == ["Ctrl", "Shift", "t"]
    assert parse_combo("cmd+space") == ["Cmd", " "]
    assert parse_combo("Return") == ["Return"] and parse_combo("escape") == ["Esc"]
    assert parse_combo("f5") == ["F5"] and parse_combo("alt+F4") == ["Alt", "F4"]
    assert parse_combo("a") == ["a"] and parse_combo("+") == ["+"]


class _Writer:
    def __init__(self) -> None:
        self.sent = b""

    def write(self, data: bytes) -> None:
        self.sent += data


def _session(width: int, height: int, mode: str = "rgba", **driver_kwargs):
    """A VncDriver wired to an in-memory stream that plays the host's side."""
    import asyncio
    import zlib

    import asyncvnc

    from lapis_harness.kvm.vnc_driver import VncDriver

    reader, writer = asyncio.StreamReader(), _Writer()
    video = asyncvnc.Video(
        reader, writer, zlib.decompressobj().decompress, "t", width, height, mode
    )
    driver = VncDriver(host=None, **driver_kwargs)
    driver._client = type("Client", (), {"reader": reader, "video": video})()
    return driver, reader, writer


def _rect(x: int, y: int, w: int, h: int, encoding: int, payload: bytes) -> bytes:
    head = b"".join(v.to_bytes(2, "big") for v in (x, y, w, h)) + encoding.to_bytes(4, "big")
    return head + payload


def _update(*rects: bytes) -> bytes:
    return b"\x00\x00" + len(rects).to_bytes(2, "big") + b"".join(rects)


def test_frame_reader_survives_bell_and_clipboard_messages() -> None:
    """Regression: asyncvnc 1.3.0 swaps Bell (2) and ServerCutText (3) and loses sync."""
    import asyncio

    async def scenario() -> None:
        w, h = 4, 2
        driver, reader, writer = _session(w, h)
        rgba = bytes(range(w * h * 4))
        reader.feed_data(
            b"\x03\x00\x00\x00"
            + (5).to_bytes(4, "big")
            + b"hello"  # ServerCutText "hello"
            + b"\x02"  # Bell
            + b"\x01\x00\x00\x00\x00\x01"
            + bytes(6)  # SetColourMapEntries, one colour
            + _update(_rect(0, 0, w, h, 0, rgba))  # one raw rectangle
        )
        img = await asyncio.wait_for(driver._read_frame(), 2)
        assert img.size == (w, h)
        assert img.getpixel((1, 0)) == tuple(rgba[4:7])
        assert img.getpixel((3, 1)) == tuple(rgba[(w + 3) * 4 : (w + 3) * 4 + 3])
        assert writer.sent[0] == 3 and writer.sent[1] == 0  # a full (non-incremental) request

        reader.feed_data(b"\x09")
        try:
            await asyncio.wait_for(driver._read_frame(), 2)
        except ConnectionError as exc:
            assert "message type 9" in str(exc)
        else:
            raise AssertionError("an unknown message type must not be skipped silently")

    asyncio.run(scenario())


def _desktop(width: int, height: int):
    """Smooth colour everywhere (a gap or a misplaced stripe shows) plus sharp marks."""
    from PIL import Image, ImageDraw

    img = Image.linear_gradient("L").resize((width, height)).convert("RGB")
    across = Image.linear_gradient("L").rotate(90).resize((width, height))
    img = Image.merge("RGB", (img.getchannel("R"), across, Image.new("L", (width, height), 96)))
    draw = ImageDraw.Draw(img)
    for cx, cy in _marks(width, height):
        draw.rectangle((cx - 12, cy - 12, cx + 12, cy + 12), fill=(255, 255, 255))
    return img


def _marks(width: int, height: int) -> list[tuple[int, int]]:
    return [(width * i // 9, height * j // 7) for i in range(1, 9) for j in range(1, 7)]


def test_a_big_desktop_is_scaled_as_it_arrives_and_never_held_whole() -> None:
    """Two displays side by side, as on the first hardware host: 5360x1440, 31 MB raw.

    The picture must match a plain downscale of the whole frame, in every pixel layout
    and both encodings, with tiles as well as one big rectangle; what the model clicks
    on must be where the linear mapping says; and the full frame must never sit in
    memory (the device has 512 MB and a 130 MB soft cap).
    """
    import asyncio
    import tracemalloc
    import zlib

    import numpy as np
    from PIL import Image, ImageChops, ImageStat

    width, height = 5360, 1440
    source = _desktop(width, height)
    want = source.resize((1280, 344), Image.Resampling.LANCZOS, reducing_gap=2.0)

    async def capture(mode: str, encoding: int, tiles: bool) -> tuple[Image.Image, int]:
        driver, reader, _writer = _session(width, height, mode)
        # The four layouts as byte positions of R, G, B and the unused byte.
        order = {
            "rgba": [0, 1, 2, 3],
            "bgra": [2, 1, 0, 3],
            "argb": [3, 0, 1, 2],
            "abgr": [3, 2, 1, 0],
        }
        packer = zlib.compressobj()

        def payload(box: tuple[int, int, int, int]) -> bytes:
            rgbx = np.frombuffer(source.crop(box).convert("RGBX").tobytes(), "B").reshape(-1, 4)
            data = rgbx[:, order[mode]].tobytes()
            if encoding == 0:
                return data
            packed = packer.compress(data) + packer.flush(zlib.Z_SYNC_FLUSH)
            return len(packed).to_bytes(4, "big") + packed

        boxes = (
            [
                (x, y, min(x + 1340, width), min(y + 500, height))
                for y in range(0, height, 500)
                for x in range(0, width, 1340)
            ]
            if tiles
            else [(0, 0, width, height)]
        )
        update = _update(
            *[_rect(b[0], b[1], b[2] - b[0], b[3] - b[1], encoding, payload(b)) for b in boxes]
        )
        # Fed as a socket would deliver it: a little at a time, pausing while the reader
        # has plenty (asyncio's own flow control), so the test measures the driver.
        whole = memoryview(update)

        async def host() -> None:
            sent = 0
            while sent < len(whole):
                if len(reader._buffer) < 128 * 1024:
                    reader.feed_data(whole[sent : sent + 64 * 1024])
                    sent += 64 * 1024
                await asyncio.sleep(0)

        tracemalloc.start()
        feeding = asyncio.create_task(host())
        img = await asyncio.wait_for(driver._read_frame(), 60)
        await feeding
        peak = tracemalloc.get_traced_memory()[1]
        tracemalloc.stop()
        assert driver._wire == len(update), "every byte of the update is consumed, no more"
        return img, peak

    async def scenario() -> None:
        for mode, encoding, tiles in (
            ("bgra", 0, False),  # what macOS sends when it sends raw
            ("bgra", 6, False),
            ("rgba", 6, True),
            ("argb", 0, True),
            ("abgr", 6, False),
        ):
            img, peak = await capture(mode, encoding, tiles)
            case = (mode, encoding, tiles)
            assert img.size == (1280, 344)
            diff = ImageChops.difference(img, want)
            assert max(ImageStat.Stat(diff).mean) < 1.0, (case, ImageStat.Stat(diff).mean)
            # A gap, or a stripe put in the wrong place, would be a row far off the rest.
            rows = diff.convert("L").resize((1, 344), Image.Resampling.BOX)
            assert max(rows.tobytes()) < 8, (case, max(rows.tobytes()))
            # Each mark is where the linear mapping (the one clicks are mapped back by) says.
            bright = img.convert("L").point(lambda v: 255 if v > 250 else 0)
            for cx, cy in _marks(width, height):
                x, y = cx * 1280 / width, cy * 344 / height
                box = bright.crop((int(x) - 6, int(y) - 6, int(x) + 7, int(y) + 7)).getbbox()
                assert box is not None, (case, cx, cy)
                mid = (int(x) - 6 + (box[0] + box[2]) / 2, int(y) - 6 + (box[1] + box[3]) / 2)
                assert abs(mid[0] - x) <= 1.5 and abs(mid[1] - y) <= 1.5, (case, (cx, cy), mid)
            # The small image is 1.3 MB and a stripe at most 4 MB: far below the 31 MB frame.
            assert peak < 10 * 1024 * 1024, (case, peak)

    asyncio.run(scenario())


def test_a_screen_that_needs_no_scaling_arrives_pixel_for_pixel_and_frames_follow_each_other() -> (
    None
):
    import asyncio
    import zlib

    async def scenario() -> None:
        w, h = 640, 400
        source = _desktop(w, h)
        driver, reader, _writer = _session(w, h, "bgra")
        packer = zlib.compressobj()
        data = source.convert("RGBX").tobytes("raw", "BGRX")
        for _ in range(3):  # the zlib stream runs on across frames: all three must decode
            packed = packer.compress(data) + packer.flush(zlib.Z_SYNC_FLUSH)
            reader.feed_data(_update(_rect(0, 0, w, h, 6, len(packed).to_bytes(4, "big") + packed)))
            frame = await asyncio.wait_for(driver.screenshot(), 10)
            assert (frame.width, frame.height) == (w, h)
            assert driver._image.tobytes() == source.tobytes()
            assert frame.note.startswith("640x400 shown as 640x400, zlib, ")
            driver._client.video.data = None

    asyncio.run(scenario())


def test_two_displays_of_different_heights_make_a_frame_although_part_is_never_sent() -> None:
    """Found on the first hardware host: 3440x1440 beside 1920x1080 is a 5360x1440
    desktop of which the 1920x360 strip under the second display belongs to no
    display. The host sends the two displays and then nothing; the frame is that."""
    import asyncio
    import time

    async def scenario() -> None:
        w, h = 536, 144  # the same layout at a tenth of the size
        driver, reader, _writer = _session(w, h, "bgra")
        left, right = bytes([10, 20, 30, 0]) * (344 * 144), bytes([200, 100, 50, 0]) * (192 * 108)
        reader.feed_data(_update(_rect(0, 0, 344, 144, 0, left), _rect(344, 0, 192, 108, 0, right)))
        started = time.monotonic()
        frame = await asyncio.wait_for(driver.screenshot(), 5)
        assert time.monotonic() - started < 2, "it does not wait out the long silence limit"
        img = driver._image
        assert img.getpixel((100, 100)) == (30, 20, 10)  # BGRX on the wire
        assert img.getpixel((500, 50)) == (50, 100, 200)
        assert img.getpixel((500, 130)) == (0, 0, 0), "what no display covers is black"
        assert "displays cover 91% of that area" in frame.note, frame.note

        # The next capture starts cleanly, and a host that sends nothing at all still fails.
        reader.feed_data(_update(_rect(0, 0, 344, 144, 0, left), _rect(344, 0, 192, 108, 0, right)))
        frame = await asyncio.wait_for(driver.screenshot(), 5)
        assert (frame.width, frame.height) == (w, h)

    asyncio.run(scenario())


def test_a_host_that_goes_quiet_mid_frame_is_given_up_with_words(monkeypatch) -> None:
    import asyncio

    import pytest

    from lapis_harness.kvm import vnc_driver

    async def scenario() -> None:
        monkeypatch.setattr(vnc_driver, "FRAME_IDLE_TIMEOUT_S", 0.2)
        driver, reader, _writer = _session(64, 64)
        reader.feed_data(_update(_rect(0, 0, 64, 64, 0, bytes(1000))))  # then silence

        async def gone() -> None:
            driver._client = None

        driver.disconnect = gone  # the stand-in client has no connection to close
        with pytest.raises(TimeoutError, match=r"stopped sending its screen after 0\.0 MB"):
            await driver.screenshot()
        assert driver._client is None, "a half-read stream is not reused"

    asyncio.run(scenario())


def test_a_refused_login_is_reported_as_one_not_as_a_read_error() -> None:
    """macOS closes the connection on a login it refuses; asyncvnc then fails mid-read."""
    import asyncio

    import pytest

    from lapis_harness.config import HostConfig, HostVnc
    from lapis_harness.kvm.vnc_driver import VncDriver

    async def scenario() -> None:
        async def mac(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            writer.write(b"RFB 003.889\n")
            await reader.readline()
            writer.write(bytes([1, 33]))  # one security type: Apple's RSA1
            await reader.readexactly(1 + 14)  # the choice and the key request
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric import rsa

            key = rsa.generate_private_key(public_exponent=65537, key_size=2048).public_key()
            der = key.public_bytes(
                serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
            )
            writer.write(
                (len(der) + 7).to_bytes(4, "big")
                + b"\x00\x01"
                + len(der).to_bytes(4, "big")
                + der
                + b"\x00"
            )
            await reader.readexactly(4 + 394)  # the encrypted credentials
            writer.close()  # refused: no answer at all

        server = await asyncio.start_server(mac, "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        host = HostConfig(
            hostname="mac",
            tailscale_ip="127.0.0.1",
            os="macos",
            vnc=HostVnc(port=port, username="alice", password_env="LAPIS_TEST_VNC_PW"),
        )
        import os

        os.environ["LAPIS_TEST_VNC_PW"] = "not the right one"
        try:
            with pytest.raises(PermissionError) as refused:
                await VncDriver(host).connect()
        finally:
            del os.environ["LAPIS_TEST_VNC_PW"]
            server.close()
            await server.wait_closed()
        text = str(refused.value)
        assert "refused the screen login for user 'alice'" in text and "Screen Sharing" in text
        assert "not the right one" not in text and "IncompleteRead" not in text

    asyncio.run(scenario())
