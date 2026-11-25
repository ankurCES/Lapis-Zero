"""`python -m lapis_daemon.selfcheck`: run at image build time.

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
    "bluez_peripheral",
    "dbus_next",
)
OWN_MODULES = (
    "lapis_daemon.main",
    "lapis_daemon.api.server",
    "lapis_daemon.llm.anthropic_cua",
    "lapis_daemon.llm.openai_cua",
    "lapis_daemon.kvm.vnc_driver",
    "lapis_daemon.kvm.ssh_driver",
    "lapis_daemon.ble_prov.service",
    "lapis_daemon.ble_prov.gatt_server",
)


def main() -> int:
    problems = []
    for name in (*RUNTIME_MODULES, *OWN_MODULES):
        try:
            importlib.import_module(name)
        except Exception as exc:  # noqa: BLE001 - report every failure, not just the first
            problems.append(f"{name}: {type(exc).__name__}: {exc}")
    if not problems:
        from lapis_daemon.ble_prov import crypto

        crypto.self_test()
    for line in problems:
        print(f"selfcheck FAILED  {line}", file=sys.stderr)
    if problems:
        return 1
    print("lapis-daemon selfcheck OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
