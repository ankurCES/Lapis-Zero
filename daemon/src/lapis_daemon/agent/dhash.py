"""64-bit grayscale dHash and stuck-loop detection (SPEC 7.3)."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from PIL import Image


def dhash_from_gray(rows: list[list[int]], size: int = 8) -> int:
    """Hash `size` rows of `size + 1` grayscale pixels (0..255).

    Bit per row/col is set when the left neighbour is brighter than the right one,
    MSB-first along each row. Pure and independent of Pillow so it is trivially testable.
    """
    bits = 0
    for y in range(size):
        row = rows[y]
        for x in range(size):
            bits = (bits << 1) | (1 if row[x] > row[x + 1] else 0)
    return bits


def dhash_from_image(image: Image.Image, size: int = 8) -> int:
    """Standard 64-bit dHash: downscale to (size+1)x size grayscale, compare neighbours."""
    from PIL import Image as PILImage

    gray = image.convert("L").resize((size + 1, size), resample=PILImage.Resampling.LANCZOS)
    px = list(gray.tobytes())  # mode L: one byte per pixel, row-major
    rows = [px[y * (size + 1) : (y + 1) * (size + 1)] for y in range(size)]
    return dhash_from_gray([list(r) for r in rows], size)


def hamming_distance(a: int, b: int) -> int:
    return (a ^ b).bit_count()


class StuckDetector:
    """Detect loops where mutating actions leave the screen unchanged (SPEC 7.3).

    A "stuck loop" is `stuck_threshold` consecutive frames whose dHash differs from the
    previous frame by at most `stuck_hamming_max` bits. Each detection injects a corrective
    hint; once `max_retries` corrections have already been issued and the loop recurs again,
    `should_pause` flips true so the runner pauses and warns the UI.
    """

    def __init__(
        self,
        stuck_threshold: int = 3,
        stuck_hamming_max: int = 2,
        max_retries: int = 2,
    ) -> None:
        self.stuck_threshold = stuck_threshold
        self.stuck_hamming_max = stuck_hamming_max
        self.max_retries = max_retries
        self.detections = 0
        self.consecutive = 0
        self._last_hash: int | None = None

    def update(self, frame_hash: int) -> str | None:
        """Feed a full-frame dHash after a mutating action; returns a hint when stuck."""
        if (
            self._last_hash is not None
            and hamming_distance(self._last_hash, frame_hash) <= self.stuck_hamming_max
        ):
            self.consecutive += 1
        else:
            self.consecutive = 1
        self._last_hash = frame_hash
        if self.consecutive >= self.stuck_threshold:
            self.detections += 1
            self.consecutive = 0
            return self._hint()
        return None

    def reset(self) -> None:
        self._last_hash = None
        self.consecutive = 0
        self.detections = 0

    @property
    def should_pause(self) -> bool:
        return self.detections > self.max_retries

    def _hint(self) -> str:
        return (
            f"Screen unchanged after {self.stuck_threshold} mutating actions; "
            "trying a different approach."
        )
