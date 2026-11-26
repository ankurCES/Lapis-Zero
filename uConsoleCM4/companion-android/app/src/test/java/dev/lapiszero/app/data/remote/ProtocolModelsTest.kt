package dev.lapiszero.app.data.remote

import dev.lapiszero.app.Fixtures
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Every object of ANDROID_COMPANION_SPEC.md 5.4, decoded from JSON the mock daemon really sends. */
class ProtocolModelsTest {
    private val json = LapisJson

    private inline fun <reified T> decode(text: String): T = json.decodeFromString(text)

    private fun encodeToObject(text: String): JsonObject = json.parseToJsonElement(text).jsonObject

    // ---------------------------------------------------------------- objects

    @Test
    fun `health`() {
        val h: Health = decode(
            """{"ok": true, "version": "0.2.0", "hostname": "lapis-zero", "uptime_sec": 5, "mock": true, "device_id": "dev_local"}""",
        )
        assertTrue(h.ok)
        assertEquals("0.2.0", h.version)
        assertEquals("lapis-zero", h.hostname)
        assertEquals(5, h.uptimeSec)
        assertTrue(h.mock)
        assertEquals("dev_local", h.deviceId)
    }

    @Test
    fun `pair exchange request and response`() {
        val body = encodeToObject(json.encodeToString(PairExchangeRequest("n0nce", "Pixel", "Google Pixel 9")))
        assertEquals(setOf("nonce", "device_name", "device_model"), body.keys)
        assertEquals("Google Pixel 9", body.getValue("device_model").jsonPrimitive.content)

        val r: PairExchangeResponse = decode(
            """{"device_id": "dev_1a2b3c4d", "api_token": "s3cr3t-token", "hostname": "lapis-zero"}""",
        )
        assertEquals("dev_1a2b3c4d", r.deviceId)
        assertEquals("s3cr3t-token", r.apiToken)
        assertEquals("lapis-zero", r.hostname)
        assertFalse("token must not leak through toString", r.toString().contains("s3cr3t"))
    }

    @Test
    fun `paired devices list`() {
        val r: DevicesResponse = decode(
            """{"devices": [
                {"id": "dev_local", "name": "Dev token", "model": "dev", "created_at": 1795541234,
                 "last_seen_at": 1795541240, "revoked": false, "current": true},
                {"id": "dev_2", "name": "Tablet", "model": "", "created_at": 1795541000.75,
                 "last_seen_at": null, "revoked": true, "current": false}]}""",
        )
        assertEquals(2, r.devices.size)
        assertTrue(r.devices[0].current)
        assertEquals(1795541240L, r.devices[0].lastSeenAt)
        // A float timestamp from an older daemon is truncated, a null one stays null.
        assertEquals(1795541000L, r.devices[1].createdAt)
        assertNull(r.devices[1].lastSeenAt)
        assertTrue(r.devices[1].revoked)
    }

    @Test
    fun `chat with and without live fields`() {
        val full: Chat = decode(
            Fixtures.chat(lastMessage = Fixtures.assistantMessage()).replace("\"active\": false, \"step\": null, \"pending_approvals\": 0", "\"active\": true, \"step\": 3, \"pending_approvals\": 1"),
        )
        assertEquals("c_9e838c02", full.id)
        assertEquals("macbook-pro", full.hostId)
        assertNull(full.modelOverride)
        assertNull(full.policyOverride)
        assertFalse(full.archived)
        assertEquals(1795541257L, full.createdAt)
        assertEquals(1795541264L, full.updatedAt)
        assertEquals("assistant", full.lastMessage?.role)
        assertTrue(full.active)
        assertEquals(3, full.step)
        assertEquals(1, full.pendingApprovals)

        // `chat_updated` events omit active / step / pending_approvals entirely.
        val bare: Chat = decode(Fixtures.chat(live = false))
        assertFalse(bare.active)
        assertNull(bare.step)
        assertEquals(0, bare.pendingApprovals)
        assertNull(bare.lastMessage)

        val overridden: Chat = decode(
            """{"id": "c_1", "title": "t", "host_id": "arch-dev", "model_override": "claude-x",
                "policy_override": "autonomous", "archived": true, "created_at": 1, "updated_at": 2,
                "last_message": null, "active": false, "step": null, "pending_approvals": 0}""",
        )
        assertEquals("claude-x", overridden.modelOverride)
        assertEquals("autonomous", overridden.policyOverride)
        assertTrue(overridden.archived)
    }

