"""Tests for the PTT voice pipeline (Phase 6)."""

from __future__ import annotations

import asyncio

from lapis_harness.config import VoiceConfig
from lapis_harness.voice import (
    FakeTranscriber,
    NullCuePlayer,
    SyntheticSource,
    VoiceController,
    ensure_wav,
    generate_cue,
    normalize_rms,
)


def _sine_pcm(sample_rate: int = 16000, n: int = 2560, freq: float = 440.0) -> bytes:
    import math
    import struct

    return b"".join(
        struct.pack("<h", int(12000 * math.sin(2 * math.pi * freq * i / sample_rate)))
        for i in range(n)
    )


def test_normalize_rms_is_bounded_and_silence_is_zero() -> None:
    assert normalize_rms(b"") == 0.0
    assert normalize_rms(b"\x00\x01") == 0.0
    rms = normalize_rms(_sine_pcm())
    assert rms is not None and 0.0 < rms <= 1.0
    # A full-scale square wave saturates at ~1.0.
    assert normalize_rms(b"\xff\x7f" * 100) == 1.0


def test_cue_and_wav_are_wave_parseable() -> None:
    import io
    import wave

    for kind in ("ok", "error", "listen", "start", "stop", "beep"):
        data = generate_cue(kind)
        with wave.open(io.BytesIO(data)) as w:
            assert w.getnchannels() == 1
            assert w.getsampwidth() == 1
            assert w.getnframes() > 0

    wav = ensure_wav(_sine_pcm(), 16000, 1)
    with wave.open(io.BytesIO(wav)) as w:
        assert w.getnchannels() == 1
        assert w.getframerate() == 16000


def test_fake_transcriber_records_and_returns() -> None:
    t = FakeTranscriber("hello")
    pcm = _sine_pcm()
    assert t.transcribe(pcm) == "hello"
    assert t.calls == [pcm]


def test_voice_controller_captures_transcribes_and_dispatches() -> None:
    sent: list[float] = []
    transcript = "Check the build"
    cfg = VoiceConfig(enabled=True, max_capture_bytes=64 * 1024)

    async def scenario() -> None:
        controller = VoiceController(
            cfg,
            source_factory=lambda: SyntheticSource(chunks=16),
            transcriber_factory=lambda: FakeTranscriber(transcript),
            player=NullCuePlayer(),
            send_mic=sent.append,
            on_transcript=lambda text: sent.append(99.9) if text else None,
        )
        await controller.begin_ptt()
        await controller.end_ptt()
        # RMS stream: one value per captured chunk (16) + a final 0.0.
        assert any(0.0 < v <= 1.0 for v in sent)
        assert sent[-1] == 0.0
        assert 99.9 in sent
        assert controller.capturing is False
        await controller.close()

    asyncio.run(scenario())


def test_voice_controller_dispatch_can_be_async() -> None:
    cfg = VoiceConfig(enabled=True, max_capture_bytes=4096)
    dispatched: list[str] = []

    async def on_transcript(text: str) -> None:
        await asyncio.sleep(0)
        dispatched.append(text)

    async def scenario() -> None:
        controller = VoiceController(
            cfg,
            source_factory=lambda: SyntheticSource(chunks=4),
            transcriber_factory=lambda: FakeTranscriber("async dispatch"),
            player=NullCuePlayer(),
            on_transcript=on_transcript,
        )
        await controller.begin_ptt()
        await controller.end_ptt()
        assert dispatched == ["async dispatch"]
        await controller.close()

    asyncio.run(scenario())


def test_voice_controller_drops_excess_to_ring_buffer() -> None:
    # Small ceiling + many chunks: the final transcript byte count never exceeds it.
    cfg = VoiceConfig(enabled=True, max_capture_bytes=2560)

    async def scenario() -> None:
        controller = VoiceController(
            cfg,
            source_factory=lambda: SyntheticSource(chunks=40),
            transcriber_factory=lambda: FakeTranscriber("x"),
            player=NullCuePlayer(),
        )
        await controller.begin_ptt()
        await controller.end_ptt()
        await controller.close()

    asyncio.run(scenario())
