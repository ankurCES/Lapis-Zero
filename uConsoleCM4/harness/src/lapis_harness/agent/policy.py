"""Approval policy tiers and the hard denylist (SPEC 7.3)."""

from __future__ import annotations

from enum import StrEnum
from typing import NamedTuple


class PolicyTier(StrEnum):
    STRICT = "strict"
    BALANCED = "balanced"
    AUTONOMOUS = "autonomous"


HARD_DENYLIST = ("rm -rf /", "mkfs", "dd if=", ":(){", "shutdown", "reboot")
READ_ONLY_ACTIONS = ("screenshot", "wait", "finish")
BALANCED_AUTO_ACTIONS = (*READ_ONLY_ACTIONS, "scroll")
SSH_READ_ALLOWLIST = ("ls", "pwd", "git status", "git log", "df", "ps", "uname")
# Anything that can chain, substitute, or redirect disqualifies a "read-only" command.
SHELL_META = set(";|&><`$\n")


class Decision(NamedTuple):
    requires_approval: bool
    reason: str
    denied: bool = False  # hard denylist: never run, not even with approval


def is_read_only_ssh(command: str) -> bool:
    cmd = command.strip()
    if SHELL_META & set(cmd):
        return False
    return any(cmd == allowed or cmd.startswith(allowed + " ") for allowed in SSH_READ_ALLOWLIST)


class Policy:
    def __init__(self, tier: PolicyTier | str) -> None:
        self.tier = PolicyTier(tier)

    def evaluate(
        self, action: str, command: str | None = None, *, high_risk: bool = False
    ) -> Decision:
        """Decide whether `action` (with its SSH command or typed text) may auto-run.

        `high_risk` flags secret entry and risky UI clicks, which the action name alone
        cannot reveal.
        """
        text = (command or "").lower()
        for needle in HARD_DENYLIST:
            if needle in text:
                return Decision(True, f"hard denylist: {needle}", denied=True)
        if self.tier is PolicyTier.AUTONOMOUS:
            return Decision(False, "autonomous tier")
        if action == "ssh_exec":
            if is_read_only_ssh(command or ""):
                return Decision(False, "read-only ssh allowlist")
            return Decision(True, "ssh command outside read-only allowlist")
        if self.tier is PolicyTier.STRICT:
            if action in READ_ONLY_ACTIONS:
                return Decision(False, "read-only action")
            return Decision(True, "strict tier: mutating action")
        if action in BALANCED_AUTO_ACTIONS:
            return Decision(False, "read-only action")
        if high_risk:
            return Decision(True, "high-risk action")
        return Decision(False, "balanced tier: low-risk action")


# tool name -> (activity kind shown in the ticker, approval badge action)
KINDS = {
    "screenshot": ("screenshot", "vnc_click"),
    "mouse_click": ("vnc_click", "vnc_click"),
    "mouse_move": ("vnc_move", "vnc_click"),
    "mouse_drag": ("vnc_drag", "vnc_click"),
    "scroll": ("vnc_scroll", "vnc_click"),
    "type_text": ("type_text", "vnc_type"),
    "key_press": ("vnc_key", "vnc_key"),
    "ssh_exec": ("ssh_exec", "ssh_exec"),
    "wait": ("wait", "wait"),
    "agent_run": ("agent_run", "agent_run"),
}