    @Test
    fun `messages of every role and their metadata`() {
        val user: Message = decode(Fixtures.userMessage(taskId = "t_855667000"))
        assertEquals(1L, user.id)
        assertEquals(Message.ROLE_USER, user.role)
        assertEquals("android:Pixel", user.metadata.source)
        assertEquals("t_855667000", user.taskId)
        assertNull(user.metadata.kind)

        val tool: Message = decode(Fixtures.toolMessage())
        assertEquals(Message.ROLE_TOOL, tool.role)
        assertEquals("vnc_click", tool.metadata.kind)
        assertEquals("ok", tool.metadata.status)
        assertEquals("Click header", tool.metadata.detail)
        assertEquals(1L, tool.metadata.screenshotId)
        assertEquals(2, tool.metadata.step)
        assertEquals(1795541258L, tool.metadata.ts)

        val toolNoShot: Message = decode(Fixtures.toolMessage(screenshotId = null))
        assertNull(toolNoShot.metadata.screenshotId)

        val assistant: Message = decode(Fixtures.assistantMessage())
        assertEquals(Message.ROLE_ASSISTANT, assistant.role)
        assertEquals("done", assistant.metadata.status)

        val system: Message = decode(
            """{"id": 9, "chat_id": "c", "role": "system", "content": "note", "task_id": null, "metadata": {}, "created_at": 3}""",
        )
        assertEquals(Message.ROLE_SYSTEM, system.role)
        assertNull(system.taskId)
        assertEquals(MessageMetadata(), system.metadata)
    }

    @Test
    fun `chat detail and send message responses`() {
        val detail: ChatDetailResponse = decode(
            """{"chat": ${Fixtures.chat()}, "messages": [${Fixtures.userMessage()}, ${Fixtures.toolMessage()}, ${Fixtures.assistantMessage()}], "has_more": true}""",
        )
        assertEquals(listOf(1L, 3L, 8L), detail.messages.map { it.id })
        assertTrue(detail.hasMore)

        val sent: SendMessageResponse = decode(
            """{"message": ${Fixtures.userMessage(taskId = "t_1")}, "task_id": "t_1"}""",
        )
        assertEquals("t_1", sent.taskId)
        assertEquals("open settings", sent.message.content)
        assertTrue(decode<StopResponse>("""{"stopped": true}""").stopped)
        assertEquals(1, decode<ChatsResponse>("""{"chats": [${Fixtures.chat()}]}""").chats.size)
    }

    @Test
    fun `approval for a click and for a shell command`() {
        val click: Approval = decode(Fixtures.approval(preview = "AAAA"))
        assertEquals("req_1", click.requestId)
        assertEquals("macbook-pro", click.host)
        assertEquals("vnc_click", click.action)
        assertEquals("high", click.risk)
        assertEquals("AAAA", click.preview1BitB64)
        assertEquals("t_855667000", click.taskId)
        assertEquals("c_9e838c02", click.chatId)
        assertEquals(5, click.step)
        assertEquals(2L, click.screenshotId)
        assertEquals(840, click.clickX)
        assertEquals(412, click.clickY)

        val ssh: Approval = decode(Fixtures.approval(requestId = "req_2", action = "ssh_exec"))
        assertNull(ssh.preview1BitB64)
        assertNull(ssh.clickX)
        assertNull(ssh.clickY)

        // chat_id and screenshot_id may be null (task started from the device, no frame yet).
        val bare: Approval = decode(
            """{"type": "approval_request", "request_id": "req_3", "host": "h", "action": "vnc_key",
                "summary": "s", "risk": "low", "preview_1bit_120x68_b64": null, "task_id": "t",
                "chat_id": null, "step": 1, "screenshot_id": null, "click_x": null, "click_y": null}""",
        )
        assertNull(bare.chatId)
        assertNull(bare.screenshotId)
        assertEquals("low", bare.risk)
        assertEquals(1, decode<ApprovalsResponse>("""{"approvals": [${Fixtures.approval()}]}""").approvals.size)
    }

    @Test
    fun `approval decision request omits an empty reason`() {
        val plain = encodeToObject(json.encodeToString(ApprovalDecisionRequest(Decision.APPROVE_ONCE)))
        assertEquals(mapOf("decision" to JsonPrimitive("approve_once")), plain.toMap())
        val deny = encodeToObject(json.encodeToString(ApprovalDecisionRequest(Decision.DENY, "wrong button")))
        assertEquals("wrong button", deny.getValue("reason").jsonPrimitive.content)
        assertTrue(decode<OkResponse>("""{"ok": true}""").ok)
    }

    @Test
    fun `activity event`() {
        val event = LapisEvent.parse(Fixtures.activity()) as LapisEvent.ActivityEvent
        val a = event.activity
        assertEquals(1795541258L, a.ts)
        assertEquals("t_855667000", a.taskId)
        assertEquals("vnc_click", a.kind)
        assertEquals("click(160, 24) left", a.summary)
        assertEquals("Click header", a.detail)
        assertEquals("ok", a.status)
        assertEquals("c_9e838c02", a.chatId)
        assertEquals(2, a.step)
        assertEquals(1L, a.screenshotId)
        val noShot = LapisEvent.parse(Fixtures.activity(kind = "ssh_exec", summary = "uname -a", screenshotId = null)) as LapisEvent.ActivityEvent
        assertNull(noShot.activity.screenshotId)
    }

