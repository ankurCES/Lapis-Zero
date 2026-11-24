"""Builders for the `state_sync` IPC message (SPEC 6.1)."""

from __future__ import annotations

from typing import Any

from lapis_daemon.config import HostsConfig, mock_hosts
from lapis_daemon.db import LapisDb


def build_state_sync(
    db: LapisDb | None,  # reserved: real status probes and schedules land in later phases
    hosts: HostsConfig,
    agent_state: str,
    active_host: str,
    active_task: dict[str, Any] | None,
    wifi_ssid: str,
    local_ip: str,
    tailscale_state: str,
    tailscale_ip: str,
    tailscale_auth_url: str | None,
    api_key_ready: bool,
    next_cron: str,
) -> dict[str, Any]:
    return {
        "type": "state_sync",
        "agent_state": agent_state,
        "active_host": active_host,
        "hosts": list(hosts.hosts),
        "active_task": active_task,
        "next_cron": next_cron,
        "wifi_ssid": wifi_ssid,
        "local_ip": local_ip,
        "tailscale_state": tailscale_state,
        "tailscale_ip": tailscale_ip,
        "tailscale_auth_url": tailscale_auth_url,
        "api_key_ready": api_key_ready,
    }


def mock_state() -> dict[str, Any]:
    return build_state_sync(
        None,
        mock_hosts(),
        agent_state="IDLE",
        active_host="macbook-pro",
        active_task=None,
        wifi_ssid="HomeWiFi",
        local_ip="192.168.1.42",
        tailscale_state="connected",
        tailscale_ip="100.91.12.4",
        tailscale_auth_url=None,
        api_key_ready=True,
        next_cron="08:00 Daily Digest",
    )
