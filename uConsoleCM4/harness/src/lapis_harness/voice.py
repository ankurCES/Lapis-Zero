"""Push-to-Talk voice pipeline (SPEC 6, Phase 6).

Scope: ALSA 16 kHz mono capture while PTT is held, live `mic_level` RMS streaming
to the UI waveform, STT transcription on release, and automatic task enqueue.
Capture, transcription and audio-cue playback are all swappable behind small
interfaces so the daemon can run and be tested without audio hardware (mock mode).

Everything that touches a device or network is a closure/factory passed in; the
controller itself is pure asyncio orchestration.
"""

from __future__ import annotations

import array
import asyncio
import contextlib
import inspect
import json
import math
import os
import struct
import subprocess
import sys
import urllib.request
import wave
from abc import ABC, abstractmethod
from collections.abc import Callable
from typing import Any

from lapis_harness.config import VoiceConfig

# Default chunk: ~80 ms at 16 kHz mono S16_LE (2560 bytes). Small enough to feel
# live, large enough to avoid syscall-per-sample overhead.
DEFAULT_CHUNK_BYTES = 2560

# Cue playback sample rate (8-bit mono PCM fits the 1 W speaker budget).
CUE_SAMPLE_RATE = 22050

# Hard ceiling on buffered raw PCM while PTT is held (0.5 MB ≈ 16 s of audio);
# protects the 512 MB RAM budget against a stuck PTT key.
DEFAULT_MAX_CAPTURE_BYTES = 512 * 1024

# Display noise floor for `mic_level` RMS: anything below ~1% of full scale is
# reported as silence so ADC hiss does not jitter the UI waveform.
RMS_FLOOR = 0.01


# ---------------------------------------------------------------------------
# Signal helpers (pure)
# ---------------------------------------------------------------------------


def normalize_rms(pcm: bytes) -> float:
    """Normalize the RMS of 16-bit LE mono PCM to 0.0..=1.0.

    Empty input and sub-`RMS_FLOOR` noise collapse to ``0.0`` (pure silence,
    never ``None``), so `mic_level` is always a float the UI can plot.
    """
    n = len(pcm) // 2
    samples = array.array("h")
    samples.frombytes(pcm[: n * 2])
    if sys.byteorder != "little":
        samples.byteswap()
    if n == 0:
        return 0.0
    # Python ints are arbitrary-precision; normalizing by 32767 makes a
    # full-scale +32767 square wave saturate to exactly 1.0.
    total = sum(s * s for s in samples)
    rms = (total / n) ** 0.5 / 32767.0
    rms = min(rms, 1.0)
    return 0.0 if rms < RMS_FLOOR else rms


def _wav_8bit(samples: list[int], sample_rate: int) -> bytes:
    """Pack unsigned 8-bit mono samples into a minimal WAV container (PCM)."""
    data = bytes(max(0, min(255, s)) for s in samples)
    header = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF",
        36 + len(data),
        b"WAVE",
        b"fmt ",
        16,
        1,  # PCM
        1,  # mono
        sample_rate,
        sample_rate,  # byte rate = sample_rate * 1 channel * 1 byte
        1,  # block align
        8,  # bits per sample
        b"data",
        len(data),
    )
    return header + data


def _tone(freq: float, ms: int, vol: int = 127, sample_rate: int = CUE_SAMPLE_RATE) -> list[int]:
    """Synthesize a constant-amplitude square tone (cheap, no numpy)."""
    n = sample_rate * ms // 1000
    period = sample_rate / freq if freq else 1
    return [128 + int((1.0 if (i % period) < period / 2 else -1.0) * vol) for i in range(n)]


def generate_cue(kind: str, sample_rate: int = CUE_SAMPLE_RATE) -> bytes:
    """Procedural 8-bit UI chime for `kind` (ok|error|listen|start|stop|beep)."""
    kind = kind.lower()
    if kind == "ok":
        samples = _tone(880, 60) + _tone(1320, 90, vol=140)
    elif kind == "error":
        samples = _tone(220, 160, vol=150) + _tone(180, 160, vol=150)
    elif kind == "listen":
        samples = _tone(660, 50, vol=110)
    elif kind == "start":
        samples = _tone(440, 30, vol=100)
    elif kind == "stop":
        samples = _tone(330, 30, vol=100)
    else:
        samples = _tone(520, 80)
    return _wav_8bit(samples, sample_rate)