    @Test
    fun `state sync idle and running with approvals`() {
        val idle: DeviceState = decode(Fixtures.stateSync())
        assertEquals(AgentState.IDLE, idle.agentState)
        assertEquals("macbook-pro", idle.activeHost)
        assertEquals(listOf("macbook-pro", "arch-dev"), idle.hosts)
        assertNull(idle.activeTask)
        assertNull(idle.activeChatId)
        assertEquals("", idle.nextCron)
        assertEquals("HomeWiFi", idle.wifiSsid)
        assertEquals("192.168.1.42", idle.localIp)
        assertEquals("connected", idle.tailscaleState)
        assertEquals("100.91.12.4", idle.tailscaleIp)
        assertEquals("lapis-zero.tailnet.ts.net", idle.tailscaleFqdn)
        assertNull(idle.tailscaleAuthUrl)
        assertTrue(idle.apiKeyReady)
        assertEquals(84, idle.batteryPct)
        assertFalse(idle.batteryCharging)
        assertTrue(idle.pendingApprovals.isEmpty())
        assertNull(idle.pairing)
        assertEquals(UiSettings("paper", 100, false, ""), idle.ui)
        assertEquals(DeviceInfo("lapis-zero", "0.2.0", true), idle.device)
        assertFalse(idle.isRunning)
        assertFalse(idle.canNudge)

        val waiting: DeviceState = decode(
            Fixtures.stateSync("WAITING_APPROVAL", activeChatId = "c_9e838c02", step = 5, approvals = Fixtures.approval()),
        )
        assertTrue(waiting.isRunning)
        assertTrue(waiting.canNudge)
        assertEquals(ActiveTask("t_855667000", "open settings", 5, 25), waiting.activeTask)
        assertEquals("c_9e838c02", waiting.activeChatId)
        assertEquals("req_1", waiting.pendingApprovals.single().requestId)

        assertTrue(decode<DeviceState>(Fixtures.stateSync("PAUSED", "c")).canNudge)
        assertFalse(decode<DeviceState>(Fixtures.stateSync("ACTING", "c")).canNudge)
        assertFalse(decode<DeviceState>(Fixtures.stateSync("THINKING", "c")).canNudge)

        // No battery fitted, login needed.
        val odd: DeviceState = decode(
            Fixtures.stateSync().replace("\"battery_pct\": 84", "\"battery_pct\": null")
                .replace("\"tailscale_auth_url\": null", "\"tailscale_auth_url\": \"https://login.tailscale.com/a/abc\"")
                .replace("\"tailscale_state\": \"connected\"", "\"tailscale_state\": \"needs_login\""),
        )
        assertNull(odd.batteryPct)
        assertEquals("https://login.tailscale.com/a/abc", odd.tailscaleAuthUrl)
        assertEquals("needs_login", odd.tailscaleState)
    }

    @Test
    fun `pairing in all three modes`() {
        val qr: Pairing = decode(
            """{"type": "pairing", "mode": "qr", "payload": "lapis://pair?host=127.0.0.1:18431&nonce=GneYMjL9l4v_GZvPP4A8TukmQs978gPNuFg-W3Q3ZGc&pin=114773&tls=0",
                "host": "127.0.0.1:18431", "pin": "114773", "expires_in": 180, "expires_at": 1795541453}""",
        )
        assertEquals("qr", qr.mode)
        assertTrue(qr.isActive)
        assertEquals("114773", qr.pin)
        assertEquals(180, qr.expiresIn)
        assertEquals(1795541453L, qr.expiresAt)
        assertTrue(qr.payload!!.startsWith("lapis://pair?"))

        val pin: Pairing = decode(
            """{"type": "pairing", "mode": "pin", "pin": "560443", "device_name": "LapisZero-MOCK",
                "status": "waiting", "expires_in": 300, "expires_at": 1795541573}""",
        )
        assertEquals("pin", pin.mode)
        assertEquals("LapisZero-MOCK", pin.deviceName)
        assertEquals("waiting", pin.status)
        assertNull(pin.payload)

        val off: Pairing = decode("""{"type": "pairing", "mode": "off"}""")
        assertFalse(off.isActive)
        assertNull(off.pin)
    }

