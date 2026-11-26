import json
from pathlib import Path

from lapis_harness.redact import redact, redact_json, register_secret

VECTORS = Path(__file__).resolve().parents[2] / "tests" / "vectors"


def test_redaction_vectors_match_rust() -> None:
    data = json.loads((VECTORS / "redaction_vectors.json").read_text())
    for case in data["text_cases"]:
        assert redact(case["input"]) == case["expected"], case["input"]
    for case in data["json_cases"]:
        assert redact_json(case["input"]) == case["expected"], case["input"]


def test_known_secret_values() -> None:
    register_secret("tiny")
    register_secret("value-from-secrets-env-0001")
    assert redact("tiny") == "tiny"
    assert redact("a value-from-secrets-env-0001 b") == "a [REDACTED] b"
