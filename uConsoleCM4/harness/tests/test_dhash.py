from __future__ import annotations

from lapis_harness.agent.dhash import StuckDetector, dhash_from_gray, hamming_distance


def test_gradient_hash_matches_manual_bits() -> None:
    # 9x8 grid where each row alternates left-bright > right-dark in a known pattern
    rows = []
    for _y in range(8):
        row = [200, 100, 200, 100, 200, 100, 200, 100, 200]
        rows.append(row)
    # each row: pairs where left>right = [1,0,1,0,1,0,1,0] -> 0b10101010 per row
    assert dhash_from_gray(rows) == int("10101010" * 8, 2)


def test_hamming() -> None:
    assert hamming_distance(0, 0) == 0
    assert hamming_distance(0b1111, 0b0000) == 4
    assert hamming_distance(0xAA, 0x55) == 8


def test_stuck_detection_and_pause() -> None:
    det = StuckDetector(stuck_threshold=3, stuck_hamming_max=2, max_retries=2)
    # a static hash three times in a row => one detection + corrective hint
    assert det.update(0xDEADBEEF) is None
    assert det.update(0xDEADBEEF) is None
    hint = det.update(0xDEADBEEF)  # third identical frame (distance 0 <= 2)
    assert hint and "3" in hint
    assert det.detections == 1
    assert not det.should_pause

    # the cycle restarts; once detections pass max_retries => pause
    for _ in range(2):  # two more loops of 3 identical frames each
        det.update(0xAAAA)
        det.update(0xAAAA)
        det.update(0xAAAA)
    assert det.detections == 3
    assert det.should_pause is True


def test_stuck_does_not_trigger_on_changing_frames() -> None:
    det = StuckDetector(stuck_threshold=3, stuck_hamming_max=2)
    # large hamming distances never accumulate to a stuck loop
    det.update(0)
    det.update(0xFFFFFFFFFFFFFFFF)
    det.update(0)
    det.update(0xFFFFFFFFFFFFFFFF)
    assert det.detections == 0