    @Test
    fun `hosts round trip keeps every field`() {
        val text = """{"hosts": {
            "macbook-pro": {"hostname": "macbook-pro", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
              "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_macbook_pro"},
              "ssh": {"port": 22, "user": "alice", "identity_file": null}},
            "arch-dev": {"hostname": "arch-dev", "tailscale_ip": "100.64.20.42", "os": "linux", "policy": "strict",
              "vnc": {"port": 5901, "username": "dev", "password_env": "LAPIS_VNC_PASSWORD_arch_dev"},
              "ssh": {"port": 2222, "user": "dev", "identity_file": "/etc/lapis/lapis_ed25519"}}},
            "active_host": "macbook-pro"}"""
        val r: HostsResponse = decode(text)
        assertEquals("macbook-pro", r.activeHost)
        assertEquals(listOf("macbook-pro", "arch-dev"), r.hosts.keys.toList())
        val arch = r.hosts.getValue("arch-dev")
        assertEquals("linux", arch.os)
        assertEquals("strict", arch.policy)
        assertEquals(HostVnc(5901, "dev", "LAPIS_VNC_PASSWORD_arch_dev"), arch.vnc)
        assertEquals(HostSsh(2222, "dev", "/etc/lapis/lapis_ed25519"), arch.ssh)
        assertNull(r.hosts.getValue("macbook-pro").ssh.identityFile)

        // PUT body: {"hosts": {id: Host}} with defaults written out explicitly.
        val put = encodeToObject(json.encodeToString(HostsRequest(mapOf("new-box" to Host(hostname = "new-box", tailscaleIp = "100.1.2.3")))))
        val host = put.getValue("hosts").jsonObject.getValue("new-box").jsonObject
        assertEquals("macos", host.getValue("os").jsonPrimitive.content)
        assertEquals("balanced", host.getValue("policy").jsonPrimitive.content)
        assertEquals("5900", host.getValue("vnc").jsonObject.getValue("port").jsonPrimitive.content)
        assertEquals("22", host.getValue("ssh").jsonObject.getValue("port").jsonPrimitive.content)
        assertFalse("null identity_file is omitted", host.getValue("ssh").jsonObject.containsKey("identity_file"))
        assertEquals(r, decode<HostsResponse>(json.encodeToString(r)))
    }

    @Test
    fun `probe result with failures`() {
        val ok: ProbeResult = decode(
            """{"host_id": "macbook-pro", "tailscale": {"ok": true, "detail": "mock", "latency_ms": 12},
                "vnc": {"ok": true, "detail": "mock", "latency_ms": 85}, "ssh": {"ok": true, "detail": "mock", "latency_ms": 140}}""",
        )
        assertEquals("macbook-pro", ok.hostId)
        assertEquals(12, ok.tailscale.latencyMs)
        assertTrue(ok.vnc.ok)
        val bad: ProbeResult = decode(
            """{"host_id": "arch-dev", "tailscale": {"ok": true, "latency_ms": 9, "detail": "pong"},
                "vnc": {"ok": false, "latency_ms": null, "detail": "ConnectionRefusedError: [Errno 111]"},
                "ssh": {"ok": false, "latency_ms": null, "detail": "TimeoutError: "}}""",
        )
        assertFalse(bad.vnc.ok)
        assertNull(bad.vnc.latencyMs)
        assertTrue(bad.ssh.detail.startsWith("TimeoutError"))
    }

    @Test
    fun `ssh password name travels like the vnc one and is left out for older device software`() {
        // Device software with the SSH features: the NAME of a vault secret, or "" for key only.
        val newer: HostsResponse = decode(
            """{"hosts": {
                "mac": {"hostname": "mac", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
                  "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_mac"},
                  "ssh": {"port": 22, "user": "alice", "identity_file": null, "password_env": "LAPIS_VNC_PASSWORD_mac"}},
                "box": {"hostname": "box", "tailscale_ip": "100.64.20.42", "os": "linux", "policy": "strict",
                  "vnc": {"port": 5901, "username": "", "password_env": ""},
                  "ssh": {"port": 22, "user": "dev", "identity_file": null, "password_env": ""}}},
                "active_host": "mac"}""",
        )
        assertEquals("LAPIS_VNC_PASSWORD_mac", newer.hosts.getValue("mac").ssh.passwordEnv)
        assertEquals("", newer.hosts.getValue("box").ssh.passwordEnv)
        val sent = encodeToObject(json.encodeToString(HostsRequest(newer.hosts))).getValue("hosts").jsonObject
        assertEquals("LAPIS_VNC_PASSWORD_mac", sent.getValue("mac").jsonObject.getValue("ssh").jsonObject.getValue("password_env").jsonPrimitive.content)
        assertEquals("", sent.getValue("box").jsonObject.getValue("ssh").jsonObject.getValue("password_env").jsonPrimitive.content)

        // Older software does not send the field, and does not get it back.
        val older: Host = decode(
            """{"hostname": "mac", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
                "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_mac"},
                "ssh": {"port": 22, "user": "alice", "identity_file": null}}""",
        )
        assertNull(older.ssh.passwordEnv)
        val back = encodeToObject(json.encodeToString(older)).getValue("ssh").jsonObject
        assertEquals(setOf("port", "user"), back.keys)
    }