def ensure_wav(pcm: bytes, sample_rate: int, channels: int) -> bytes:
    """Wrap raw S16_LE PCM in a canonical WAV header (for STT HTTP multipart)."""
    import io

    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm)
    return buf.getvalue()


# ---------------------------------------------------------------------------
# Capture sources
# ---------------------------------------------------------------------------


class CaptureSource(ABC):
    """A blocking S16_LE mono PCM byte source; [`read`][CaptureSource.read] returns
    ``b""`` on EOF and ``None`` only after the device was closed."""

    @abstractmethod
    def start(self) -> None: ...

    @abstractmethod
    def read(self) -> bytes: ...

    @abstractmethod
    def stop(self) -> None: ...

    @abstractmethod
    def close(self) -> None: ...


class ArecordSource(CaptureSource):
    """ALSA capture via `arecord` (no PulseAudio/PipeWire; SPEC §1 Audio)."""

    def __init__(
        self,
        device: str,
        sample_rate: int,
        channels: int,
        chunk_bytes: int = DEFAULT_CHUNK_BYTES,
    ) -> None:
        self.device = device
        self.sample_rate = sample_rate
        self.channels = channels
        self.chunk_bytes = chunk_bytes
        self._proc: subprocess.Popen[bytes] | None = None

    def start(self) -> None:
        cmd = [
            "arecord",
            "-q",
            "-t",
            "raw",
            "-f",
            "S16_LE",
            "-c",
            str(self.channels),
            "-r",
            str(self.sample_rate),
        ]
        if self.device and self.device != "default":
            cmd += ["-D", self.device]
        self._proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def read(self) -> bytes:
        proc = self._proc
        if proc is None or proc.stdout is None:
            return b""
        return os.read(proc.stdout.fileno(), self.chunk_bytes)

    def stop(self) -> None:
        proc = self._proc
        if proc is None:
            return
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()

    def close(self) -> None:
        self.stop()
        self._proc = None


class SyntheticSource(CaptureSource):
    """Deterministic sine-wave source for tests and `--mock` (no audio hardware)."""

    def __init__(
        self,
        sample_rate: int = 16000,
        chunk_bytes: int = DEFAULT_CHUNK_BYTES,
        chunks: int = 40,
        freq: float = 440.0,
    ) -> None:
        self.sample_rate = sample_rate
        self.chunk_bytes = chunk_bytes
        self.chunks = chunks
        self.freq = freq
        self._n = 0
        self._phase = 0.0

    def start(self) -> None:
        self._n = 0
        self._phase = 0.0

    def read(self) -> bytes:
        if self._n >= self.chunks:
            return b""
        self._n += 1
        n = self.chunk_bytes // 2
        out = bytearray()
        step = 2.0 * math.pi * self.freq / self.sample_rate
        for _ in range(n):
            sample = int(12000 * math.sin(self._phase))
            out += struct.pack("<h", sample)
            self._phase = (self._phase + step) % (2.0 * math.pi)
        return bytes(out)

    def stop(self) -> None:
        self._n = self.chunks

    def close(self) -> None:
        self.stop()


# ---------------------------------------------------------------------------
# Transcribers (STT)
# ---------------------------------------------------------------------------


class Transcriber(ABC):
    @abstractmethod
    def transcribe(self, pcm: bytes) -> str: ...


class FakeTranscriber(Transcriber):
    """Returns a canned transcript and records what it received (mock/tests)."""

    def __init__(self, text: str = "Run diagnostics on the target host") -> None:
        self.text = text
        self.calls: list[bytes] = []

    def transcribe(self, pcm: bytes) -> str:
        self.calls.append(pcm)
        return self.text


