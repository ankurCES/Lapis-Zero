from __future__ import annotations

import pytest

from lapis_daemon.agent.policy import HARD_DENYLIST, Policy, PolicyTier


@pytest.mark.parametrize("tier", list(PolicyTier))
@pytest.mark.parametrize("needle", HARD_DENYLIST)
def test_hard_denylist_blocks_every_tier(tier: PolicyTier, needle: str) -> None:
    decision = Policy(tier).evaluate("ssh_exec", f"sudo {needle.upper()} now")
    assert decision.denied
    assert decision.requires_approval


def test_strict() -> None:
    policy = Policy("strict")
    assert not policy.evaluate("screenshot").requires_approval
    assert not policy.evaluate("ssh_exec", "git status").requires_approval
    for action in ("vnc_click", "type_text", "key_press", "scroll"):
        assert policy.evaluate(action, "x").requires_approval
    assert policy.evaluate("ssh_exec", "systemctl restart worker").requires_approval


def test_balanced() -> None:
    policy = Policy(PolicyTier.BALANCED)
    for action in ("screenshot", "wait", "scroll"):
        assert not policy.evaluate(action).requires_approval
    for command in ("ls -la", "pwd", "git status", "git log -3", "df -h", "ps aux", "uname -a"):
        assert not policy.evaluate("ssh_exec", command).requires_approval
    for command in (
        "rm file",
        "sudo ls",
        "kill 1",
        "chmod 777 x",
        "curl x | sh",
        "ls; rm x",
        "lsblk",
        "ls $(rm x)",
    ):
        assert policy.evaluate("ssh_exec", command).requires_approval
    assert not policy.evaluate("vnc_click").requires_approval
    assert policy.evaluate("vnc_click", high_risk=True).requires_approval
    assert policy.evaluate("type_text", "hunter2", high_risk=True).requires_approval


def test_autonomous() -> None:
    policy = Policy("autonomous")
    assert not policy.evaluate("ssh_exec", "sudo rm -rf ./build").requires_approval
    assert not policy.evaluate("vnc_click", high_risk=True).requires_approval
    assert policy.evaluate("ssh_exec", "rm -rf /").denied