    @Test
    fun `device key authorise answer and agent tools`() {
        val key: DeviceSshKey = decode(Fixtures.SSH_KEY)
        assertEquals(Fixtures.PUBLIC_KEY, key.publicKey)
        assertEquals(Fixtures.FINGERPRINT, key.fingerprint)

        val done: SshAuthorizeResult = decode("""{"ok": true, "already": false, "detail": "key added for alice"}""")
        assertTrue(done.ok)
        assertFalse(done.already)
        assertEquals("key added for alice", done.detail)
        assertTrue(decode<SshAuthorizeResult>("""{"ok": true, "already": true, "detail": "already authorised"}""").already)

        val agents: HostAgentsResponse = decode(Fixtures.AGENTS)
        assertEquals(listOf("claude", "opencode", "freya"), agents.agents.map { it.id })
        assertEquals(HostAgent("claude", "Claude Code", "/opt/homebrew/bin/claude"), agents.agents.first())
        assertTrue(decode<HostAgentsResponse>("{}").agents.isEmpty())

        // The probe lists them too, and says how the SSH login worked.
        val probe: ProbeResult = decode(
            """{"host_id": "mac", "tailscale": {"ok": true, "latency_ms": 12, "detail": "pong"},
                "vnc": {"ok": true, "latency_ms": 85, "detail": "ok"}, "ssh": {"ok": true, "latency_ms": 140, "detail": "ok (device key)"},
                "agents": [{"id": "claude", "name": "Claude Code", "path": "/opt/homebrew/bin/claude"}]}""",
        )
        assertEquals("ok (device key)", probe.ssh.detail)
        assertEquals("Claude Code", probe.agents.single().name)
        // Older software: no list at all.
        assertTrue(decode<ProbeResult>("""{"host_id": "mac"}""").agents.isEmpty())
    }

    @Test
    fun `a chat says who answers in it and where`() {
        val tool: Chat = decode(Fixtures.chat(agent = "claude", workdir = "~/projects/lapis"))
        assertEquals("claude", tool.agent)
        assertEquals("~/projects/lapis", tool.workdir)
        // Older device software sends neither: the device's own agent, in the home folder.
        val older: Chat = decode(Fixtures.chat())
        assertEquals(Chat.AGENT_LAPIS, older.agent)
        assertEquals("lapis", older.agent)
        assertEquals("", older.workdir)
        // `chat_updated` carries the chat too.
        val event = LapisEvent.parse("""{"type": "chat_updated", "chat": ${Fixtures.chat(live = false, agent = "freya")}}""") as LapisEvent.ChatUpdated
        assertEquals("freya", event.chat.agent)
        // Creating a chat sends nothing new: it is a chat of the device's own agent everywhere.
        assertEquals(setOf("host_id"), encodeToObject(json.encodeToString(CreateChatRequest("mac"))).keys)
        assertEquals("agent_run", Activity.KIND_AGENT_RUN)
    }

    @Test
    fun `config stays a raw object so unknown sections survive`() {
        val text = """{"config": {"api": {"enabled": true, "port": 8421, "frames_cap_mb": 25},
            "llm": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key_env": "ANTHROPIC_API_KEY",
                    "effort": "medium", "base_url": "", "max_steps": 25, "request_timeout_sec": 120},
            "future_section": {"knob": [1, 2, 3]}}}"""
        val env: ConfigEnvelope = decode(text)
        assertEquals("anthropic", env.config.getValue("llm").jsonObject.getValue("provider").jsonPrimitive.content)
        val back = encodeToObject(json.encodeToString(env)).getValue("config").jsonObject
        assertEquals(3, back.getValue("future_section").jsonObject.getValue("knob").jsonArray.size)
        assertEquals(env.config, back)
    }

    @Test
    fun `secrets are status only`() {
        val r: SecretsResponse = decode(
            """{"secrets": [{"name": "ANTHROPIC_API_KEY", "is_set": true, "masked": "...a9f2"},
                            {"name": "OPENAI_API_KEY", "is_set": false, "masked": null}]}""",
        )
        assertTrue(r.secrets[0].isSet)
        assertEquals("...a9f2", r.secrets[0].masked)
        assertFalse(r.secrets[1].isSet)
        assertNull(r.secrets[1].masked)

        val patch = SecretsPatch(set = mapOf("ANTHROPIC_API_KEY" to "sk-ant-very-secret"), unset = listOf("OLD"))
        val body = encodeToObject(json.encodeToString(patch))
        assertEquals("sk-ant-very-secret", body.getValue("set").jsonObject.getValue("ANTHROPIC_API_KEY").jsonPrimitive.content)
        assertEquals("OLD", body.getValue("unset").jsonArray.single().jsonPrimitive.content)
        assertFalse("values must not leak through toString", patch.toString().contains("very-secret"))
    }

