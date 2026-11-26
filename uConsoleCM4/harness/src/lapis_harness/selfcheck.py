"""`python -m lapis_harness.selfcheck`: run at image build time.

Fails the build, rather than the first boot, when a runtime dependency is
missing from the device's virtualenv or the package itself does not import.
"""

from __future__ import annotations

import importlib
import sys

RUNTIME_MODULES = (
    "aiohttp",
    "cryptography",
    "tomli_w",
    "PIL",
    "pydantic",
    "numpy",
    "asyncvnc",
    "asyncssh",
)
OWN_MODULES = (
    "lapis_harness.main",
    "lapis_harness.api.server",
    "lapis_harness.llm.anthropic_cua",
    "lapis_harness.llm.openai_cua",
    "lapis_harness.kvm.vnc_driver",
    "lapis_harness.kvm.ssh_driver",
    "lapis_harness.gate",
    "lapis_harness.remote",
    "lapis_harness.mcp",
)


def main() -> int:
    problems = []
    for name in (*RUNTIME_MODULES, *OWN_MODULES):
        try:
            importlib.import_module(name)
        except Exception as exc:  # noqa: BLE001 - report every failure, not just the first
            problems.append(f"{name}: {type(exc).__name__}: {exc}")
    for line in problems:
        print(f"selfcheck FAILED  {line}", file=sys.stderr)
    if problems:
        return 1
    print("lapis-harness selfcheck OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