class HttpTranscriber(Transcriber):
    """OpenAI-compatible `/v1/audio/transcriptions` client (stdlib only)."""

    def __init__(
        self,
        url: str,
        model: str,
        sample_rate: int,
        channels: int,
        api_key_env: str,
        timeout_sec: int = 30,
    ) -> None:
        self.url = url
        self.model = model
        self.sample_rate = sample_rate
        self.channels = channels
        self.api_key_env = api_key_env
        self.timeout_sec = timeout_sec

    def transcribe(self, pcm: bytes) -> str:
        if not pcm:
            return ""
        api_key = os.environ.get(self.api_key_env, "")
        wav = ensure_wav(pcm, self.sample_rate, self.channels)
        boundary = "----LapisZeroVoiceBoundary7"
        body = (
            (
                f"--{boundary}\r\n"
                'Content-Disposition: form-data; name="model"\r\n\r\n'
                f"{self.model}\r\n"
                f"--{boundary}\r\n"
                'Content-Disposition: form-data; name="file"; filename="capture.wav"\r\n'
                "Content-Type: audio/wav\r\n\r\n"
            ).encode()
            + wav
            + f"\r\n--{boundary}--\r\n".encode()
        )
        headers = {"Content-Type": f"multipart/form-data; boundary={boundary}"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
        req = urllib.request.Request(self.url, data=body, headers=headers, method="POST")
        with urllib.request.urlopen(req, timeout=self.timeout_sec) as resp:
            payload = json.loads(resp.read().decode())
        return str(payload.get("text") or "").strip()


# ---------------------------------------------------------------------------
# Audio-cue playback
# ---------------------------------------------------------------------------


class CuePlayer(ABC):
    @abstractmethod
    def play(self, kind: str) -> None: ...


class NullCuePlayer(CuePlayer):
    """No-op player that records calls (mock/tests)."""

    def __init__(self) -> None:
        self.played: list[str] = []

    def play(self, kind: str) -> None:
        self.played.append(kind)


class AplayCuePlayer(CuePlayer):
    """Plays 8-bit cues through ALSA `aplay` (stdin)."""

    def __init__(self, sample_rate: int = CUE_SAMPLE_RATE) -> None:
        self.sample_rate = sample_rate

    def play(self, kind: str) -> None:
        wav = generate_cue(kind, self.sample_rate)
        subprocess.run(["aplay", "-q"], input=wav, stdout=subprocess.DEVNULL, check=False)


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


class VoiceController:
    """Owns the PTT lifecycle: one capture session at a time, RAM-bounded."""

    def __init__(
        self,
        config: VoiceConfig,
        *,
        source_factory: Callable[[], CaptureSource],
        transcriber_factory: Callable[[], Transcriber],
        player: CuePlayer | None = None,
        send_mic: Callable[[float], None] | None = None,
        on_transcript: Callable[[str], Any] | None = None,
        on_error: Callable[[BaseException], None] | None = None,
    ) -> None:
        config = config  # retained for introspection / future use
        self._config = config
        self._source_factory = source_factory
        self._transcriber_factory = transcriber_factory
        self._player = player if player is not None else NullCuePlayer()
        self._send_mic = send_mic or (lambda _rms: None)
        self._on_transcript = on_transcript or (lambda _text: None)
        self._on_error = on_error or (lambda _exc: None)
        self._task: asyncio.Task[None] | None = None
        self._started = asyncio.Event()
        self.capturing = False

    @property
    def enabled(self) -> bool:
        return self._config.enabled

    @property
    def active(self) -> bool:
        return self._task is not None and not self._task.done()

    async def start(self) -> None:
        """Idempotent; capture is spawned on the first PTT press."""

    async def begin_ptt(self) -> None:
        if self.active:
            raise ValueError("PTT already active")
        self._started.clear()
        # Baseline meter frame before any live RMS so the waveform resets to a
        # clean zero on PTT press.
        self._send_mic(0.0)
        self._task = asyncio.create_task(self._capture_loop())
        # Wait until capture is actually rolling (first chunk read) before
        # returning, so a prompt `end_ptt()` never cancels the loop before any
        # RMS has flowed. The loop guarantees `_started` is set even on failure.
        await self._started.wait()

    async def end_ptt(self) -> None:
        """Stop capture, transcribe, dispatch, and return the final transcript."""
        task = self._task
        if task is None:
            return None
        self._task = None
        if not task.done():
            task.cancel()
        try:
            await task
        except asyncio.CancelledError as e:
            # The loop runs full transcription before it first yields to cancel,
            # so we continue on; the transcript is emitted by the loop itself.
            del e

    async def _capture_loop(self) -> None:
        source = self._source_factory()
        buf = bytearray()
        first_rms_sent = False
        try:
            await asyncio.to_thread(source.start)
            self.capturing = True
            while True:
                chunk = await asyncio.to_thread(source.read)
                if not chunk:
                    break
                buf.extend(chunk)
                if len(buf) > self._config.max_capture_bytes:
                    del buf[: len(buf) - self._config.max_capture_bytes]
                self._send_mic(normalize_rms(chunk))
                if not first_rms_sent:
                    first_rms_sent = True
                    # Signal only once real audio has flowed, so `begin_ptt`
                    # cannot return while capture is still opening.
                    self._started.set()
                await asyncio.sleep(0)
        except asyncio.CancelledError:
            # PTT released: finalize below.
            pass
        except Exception as exc:
            self._on_error(exc)
        finally:
            # Never leave `begin_ptt` awaiting a source that errored out.
            # When the source yields no chunk at all (e.g. no device) the loop
            # breaks above without setting the event.
            self._started.set()
            await asyncio.to_thread(source.close)
            self.capturing = False
            text = ""
            try:
                text = await asyncio.to_thread(self._transcriber_factory().transcribe, bytes(buf))
            except Exception as exc:
                self._on_error(exc)
            if text:
                try:
                    result = self._on_transcript(text)
                    if inspect.isawaitable(result):
                        await result
                except Exception as exc:
                    self._on_error(exc)
            # Final meter reset AFTER transcript dispatch so consumers see the
            # dispatch happen before the waveform drops to silence.
            self._send_mic(0.0)

    async def close(self) -> None:
        task = self._task
        if task is not None and not task.done():
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task
        self._task = None


# ---------------------------------------------------------------------------
# Factory
# ---------------------------------------------------------------------------


def build_voice_controller(
    config: VoiceConfig,
    *,
    sample_rate: int = 16000,
    channels: int = 1,
    capture_device: str = "default",
    stt_api_url: str = "",
    stt_model: str = "whisper-1",
    stt_api_key_env: str = "OPENAI_API_KEY",
    stt_timeout_sec: int = 30,
    audio_cues: bool = True,
    max_capture_bytes: int = DEFAULT_MAX_CAPTURE_BYTES,
    chunk_bytes: int = DEFAULT_CHUNK_BYTES,
    send_mic: Callable[[float], None] | None = None,
    on_transcript: Callable[[str], Any] | None = None,
    on_error: Callable[[BaseException], None] | None = None,
) -> VoiceController:
    """Build a controller from a [`VoiceConfig`][lapis_harness.config.VoiceConfig].

    When `stt_api_url` is set the real [`HttpTranscriber`] is used; otherwise a
    fake transcript is returned so the pipeline is end-to-end testable without
    network. Devices default to ALSA `arecord`/`aplay` and swap cleanly in tests.
    """
    local_chunk = chunk_bytes

    if config.enabled or capture_device:

        def source_factory() -> CaptureSource:
            return ArecordSource(capture_device, sample_rate, channels, local_chunk)
    else:

        def source_factory() -> CaptureSource:
            return SyntheticSource(sample_rate, local_chunk)

    if stt_api_url:

        def transcriber_factory() -> Transcriber:
            return HttpTranscriber(
                stt_api_url,
                stt_model,
                sample_rate,
                channels,
                stt_api_key_env,
                stt_timeout_sec,
            )
    else:

        def transcriber_factory() -> Transcriber:
            return FakeTranscriber()

    player = AplayCuePlayer() if audio_cues else NullCuePlayer()

    return VoiceController(
        config,
        source_factory=source_factory,
        transcriber_factory=transcriber_factory,
        player=player,
        send_mic=send_mic,
        on_transcript=on_transcript,
        on_error=on_error,
    )