    @Test
    fun `schedules`() {
        val r: SchedulesResponse = decode(
            """{"schedules": [
                {"name": "Daily Digest", "schedule": "0 8 * * *", "host": "macbook-pro",
                 "prompt": "Check overnight CI runs and summarize any failures.", "enabled": false,
                 "next_run": null, "last_run_at": null, "last_chat_id": null},
                {"name": "Hourly", "schedule": "0 * * * *", "host": "arch-dev", "prompt": "p", "enabled": true,
                 "next_run": 1795543200, "last_run_at": 1795539600, "last_chat_id": "c_1"}]}""",
        )
        assertEquals("Daily Digest", r.schedules[0].name)
        assertNull(r.schedules[0].nextRun)
        assertNull(r.schedules[0].lastChatId)
        assertEquals(1795543200L, r.schedules[1].nextRun)
        assertEquals(1795539600L, r.schedules[1].lastRunAt)
        assertEquals("c_1", r.schedules[1].lastChatId)

        val create = encodeToObject(json.encodeToString(ScheduleCreate("n", "0 8 * * *", "macbook-pro", "p", true)))
        assertEquals(setOf("name", "schedule", "host", "prompt", "enabled"), create.keys)
        // A partial PATCH carries only the changed field.
        assertEquals(setOf("enabled"), encodeToObject(json.encodeToString(SchedulePatch(enabled = false))).keys)
        val run: ScheduleRunResponse = decode("""{"chat_id": "c_7", "task_id": "t_7"}""")
        assertEquals("c_7", run.chatId)
        assertEquals("t_7", run.taskId)
    }

    @Test
    fun `telemetry with and without optional readings`() {
        val t: Telemetry = decode(Fixtures.TELEMETRY)
        assertEquals(84, t.battery?.percent)
        assertEquals(3.9, t.battery!!.voltageV, 1e-9)
        assertFalse(t.battery.charging)
        assertEquals(47.5, t.cpu.tempC!!, 1e-9)
        assertEquals(0.32, t.cpu.load1!!, 1e-9)
        assertEquals(496.0, t.memory.totalMb, 1e-9)
        assertEquals(301.0, t.memory.availableMb, 1e-9)
        assertEquals(61.0, t.memory.daemonRssMb, 1e-9)
        assertEquals(6.0, t.memory.uiRssMb, 1e-9)
        assertEquals(195.0, t.memory.usedMb, 1e-9)
        assertFalse(t.memory.overBudget)
        assertEquals("HomeWiFi", t.wifi.ssid)
        assertEquals(-58, t.wifi.rssiDbm)
        assertEquals("connected", t.tailscale.state)
        assertEquals(3, t.tailscale.peers)
        assertEquals(9, t.uptimeSec)
        assertEquals("active", t.services["lapis-ui"])
        assertEquals(5, t.services.size)

        val bare: Telemetry = decode(
            """{"battery": null, "cpu": {"temp_c": null, "load1": null},
                "memory": {"total_mb": 496, "available_mb": 250.5, "zram_total_mb": 496, "zram_used_mb": 40,
                           "daemon_rss_mb": 88.4, "ui_rss_mb": 12},
                "wifi": {"ssid": "", "ip": "", "rssi_dbm": null},
                "tailscale": {"state": "needs_login", "ip": "", "fqdn": "", "peers": 0},
                "uptime_sec": 12, "services": {}}""",
        )
        assertNull(bare.battery)
        assertNull(bare.cpu.tempC)
        assertNull(bare.wifi.rssiDbm)
        assertEquals(245.5, bare.memory.usedMb, 1e-9)
        assertTrue("above the 220 MB budget", bare.memory.overBudget)
    }

    @Test
    fun `device ui patch and power bodies`() {
        assertEquals(setOf("brightness"), encodeToObject(json.encodeToString(UiPatch(brightness = 40))).keys)
        assertEquals(setOf("theme", "wake"), encodeToObject(json.encodeToString(UiPatch(theme = "dark", wake = true))).keys)
        assertEquals(UiSettings("dark", 40, true), decode<UiSettings>("""{"theme": "dark", "brightness": 40, "mute_audio": true}"""))
        assertEquals("""{"action":"start_qr_pairing"}""", json.encodeToString(PowerRequest(PowerAction.START_QR_PAIRING)))
        val power: PowerResponse = decode("""{"ok": true, "action": "reboot"}""")
        assertTrue(power.ok)
        assertEquals("reboot", power.action)
        assertEquals(
            setOf("action", "x", "y"),
            encodeToObject(json.encodeToString(NudgeRequest(action = "click", x = 3, y = 4))).keys,
        )
        assertEquals(setOf("action", "combo"), encodeToObject(json.encodeToString(NudgeRequest(action = "key", combo = "ctrl+c"))).keys)
    }

    @Test
    fun `error envelope`() {
        val e: ApiErrorEnvelope = decode("""{"error": {"code": "busy", "message": "Agent busy: a task is already running"}}""")
        assertEquals("busy", e.error?.code)
        assertEquals("Agent busy: a task is already running", e.error?.message)
    }

    @Test
    fun `timezone fields are additive`() {
        // A 0.3 daemon reports the clock's zone and offset...
        val state: DeviceState = decode(
            Fixtures.stateSync()
                .replace("\"battery_charging\": false,", "\"battery_charging\": false, \"utc_offset_min\": -480,")
                .replace("\"mute_audio\": false}", "\"mute_audio\": false, \"timezone\": \"America/Los_Angeles\"}"),
        )
        assertEquals(-480, state.utcOffsetMin)
        assertEquals("America/Los_Angeles", state.ui.timezone)
        // ...an older one simply has neither, and an unset zone is the empty string.
        val old: DeviceState = decode(Fixtures.stateSync())
        assertNull(old.utcOffsetMin)
        assertEquals("", old.ui.timezone)
        assertEquals("", decode<UiSettings>("""{"theme": "dark", "brightness": 40, "mute_audio": true, "timezone": ""}""").timezone)
        assertEquals(
            UiSettings("paper", 100, false, "Europe/Berlin"),
            decode<UiSettings>("""{"theme": "paper", "brightness": 100, "mute_audio": false, "timezone": "Europe/Berlin"}"""),
        )
        // The PATCH carries only the zone when only the zone changes.
        assertEquals("""{"timezone":"Asia/Kolkata"}""", json.encodeToString(UiPatch(timezone = "Asia/Kolkata")))
        assertFalse(json.encodeToString(UiPatch(brightness = 10)).contains("timezone"))
    }

    // ---------------------------------------------------------------- tolerance

    @Test
    fun `unknown fields are ignored everywhere`() {
        val state: DeviceState = decode(
            Fixtures.stateSync().replace("\"type\": \"state_sync\",", "\"type\": \"state_sync\", \"new_top_level\": {\"a\": [1]},")
                .replace("\"mute_audio\": false", "\"mute_audio\": false, \"contrast\": 7"),
        )
        assertEquals("paper", state.ui.theme)
        val chat: Chat = decode(Fixtures.chat().replace("\"archived\": false", "\"archived\": false, \"pinned\": true"))
        assertEquals("open settings", chat.title)
        val message: Message = decode(
            Fixtures.toolMessage().replace("\"kind\": \"vnc_click\"", "\"kind\": \"vnc_click\", \"latency_ms\": 12, \"nested\": {\"x\": 1}"),
        )
        assertEquals("vnc_click", message.metadata.kind)
        val approval: Approval = decode(Fixtures.approval().replace("\"risk\": \"high\"", "\"risk\": \"high\", \"expires_at\": 99"))
        assertEquals("high", approval.risk)
    }

    @Test
    fun `missing optional fields fall back to defaults and nulls`() {
        val chat: Chat = decode("""{"id": "c_min"}""")
        assertEquals("", chat.title)
        assertNull(chat.lastMessage)
        val state: DeviceState = decode("""{"type": "state_sync"}""")
        assertEquals(AgentState.IDLE, state.agentState)
        assertTrue(state.hosts.isEmpty())
        assertNull(state.batteryPct)
        // Explicit JSON null for a non-null field with a default is coerced to the default.
        val coerced: Activity = decode("""{"ts": 1, "task_id": "t", "kind": "wait", "summary": "s", "detail": null, "status": "ok", "chat_id": null, "step": 0, "screenshot_id": null}""")
        assertEquals("", coerced.detail)
    }

    @Test
    fun `state carries the step being executed and older software simply has none`() {
        // The same field on the WebSocket frame and on `GET /v1/state`.
        assertEquals("screenshot", decode<DeviceState>(Fixtures.stateSync("ACTING", "c", action = "screenshot")).action)
        val frame = LapisEvent.parse(Fixtures.stateSync("ACTING", "c", action = "ssh_exec")) as LapisEvent.StateSync
        assertEquals("ssh_exec", frame.state.action)
        // "" is "no step is executing"; a missing field is "not reported", which is not the same.
        assertEquals("", decode<DeviceState>(Fixtures.stateSync("THINKING", "c", action = "")).action)
        val older: DeviceState = decode(Fixtures.stateSync("ACTING", "c"))
        assertNull(older.action)
        assertEquals("ACTING", older.agentState)
        assertNull(DeviceState().action)
        // A kind this app version has never heard of is kept as it came.
        assertEquals("teleport", decode<DeviceState>(Fixtures.stateSync("ACTING", "c", action = "teleport")).action)
        // The field is not sent anywhere: it never appears in something this app encodes.
        assertFalse(json.encodeToString(DeviceState()).contains("\"action\""))
        // A frame with the wrong type there is dropped whole, as the device does.
        assertNull(LapisEvent.parse(Fixtures.stateSync("ACTING", "c").replace("\"agent_state\"", "\"action\": 7, \"agent_state\"")))
    }

    // ---------------------------------------------------------------- websocket frames

    @Test
    fun `every server frame type is recognised`() {
        assertTrue(LapisEvent.parse(Fixtures.stateSync()) is LapisEvent.StateSync)
        assertTrue(LapisEvent.parse(Fixtures.activity()) is LapisEvent.ActivityEvent)
        val request = LapisEvent.parse(Fixtures.approval()) as LapisEvent.ApprovalRequest
        assertEquals("req_1", request.approval.requestId)
        val resolved = LapisEvent.parse(Fixtures.approvalResolved(by = "android:Pixel")) as LapisEvent.ApprovalResolvedEvent
        assertEquals(ApprovalResolved("req_1", "approve_once", "android:Pixel"), resolved.resolved)
        val finished = LapisEvent.parse(
            """{"type": "task_finished", "task_id": "t_855667000", "chat_id": "c_9e838c02", "host": "macbook-pro",
                "status": "done", "summary": "Mock run complete: 6 scripted steps."}""",
        ) as LapisEvent.TaskFinishedEvent
        assertEquals(TaskFinished("t_855667000", "c_9e838c02", "macbook-pro", "done", "Mock run complete: 6 scripted steps."), finished.finished)
        val created = LapisEvent.parse(Fixtures.messageCreated("c_9e838c02", Fixtures.toolMessage())) as LapisEvent.ChatMessageCreated
        assertEquals("c_9e838c02", created.chatId)
        assertEquals(3L, created.message.id)
        val updated = LapisEvent.parse("""{"type": "chat_updated", "chat": ${Fixtures.chat(live = false)}}""") as LapisEvent.ChatUpdated
        assertEquals("c_9e838c02", updated.chat.id)
        assertEquals(LapisEvent.ChatDeleted("c_x"), LapisEvent.parse("""{"type": "chat_deleted", "chat_id": "c_x"}"""))
        val pairing = LapisEvent.parse("""{"type": "pairing", "mode": "off"}""") as LapisEvent.PairingEvent
        assertEquals("off", pairing.pairing.mode)
        val tick = LapisEvent.parse("""{"type": "telemetry_tick", "telemetry": ${Fixtures.TELEMETRY}}""") as LapisEvent.TelemetryTick
        assertEquals(84, tick.telemetry.battery?.percent)
        assertEquals(LapisEvent.Toast("warn", "Stuck loop detected"), LapisEvent.parse("""{"type": "toast", "level": "warn", "text": "Stuck loop detected"}"""))
        assertEquals(LapisEvent.Pong(1795541300), LapisEvent.parse("""{"type": "pong", "ts": 1795541300}"""))
    }

    @Test
    fun `unknown and malformed frames never throw`() {
        assertEquals(LapisEvent.Unknown("mic_level"), LapisEvent.parse("""{"type": "mic_level", "level": 0.4}"""))
        assertNull(LapisEvent.parse("not json"))
        assertNull(LapisEvent.parse("[1, 2]"))
        assertNull(LapisEvent.parse("""{"no_type": true}"""))
        assertNull(LapisEvent.parse("""{"type": 5}""")?.takeIf { it !is LapisEvent.Unknown })
        // A known type with a broken body is dropped, not fatal.
        assertNull(LapisEvent.parse("""{"type": "approval_resolved"}"""))
        assertNull(LapisEvent.parse("""{"type": "chat_message_created", "chat_id": "c", "message": "oops"}"""))
    }

    @Test
    fun `client commands match the wire contract`() {
        assertEquals("""{"type":"ping"}""", ClientCommand.Ping.encode())
        assertEquals("""{"type":"subscribe_telemetry","enabled":true}""", ClientCommand.SubscribeTelemetry(true).encode())
        assertEquals("""{"type":"kill_all"}""", ClientCommand.KillAll.encode())
        assertEquals(
            """{"type":"send_prompt","chat_id":"c_1","content":"say \"hi\""}""",
            ClientCommand.SendPrompt("c_1", "say \"hi\"").encode(),
        )
        assertEquals(
            """{"type":"approval_decision","request_id":"req_1","decision":"deny","reason":"no"}""",
            ClientCommand.ApprovalDecision("req_1", "deny", "no").encode(),
        )
        assertEquals(
            """{"type":"approval_decision","request_id":"req_1","decision":"approve_session"}""",
            ClientCommand.ApprovalDecision("req_1", "approve_session").encode(),
        )
        assertNotNull(LapisJson.parseToJsonElement(ClientCommand.SendPrompt("c", "line\nbreak").encode()))
    }
}
