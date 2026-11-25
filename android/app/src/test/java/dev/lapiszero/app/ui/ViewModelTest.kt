package dev.lapiszero.app.ui

import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.FakeEventSource
import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.MainDispatcherRule
import dev.lapiszero.app.MemorySecureStore
import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.await
import dev.lapiszero.app.awaitSize
import dev.lapiszero.app.awaitTrue
import dev.lapiszero.app.data.remote.Activity
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.data.remote.Chat
import dev.lapiszero.app.data.remote.ClientCommand
import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.Health
import dev.lapiszero.app.data.remote.Host
import dev.lapiszero.app.data.remote.HostAgent
import dev.lapiszero.app.data.remote.HostSsh
import dev.lapiszero.app.data.remote.HostVnc
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.data.remote.PairExchangeResponse
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.DeviceRepository
import dev.lapiszero.app.domain.HostAgentsState
import dev.lapiszero.app.domain.SshSupport
import dev.lapiszero.app.errorResponse
import dev.lapiszero.app.jsonResponse
import dev.lapiszero.app.ui.chats.ApprovalViewModel
import dev.lapiszero.app.ui.chats.ChatListViewModel
import dev.lapiszero.app.ui.chats.ChatThreadViewModel
import dev.lapiszero.app.ui.chats.RunWith
import dev.lapiszero.app.ui.chats.buildChatRows
import dev.lapiszero.app.ui.chats.buildRunWith
import dev.lapiszero.app.ui.components.kindLabel
import dev.lapiszero.app.ui.config.AgentSettings
import dev.lapiszero.app.ui.config.ConfigRules
import dev.lapiszero.app.ui.config.ConfigViewModel
import dev.lapiszero.app.ui.config.ProviderPreset
import dev.lapiszero.app.ui.device.DeviceViewModel
import dev.lapiszero.app.ui.device.MirrorState
import dev.lapiszero.app.ui.device.clockLabel
import dev.lapiszero.app.ui.kvm.ClickMarker
import dev.lapiszero.app.ui.kvm.KvmViewModel
import dev.lapiszero.app.ui.kvm.buildMarkers
import dev.lapiszero.app.ui.kvm.parseClick
import dev.lapiszero.app.ui.onboarding.PairUi
import dev.lapiszero.app.ui.onboarding.PairingViewModel
import dev.lapiszero.app.ui.onboarding.PhoneInfo
import java.io.IOException
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test

class ViewModelTest {
    @get:Rule
    val mainRule = MainDispatcherRule()

    private val server = MockWebServer()
    private val router = Router()
    private val source = FakeEventSource()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private lateinit var repo: ChatRepository
    private val chatId = "c_9e838c02"

    @Before
    fun setUp() {
        server.dispatcher = router
        server.start()
        router.json("GET", "/v1/chats", """{"chats": [${Fixtures.chat()}, ${Fixtures.chat(id = "c_arch", title = "arch job").replace("macbook-pro", "arch-dev")}]}""")
        router.json("GET", "/v1/chats/$chatId", """{"chat": ${Fixtures.chat()}, "messages": [], "has_more": false}""")
        repo = ChatRepository(server.api(), source, scope)
        repo.start()
    }

    @After
    fun tearDown() {
        scope.cancel()
        server.close()
    }

    private suspend fun sync(json: String = Fixtures.stateSync()) {
        source.emit(1, json)
        repo.state.await { it != null }
        repo.chats.await { it.isNotEmpty() }
    }

    private fun <T> collect(flow: kotlinx.coroutines.flow.Flow<T>): CopyOnWriteArrayList<T> {
        val out = CopyOnWriteArrayList<T>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { flow.collect { out += it } }
        return out
    }

    // ------------------------------------------------------------------ approvals

    @Test
    fun `approval arrives is shown resolved on the device and the sheet clears`() = runBlocking<Unit> {
        sync()
        val vm = ApprovalViewModel(repo)
        val messages = collect(vm.messages)
        assertTrue(vm.ui.value.approvals.isEmpty())

        source.emit(1, Fixtures.approval("req_1"))
        val shown = vm.ui.await { it.approvals.size == 1 }
        assertEquals("Click Retry Build at (840, 412)", shown.approvals.single().summary)
        assertTrue(shown.busy.isEmpty())

        source.emit(1, Fixtures.approvalResolved("req_1", "approve_once", "device"))
        vm.ui.await { it.approvals.isEmpty() }
        messages.awaitSize(1)
        assertEquals(listOf("Approved once on the device"), messages.toList())
        assertEquals("nothing was sent from this phone", 0, router.count("POST /v1/approvals/req_1"))
    }

    @Test
    fun `a decision taken here clears the sheet without a resolved-elsewhere message`() = runBlocking<Unit> {
        sync()
        val vm = ApprovalViewModel(repo)
        val messages = collect(vm.messages)
        source.emit(1, Fixtures.approval("req_1"))
        vm.ui.await { it.approvals.size == 1 }
        router.on("POST", "/v1/approvals/req_1") {
            MockResponse.Builder().code(200).headersDelay(250, TimeUnit.MILLISECONDS).body("""{"ok": true}""").build()
        }

        vm.decide("req_1", Decision.APPROVE_SESSION)
        vm.decide("req_1", Decision.DENY) // a double tap while in flight is ignored
        assertEquals(setOf("req_1"), vm.ui.await { it.busy.isNotEmpty() }.busy)
        vm.ui.await { it.approvals.isEmpty() && it.busy.isEmpty() }
        assertEquals(1, router.count("POST /v1/approvals/req_1"))
        assertTrue(router.take("POST /v1/approvals/req_1").body!!.utf8().contains("approve_session"))

        source.emit(1, Fixtures.approvalResolved("req_1", "approve_session", "android:Pixel"))
        delay(150)
        assertTrue("our own decision is not announced back to us", messages.isEmpty())
    }

    @Test
    fun `resolution by another phone or the kill switch is announced`() = runBlocking<Unit> {
        sync()
        val vm = ApprovalViewModel(repo)
        val messages = collect(vm.messages)
        source.emit(1, Fixtures.approval("req_1"))
        source.emit(1, Fixtures.approval("req_2", action = "ssh_exec"))
        vm.ui.await { it.approvals.size == 2 }
        source.emit(1, Fixtures.approvalResolved("req_1", "deny", "android:Tablet"))
        source.emit(1, Fixtures.approvalResolved("req_2", "deny", "kill_switch"))
        vm.ui.await { it.approvals.isEmpty() }
        messages.awaitSize(2)
        assertEquals(listOf("Denied from Tablet", "Denied by the kill switch"), messages.toList())
    }

    @Test
    fun `a failed decision leaves the approval and says why`() = runBlocking<Unit> {
        sync()
        val vm = ApprovalViewModel(repo)
        val messages = collect(vm.messages)
        source.emit(1, Fixtures.approval("req_1"))
        vm.ui.await { it.approvals.size == 1 }
        router.on("POST", "/v1/approvals/req_1") { errorResponse(401, "unauthorized", "revoked") }
        vm.decide("req_1", Decision.APPROVE_ONCE)
        messages.awaitSize(1)
        assertTrue(messages.single().startsWith("Decision not sent"))
        assertEquals(1, vm.ui.value.approvals.size)
        assertTrue(vm.ui.await { it.busy.isEmpty() }.busy.isEmpty())
    }

    // ------------------------------------------------------------------ chat thread

    @Test
    fun `409 busy puts the text back into the composer`() = runBlocking<Unit> {
        sync(Fixtures.stateSync("ACTING", activeChatId = "c_arch", step = 2))
        val vm = ChatThreadViewModel(repo, chatId)
        val messages = collect(vm.messages)
        assertTrue("another chat owns the agent", vm.ui.await { it.busyElsewhere }.busyElsewhere)
        assertFalse(vm.ui.value.running)

        router.on("POST", "/v1/chats/$chatId/messages") { errorResponse(409, "busy", "Agent busy: a task is already running") }
        vm.setDraft("restart the worker")
        vm.send()
        messages.awaitSize(1)
        assertTrue(messages.single().contains("busy"))
        assertEquals("restart the worker", vm.draft.value)
        assertFalse(vm.sending.await { !it })
        assertTrue(vm.ui.await { it.thread.pending.isEmpty() }.thread.messages.isEmpty())
    }

    @Test
    fun `a successful send clears the composer and shows the prompt`() = runBlocking<Unit> {
        sync()
        val vm = ChatThreadViewModel(repo, chatId)
        router.on("POST", "/v1/chats/$chatId/messages") {
            MockResponse.Builder().code(202).headersDelay(250, TimeUnit.MILLISECONDS)
                .body("""{"message": ${Fixtures.userMessage(id = 11, content = "open settings", taskId = "t_1")}, "task_id": "t_1"}""").build()
        }
        vm.setDraft("open settings")
        vm.send()
        assertEquals("", vm.draft.value)
        assertTrue(vm.sending.value)
        vm.send() // ignored while the first is in flight
        assertEquals("open settings", vm.ui.await { it.thread.pending.isNotEmpty() }.thread.pending.single().content)
        val done = vm.ui.await { it.thread.messages.isNotEmpty() }
        assertTrue(done.thread.pending.isEmpty())
        assertEquals(11L, done.thread.messages.single().id)
        assertEquals(1, router.count("POST /v1/chats/$chatId/messages"))

        // The daemon starts working on it: the composer turns into STOP / KILL.
        source.emit(1, Fixtures.stateSync("THINKING", activeChatId = chatId, step = 1))
        val running = vm.ui.await { it.running }
        assertEquals(1, running.step)
        assertEquals(25, running.maxSteps)
        assertFalse(running.busyElsewhere)
    }

    @Test
    fun `a draft typed during a failed send is not overwritten`() = runBlocking<Unit> {
        sync()
        val vm = ChatThreadViewModel(repo, chatId)
        router.on("POST", "/v1/chats/$chatId/messages") {
            MockResponse.Builder().code(409).headersDelay(200, TimeUnit.MILLISECONDS)
                .body("""{"error": {"code": "busy", "message": "Agent busy"}}""").build()
        }
        vm.setDraft("first")
        vm.send()
        vm.setDraft("second thought")
        vm.sending.await { !it }
        assertEquals("second thought", vm.draft.value)
    }

    @Test
    fun `kill switch and stop reach the daemon`() = runBlocking<Unit> {
        sync(Fixtures.stateSync("ACTING", activeChatId = chatId, step = 2))
        val vm = ChatThreadViewModel(repo, chatId)
        val messages = collect(vm.messages)
        router.json("POST", "/v1/agent/kill-all", """{"ok": true}""")
        router.json("POST", "/v1/chats/$chatId/stop", """{"stopped": true}""")

        vm.killAll()
        router.take("POST /v1/agent/kill-all")
        messages.awaitSize(1)
        assertEquals("Kill switch sent", messages.first())
        vm.stop()
        router.take("POST /v1/chats/$chatId/stop")

        // With REST unreachable the kill still goes out over the socket.
        server.close()
        vm.killAll()
        awaitTrue { source.sent.any { it is ClientCommand.KillAll } }
    }

    @Test
    fun `pending approvals of this chat are surfaced in the thread`() = runBlocking<Unit> {
        sync()
        val vm = ChatThreadViewModel(repo, chatId)
        source.emit(1, Fixtures.approval("req_1", chatId = chatId))
        source.emit(1, Fixtures.approval("req_2", chatId = "c_arch", action = "ssh_exec"))
        assertEquals(listOf("req_1"), vm.ui.await { it.approvals.isNotEmpty() }.approvals.map { it.requestId })
    }

    // ------------------------------------------------------------------ chat list

    @Test
    fun `chat rows derive running and approval badges from live state`() {
        val chats = listOf(
            LapisJson.decodeFromString<Chat>(Fixtures.chat()),
            LapisJson.decodeFromString<Chat>(Fixtures.chat(id = "c_arch").replace("macbook-pro", "arch-dev")),
            LapisJson.decodeFromString<Chat>(Fixtures.chat(id = "c_old").replace("\"archived\": false", "\"archived\": true")),
        )
        val state = LapisJson.decodeFromString<DeviceState>(Fixtures.stateSync("WAITING_APPROVAL", chatId, step = 5))
        val approvals = listOf(
            LapisJson.decodeFromString<Approval>(Fixtures.approval("req_1", chatId = chatId)),
            LapisJson.decodeFromString<Approval>(Fixtures.approval("req_2", chatId = chatId, action = "ssh_exec")),
        )
        val rows = buildChatRows(chats, state, approvals, hostFilter = null)
        assertEquals(listOf(chatId, "c_arch"), rows.map { it.chat.id }) // archived ones are hidden
        assertTrue(rows[0].active)
        assertEquals(5, rows[0].step)
        assertEquals(25, rows[0].maxSteps)
        assertEquals(2, rows[0].pendingApprovals)
        assertFalse(rows[1].active)
        assertNull(rows[1].step)
        assertEquals(0, rows[1].pendingApprovals)

        assertEquals(listOf("c_arch"), buildChatRows(chats, state, approvals, "arch-dev").map { it.chat.id })
        val idle = LapisJson.decodeFromString<DeviceState>(Fixtures.stateSync())
        assertTrue(buildChatRows(chats, idle, emptyList(), null).none { it.active })
        assertTrue(buildChatRows(chats, null, emptyList(), null).none { it.active })
    }

    @Test
    fun `chat list filters by host and creates chats`() = runBlocking<Unit> {
        sync()
        val vm = ChatListViewModel(repo)
        val all = vm.ui.await { it.rows.size == 2 }
        assertEquals(listOf("macbook-pro", "arch-dev"), all.hosts)
        assertEquals("macbook-pro", all.activeHost)
        vm.setHostFilter("arch-dev")
        assertEquals(listOf("c_arch"), vm.ui.await { it.hostFilter == "arch-dev" }.rows.map { it.chat.id })
        vm.setHostFilter("gone")
        assertNull("a filter for a removed host shows everything", vm.ui.await { it.rows.size == 2 }.hostFilter)

        val created = collect(vm.created)
        router.json("POST", "/v1/chats", Fixtures.chat(id = "c_new", title = "New chat", updatedAt = 1795549999), code = 201)
        vm.createChat("macbook-pro")
        created.awaitSize(1)
        assertEquals("c_new", created.single())
        assertEquals("c_new", vm.ui.await { it.rows.size == 3 }.rows.first().chat.id)
    }

    // ------------------------------------------------------------------ live kvm

    @Test
    fun `click coordinates are parsed from tool summaries`() {
        assertEquals(840 to 412, parseClick("click(840, 412) left"))
        assertEquals(1 to 2, parseClick("Click Retry Build at (1,2)"))
        assertEquals(-5 to 7, parseClick("drag( -5 , 7 )"))
        assertNull(parseClick("type 'make check'"))
        assertNull(parseClick("uname -a"))
    }

    @Test
    fun `markers keep the latest clicks and add pending approval targets`() {
        fun click(step: Int, x: Int) = LapisJson.decodeFromString<Activity>(
            Fixtures.activity(step = step, summary = "click($x, 10) left").replace("\"type\": \"activity_event\", ", ""),
        )
        val ssh = click(9, 0).copy(kind = "ssh_exec", summary = "ls (1, 2)")
        val activity = (1..7).map { click(it, it * 100) } + ssh
        val approval = LapisJson.decodeFromString<Approval>(Fixtures.approval("req_1"))
        val markers = buildMarkers(activity, listOf(approval, approval.copy(clickX = null)))
        assertEquals(listOf("#3", "#4", "#5", "#6", "#7", "?"), markers.map { it.label })
        assertEquals(ClickMarker(700, 10, "#7"), markers[4])
        assertEquals(ClickMarker(840, 412, "?", pending = true), markers.last())
    }

    @Test
    fun `nudge is only possible while paused or waiting for approval`() = runBlocking<Unit> {
        router.json("POST", "/v1/hosts/macbook-pro/nudge", """{"ok": true}""")
        sync(Fixtures.stateSync("ACTING", activeChatId = chatId, step = 2))
        val vm = KvmViewModel(repo)
        val messages = collect(vm.messages)
        val acting = vm.ui.await { it.running }
        assertFalse(acting.canNudge)
        vm.nudgeClick(10, 20)
        vm.nudgeType("hello")
        vm.nudgeKey("ctrl+c")
        delay(150)
        assertEquals("nothing is sent while the agent is acting", 0, router.count("POST /v1/hosts/macbook-pro/nudge"))
        assertTrue(messages.all { it.contains("only available while the agent is paused") })

        source.emit(1, Fixtures.stateSync("PAUSED", activeChatId = chatId, step = 2))
        assertTrue(vm.ui.await { it.canNudge }.canNudge)
        vm.nudgeClick(10, 20)
        val body = LapisJson.parseToJsonElement(router.take("POST /v1/hosts/macbook-pro/nudge").body!!.utf8()).jsonObject
        assertEquals("click", body.getValue("action").jsonPrimitive.content)
        assertEquals("10", body.getValue("x").jsonPrimitive.content)
        assertEquals("20", body.getValue("y").jsonPrimitive.content)
        vm.busy.await { !it }

        source.emit(1, Fixtures.stateSync("WAITING_APPROVAL", chatId, 5, approvals = Fixtures.approval()))
        // State and approvals are two flows: wait until both halves of the snapshot landed.
        val waiting = vm.ui.await { it.agentState == "WAITING_APPROVAL" && it.markers.any { m -> m.pending } }
        assertTrue(waiting.canNudge)
        assertEquals(2L, waiting.screenshotId) // falls back to the approval's frame
        assertTrue(waiting.markers.any { it.pending })

        source.emit(1, Fixtures.stateSync("IDLE"))
        assertFalse(vm.ui.await { !it.running }.canNudge)
    }

    @Test
    fun `kvm follows the newest frame and reports not_paused from the daemon`() = runBlocking<Unit> {
        sync(Fixtures.stateSync("PAUSED", activeChatId = chatId, step = 2))
        val vm = KvmViewModel(repo)
        val messages = collect(vm.messages)
        source.emit(1, Fixtures.activity(step = 1, screenshotId = 4))
        source.emit(1, Fixtures.activity(step = 2, kind = "ssh_exec", summary = "df -h", screenshotId = null))
        val ui = vm.ui.await { it.ticker.size == 2 }
        assertEquals(4L, ui.screenshotId)
        assertEquals(listOf(2, 1), ui.ticker.map { it.step }) // newest first

        // The agent resumed a moment before the tap arrived.
        router.on("POST", "/v1/hosts/macbook-pro/nudge") { errorResponse(409, "not_paused", "Manual assist is only available while the agent is paused") }
        vm.nudgeKey("Return")
        messages.awaitSize(1)
        assertTrue(messages.single().contains("paused"))
    }

    // ------------------------------------------------------------------ device cockpit

    @Test
    fun `screen mirror handles 504 gracefully and telemetry subscription follows visibility`() = runBlocking<Unit> {
        sync()
        router.on("GET", "/v1/device/screen.png") { errorResponse(504, "ui_unavailable", "lapis-ui did not answer the screen request") }
        router.json("GET", "/v1/device/telemetry", Fixtures.TELEMETRY)
        val vm = DeviceViewModel(repo, server.api())
        vm.onVisible(true)
        assertEquals(MirrorState.Unavailable, vm.ui.await { it.mirror == MirrorState.Unavailable }.mirror)
        assertEquals(84, vm.ui.await { it.telemetry != null }.telemetry?.battery?.percent)
        assertTrue(source.sent.contains(ClientCommand.SubscribeTelemetry(true)))

        // Live mode gives up by itself instead of polling a missing lapis-ui.
        vm.setLiveMirror(true)
        assertFalse(vm.ui.await { !it.liveMirror }.liveMirror)

        router.on("GET", "/v1/device/screen.png") { MockResponse.Builder().code(200).body(okio.Buffer().write(byteArrayOf(1, 2, 3))).build() }
        vm.refreshMirror()
        assertEquals(3, (vm.ui.await { it.mirror is MirrorState.Frame }.mirror as MirrorState.Frame).png.size)

        vm.onVisible(false)
        assertEquals(ClientCommand.SubscribeTelemetry(false), source.sent.last())
    }

    @Test
    fun `display controls patch the device and power actions are posted`() = runBlocking<Unit> {
        sync()
        val vm = DeviceViewModel(repo, server.api())
        val messages = collect(vm.messages)
        router.on("PATCH", "/v1/device/ui") {
            MockResponse.Builder().code(200).headersDelay(200, TimeUnit.MILLISECONDS)
                .body("""{"theme": "dark", "brightness": 35, "mute_audio": false, "timezone": ""}""").build()
        }
        vm.setBrightness(135) // clamped
        assertEquals("optimistic while in flight", 100, vm.ui.await { it.uiOverride != null }.lcd.brightness)
        assertEquals("""{"brightness":100}""", router.take("PATCH /v1/device/ui").body!!.utf8())
        vm.ui.await { it.uiOverride == null }
        vm.setTheme("dark")
        assertEquals("""{"theme":"dark"}""", router.take("PATCH /v1/device/ui").body!!.utf8())
        vm.setMute(true)
        assertEquals("""{"mute_audio":true}""", router.take("PATCH /v1/device/ui").body!!.utf8())
        vm.wake()
        assertEquals("""{"wake":true}""", router.take("PATCH /v1/device/ui").body!!.utf8())

        router.json("POST", "/v1/device/power", """{"ok": true, "action": "start_qr_pairing"}""", code = 202)
        vm.power("start_qr_pairing", "Pair another phone (QR)")
        assertEquals("""{"action":"start_qr_pairing"}""", router.take("POST /v1/device/power").body!!.utf8())
        awaitTrue { messages.any { it.contains("requested") } }
        vm.ui.await { it.busyAction == null }
    }

    @Test
    fun `clock label`() {
        assertEquals("America/Los_Angeles (UTC-08:00)", clockLabel("America/Los_Angeles", -480))
        assertEquals("Asia/Kolkata (UTC+05:30)", clockLabel("Asia/Kolkata", 330))
        assertEquals("UTC+00:00", clockLabel("", 0))
        assertEquals("Europe/Berlin", clockLabel("Europe/Berlin", null))
        assertEquals("not set", clockLabel("", null))
    }

    // ------------------------------------------------------------------ config

    @Test
    fun `agent settings edit lapis toml without touching unknown keys`() {
        val raw = LapisJson.parseToJsonElement(
            """{"api": {"enabled": true, "port": 8421, "frames_cap_mb": 25},
                "llm": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key_env": "ANTHROPIC_API_KEY",
                        "effort": "medium", "base_url": "", "max_steps": 25, "request_timeout_sec": 120, "future_knob": [1, 2]},
                "kvm": {"llm_max_width": 1280, "llm_max_height": 720, "jpeg_quality": 75, "vnc_idle_disconnect_sec": 60,
                        "stuck_threshold": 3, "stuck_hamming_max": 2, "ssh_timeout_sec": 30, "ssh_output_truncate": 4096},
                "security": {"approval_default_tier": "balanced"},
                "cron": {"enabled": true, "schedules": [{"name": "Daily Digest", "schedule": "0 8 * * *", "host": "macbook-pro", "prompt": "p", "enabled": false}]},
                "brand_new_section": {"x": true}}""",
        ).jsonObject
        val settings = AgentSettings.from(raw)
        assertEquals("anthropic", settings.provider)
        assertEquals("claude-sonnet-5-5", settings.model)
        assertEquals(25, settings.maxSteps)
        assertEquals(3, settings.stuckThreshold)
        assertEquals(60, settings.vncIdleDisconnectSec)
        assertEquals("balanced", settings.approvalDefaultTier)
        assertEquals("unchanged settings give the identical document", raw, settings.applyTo(raw))

        val edited = settings.copy(provider = "openrouter", model = " gpt-x ", maxSteps = 40, stuckThreshold = 5, approvalDefaultTier = "strict").applyTo(raw)
        val llm = edited.getValue("llm").jsonObject
        assertEquals("openrouter", llm.getValue("provider").jsonPrimitive.content)
        assertEquals("gpt-x", llm.getValue("model").jsonPrimitive.content)
        assertEquals("40", llm.getValue("max_steps").jsonPrimitive.content)
        assertEquals(2, llm.getValue("future_knob").jsonArray.size)
        assertEquals("5", edited.getValue("kvm").jsonObject.getValue("stuck_threshold").jsonPrimitive.content)
        assertEquals("75", edited.getValue("kvm").jsonObject.getValue("jpeg_quality").jsonPrimitive.content)
        assertEquals("strict", edited.getValue("security").jsonObject.getValue("approval_default_tier").jsonPrimitive.content)
        for (untouched in listOf("api", "cron", "brand_new_section")) assertEquals(raw[untouched], edited[untouched])
        // A minimal or empty document does not crash either.
        assertEquals(AgentSettings(), AgentSettings.from(JsonObject(emptyMap())))
        assertEquals(setOf("llm", "kvm", "security"), AgentSettings().applyTo(JsonObject(emptyMap())).keys)
    }

    @Test
    fun `config input rules`() {
        for (ok in listOf("macbook-pro", "arch_dev", "A1", "_x")) assertTrue(ok, ConfigRules.isValidHostId(ok))
        for (bad in listOf("", "1abc", "has space", "a/b", "é", "x".repeat(65), "-lead")) assertFalse(bad, ConfigRules.isValidHostId(bad))
        assertTrue(ConfigRules.isValidSecretName("LAPIS_VNC_PASSWORD_macbook_pro"))
        assertFalse(ConfigRules.isValidSecretName("ANTHROPIC-API-KEY"))
        assertFalse(ConfigRules.isValidSecretName("A=B"))
        for (ok in listOf("0 8 * * *", "*/15 * * * *", "0 9 * * 1-5", " 5  4 1,15 * mon ")) assertTrue(ok, ConfigRules.isPlausibleCron(ok))
        for (bad in listOf("", "* * * *", "* * * * * *", "0 8 * * $", "daily")) assertFalse(bad, ConfigRules.isPlausibleCron(bad))
        assertTrue(ConfigRules.CRON_PRESETS.all { ConfigRules.isPlausibleCron(it.second) })

        // What people type becomes a name the device accepts, instead of a silent refusal.
        assertEquals("minimax_api_key", ConfigRules.normalizeSecretName("minimax api key"))
        assertEquals("OPENROUTER_API_KEY", ConfigRules.normalizeSecretName("OPENROUTER-API-KEY"))
        assertEquals("open_router_key", ConfigRules.normalizeSecretName("open.router  key"))
        assertEquals("KEY1", ConfigRules.normalizeSecretName("12KEY1!é="))
        assertEquals(64, ConfigRules.normalizeSecretName("x".repeat(80)).length)
        for (typed in listOf("minimax api key", "a-b.c", "_ok", "Groq Key 2")) {
            assertTrue(typed, ConfigRules.isValidSecretName(ConfigRules.normalizeSecretName(typed)))
        }
        // Names that would replace a variable of the daemon's own process are refused.
        for (reserved in listOf("PATH", "path", "HOME", "LD_PRELOAD", "PYTHONPATH", "LAPIS_CONFIG_DIR")) {
            assertFalse(reserved, ConfigRules.isValidSecretName(reserved))
        }
        assertTrue(ConfigRules.isValidSecretName("LAPIS_VNC_PASSWORD_mac"))

        // A host keeps a usable vault name; anything else (a password typed into the old
        // "secret name" field, say) is replaced by one derived from the host id.
        val vault = setOf("LAPIS_VNC_PASSWORD_mac")
        assertEquals("LAPIS_VNC_PASSWORD_mac", ConfigRules.vncSecretName("office-mac-mini", "LAPIS_VNC_PASSWORD_mac", vault))
        assertEquals("LAPIS_VNC_PASSWORD_office_mac_mini", ConfigRules.vncSecretName("office-mac-mini", "p@ss-word", vault))
        assertEquals("LAPIS_VNC_PASSWORD_office_mac_mini", ConfigRules.vncSecretName("office-mac-mini", "", vault))
        // A password that happens to be made of name characters is not a vault name either:
        // nothing is stored under it, so it is replaced the next time the host is saved.
        assertEquals("LAPIS_VNC_PASSWORD_office_mac_mini", ConfigRules.vncSecretName("office-mac-mini", "Hunter2abc", vault))
        // The vault could not be read: a usable name is left alone rather than orphaning its password.
        assertEquals("LAPIS_VNC_PASSWORD_mac", ConfigRules.vncSecretName("office-mac-mini", "LAPIS_VNC_PASSWORD_mac", null))
        assertEquals("LAPIS_VNC_PASSWORD_office_mac_mini", ConfigRules.vncSecretName("office-mac-mini", "p@ss-word", null))
        assertTrue(ConfigRules.isValidSecretName(ConfigRules.vncSecretName("x".repeat(64), "", emptySet())))
        // A renamed host keeps its old name: a new host with the old id gets one of its own.
        assertEquals("LAPIS_VNC_PASSWORD_mac_2", ConfigRules.vncSecretName("mac", "", vault, taken = setOf("LAPIS_VNC_PASSWORD_mac")))
        assertEquals("LAPIS_VNC_PASSWORD_mac_3", ConfigRules.vncSecretName("mac", "", null, taken = setOf("LAPIS_VNC_PASSWORD_mac", "LAPIS_VNC_PASSWORD_mac_2")))

        // Base URL: what gets pasted is tidied; credentials, queries and non-URLs are refused.
        assertEquals("https://api.minimax.io/v1", ConfigRules.normalizeBaseUrl(" https://api.minimax.io/v1/chat/completions/ "))
        assertEquals("https://api.deepseek.com", ConfigRules.normalizeBaseUrl("https://api.deepseek.com/"))
        for (ok in listOf("", "https://api.x.ai/v1", "http://10.0.0.5:11434/v1", "https://generativelanguage.googleapis.com/v1beta/openai")) {
            assertTrue(ok, ConfigRules.isValidBaseUrl(ok))
        }
        for (bad in listOf("api.minimax.io/v1", "https://user:sk-key@host/v1", "https://host/v1?key=sk-1", "https://host/v1#x", "ftp://host", "https://")) {
            assertFalse(bad, ConfigRules.isValidBaseUrl(bad))
        }
        // Self-hosted endpoints: an IPv6 literal, an underscore in the name, a scheme in capitals.
        for (ok in listOf("http://[fd7a:115c:a1e0::1]:11434/v1", "http://llm_proxy:8000/v1", "HTTPS://api.x.ai/v1")) {
            assertTrue(ok, ConfigRules.isValidBaseUrl(ok))
        }
    }

    /** Made-up values in the shapes real keys have; none of them is a key. */
    private val keyShaped = listOf(
        "sk-ant-api03-" + "Ab1_cD2-".repeat(11) + "AA",
        "sk-or-v1-" + "0a1b2c3d".repeat(8),
        "sk-" + "Zx9Yw8Vu".repeat(6),
        "sk-proj-" + "Qr5St6Uv".repeat(7),
        "gsk_" + "aB3dE5gH".repeat(6) + "wxyz",
        "AIzaSy" + "D4e-F5g_".repeat(4) + "H",
        "xai-" + "Mn7Op8Qr".repeat(10),
        "fw_" + "3kT9mW2xLp7QaZ4vBn6RcY8d",
        "eyJhbGciOiJIUzI1NiJ9." + "eyJzdWIiOiIxMjM0In0." + "c2lnbmF0dXJl",
        "9f8e1c0a7b6d5e4f3a2b1c0d9e8f7a6b",
        "tskey-auth-" + "kQ3x9Zp2CNTRL",
        // Keys written in groups: a UUID (with and without a leading digit) and a licence-style token.
        "3f2b8c1e-9a4d-4e7b-b1c2-5d6e7f8a9b0c",
        "a3f2b8c1-9a4d-4e7b-b1c2-5d6e7f8a9b0c",
        "ABCD-EF12-GH34-IJ56-KL78",
    )

    @Test
    fun `a key pasted where its name belongs is refused, not tidied into a name`() {
        for (key in keyShaped) {
            assertTrue(key.take(8), ConfigRules.looksLikeSecretValue(key))
            // The tidied forms would pass for names: that was the hole.
            val tidied = ConfigRules.normalizeSecretName(key)
            assertTrue(key.take(8), ConfigRules.looksLikeSecretValue(tidied))
            assertTrue(key.take(8), ConfigRules.looksLikeSecretValue(tidied.uppercase()))
            assertEquals(key.take(8), ConfigRules.NOT_A_NAME, ConfigRules.secretNameProblem(key))
            assertEquals(key.take(8), ConfigRules.NOT_A_NAME, ConfigRules.secretNameProblem(tidied))
            assertFalse("the refusal does not repeat the value", ConfigRules.NOT_A_NAME.contains(key.take(12)))
        }
        val names = ProviderPreset.ALL.map { it.keyEnv } + listOf(
            "LAPIS_VNC_MAC", "LAPIS_VNC_PASSWORD_office_mac_mini", "AZURE_OPENAI_API_KEY_EASTUS2",
            "GOOGLE_GENERATIVE_AI_API_KEY", "my_key", "MiniMaxAPI", "minimax api key", "Groq Key 2", "INTERNATIONALIZATION_KEY",
            // Typed on a phone, where _ is a layer away: no separators, a year at the end, long.
            "minimaxapikey2025", "anthropicapikey2", "OpenRouterApiKey2025", "openrouterapikeyforlapiszero",
            "SK_OPENROUTER", "sk_minimax", "TSKEY_HOME", "OPENAI_API_KEY_2025_11_24",
            "MY_VERY_LONG_NAME_FOR_THE_KEY_OF_THE_PROVIDER_I_USE_AT_WORK",
        )
        for (name in names) {
            assertFalse(name, ConfigRules.looksLikeSecretValue(name))
            assertNull(name, ConfigRules.secretNameProblem(ConfigRules.normalizeSecretName(name)))
        }
        // The other refusals say what is wrong.
        assertTrue(ConfigRules.secretNameProblem("A-B")!!.contains("letters, digits and _"))
        assertTrue(ConfigRules.secretNameProblem("")!!.contains("letters, digits and _"))
        assertTrue(ConfigRules.secretNameProblem("PYTHONPATH")!!.contains("reserved"))
    }

    @Test
    fun `agent settings are judged where they changed`() {
        val loaded = AgentSettings(provider = "openai", model = "local-model", apiKeyEnv = "PYTHONANYWHERE_TOKEN", baseUrl = "http://[fd7a::1]:11434/v1?x=1")
        // What the device already accepted does not block changing something else...
        assertNull(ConfigRules.agentProblem(loaded.copy(maxSteps = 40), loaded))
        // ...but a new value has to pass.
        assertTrue(ConfigRules.agentProblem(loaded.copy(baseUrl = "https://host/v1?key=1"), loaded)!!.contains("base URL"))
        assertTrue(ConfigRules.agentProblem(loaded.copy(apiKeyEnv = "LD_PRELOAD"), loaded)!!.contains("reserved"))
        assertTrue(ConfigRules.agentProblem(loaded.copy(model = " "), loaded)!!.contains("model id"))
        for (key in keyShaped) {
            assertEquals(key.take(8), ConfigRules.NOT_A_NAME, ConfigRules.agentProblem(loaded.copy(apiKeyEnv = key), loaded))
            assertEquals(ConfigRules.NOT_A_NAME, ConfigRules.agentProblem(loaded.copy(apiKeyEnv = ConfigRules.normalizeSecretName(key)), loaded))
        }
        assertNull(ConfigRules.agentProblem(loaded.copy(apiKeyEnv = "MY_KEY", baseUrl = "https://api.minimax.io/v1/chat/completions/"), loaded))
        // A name the device holds a secret under is a name, whatever it looks like.
        val odd = ConfigRules.normalizeSecretName(keyShaped[2])
        assertNull(ConfigRules.agentProblem(loaded.copy(apiKeyEnv = odd), loaded, stored = setOf(odd)))
    }

    @Test
    fun `a provider's key is not sent to another provider's host`() {
        val all = ProviderPreset.ALL
        val minimax = all.first { it.name == "MiniMax" }.applyTo(AgentSettings())
        // Every protocol has a preset for its own endpoint, and no preset warns about itself.
        for (protocol in AgentSettings.PROVIDERS) assertEquals(protocol, 1, all.count { it.provider == protocol && it.baseUrl.isEmpty() })
        for (preset in all) assertNull(preset.name, ProviderPreset.strayKey(preset.applyTo(AgentSettings())))

        // Protocol changed by hand: the MiniMax endpoint goes, and so does the MiniMax key name.
        val asAnthropic = ProviderPreset.withProtocol(minimax, "anthropic")
        assertEquals("", asAnthropic.baseUrl)
        assertEquals("ANTHROPIC_API_KEY", asAnthropic.apiKeyEnv)
        assertEquals("the MiniMax example model does not come along", "claude-sonnet-5-5", asAnthropic.model)
        assertEquals("a model of the user's own does", "my-model", ProviderPreset.withProtocol(minimax.copy(model = "my-model"), "anthropic").model)
        assertNull(ProviderPreset.strayKey(asAnthropic))
        // An endpoint typed by hand for a provider without a button: only the protocol changes.
        val groq = AgentSettings(provider = "anthropic", baseUrl = "https://api.groq.com/openai/v1", model = "llama-x", apiKeyEnv = "GROQ_API_KEY")
        assertEquals(groq.copy(provider = "openai"), ProviderPreset.withProtocol(groq, "openai"))
        val byHand = AgentSettings(provider = "anthropic", baseUrl = "https://api.minimax.io/v1/", model = "MiniMax-M3", apiKeyEnv = "MiniMaxAPI")
        assertEquals("MiniMax", ProviderPreset.matching(ProviderPreset.withProtocol(byHand, "openai"))?.name)
        assertEquals("OPENROUTER_API_KEY", ProviderPreset.withProtocol(minimax, "openrouter").apiKeyEnv)
        assertEquals("the same protocol changes nothing", minimax, ProviderPreset.withProtocol(minimax, "openai"))
        assertEquals("a name of the user's own is kept", "MY_KEY", ProviderPreset.withProtocol(minimax.copy(apiKeyEnv = "MY_KEY"), "anthropic").apiKeyEnv)

        // Typed by hand: another provider's endpoint with the Anthropic key name left in place.
        val stray = ProviderPreset.strayKey(AgentSettings(provider = "openai", baseUrl = "https://api.groq.com/openai/v1", apiKeyEnv = "ANTHROPIC_API_KEY"))
        assertTrue(stray!!, stray.contains("Anthropic") && stray.contains("api.groq.com"))
        assertTrue(ProviderPreset.strayKey(AgentSettings(provider = "anthropic", apiKeyEnv = "minimax_api_key"))!!.contains("api.anthropic.com"))
        assertNull("a name of the user's own belongs to no provider", ProviderPreset.strayKey(AgentSettings(provider = "openai", baseUrl = "https://llm.example.internal/v1", apiKeyEnv = "MY_KEY")))
        // The same host through the other protocol is not "another host".
        assertNull(ProviderPreset.strayKey(AgentSettings(provider = "anthropic", baseUrl = "https://api.minimax.io/anthropic", apiKeyEnv = "MINIMAX_API_KEY")))
    }

    @Test
    fun `provider presets are only values the device accepts`() {
        assertEquals(ProviderPreset.ALL.size, ProviderPreset.ALL.map { it.name }.toSet().size)
        assertEquals("one key name per provider", ProviderPreset.ALL.size, ProviderPreset.ALL.map { it.keyEnv }.toSet().size)
        assertTrue(ProviderPreset.ALL.map { it.name }.containsAll(listOf("Anthropic", "OpenRouter", "MiniMax", "OpenAI")))
        for (preset in ProviderPreset.ALL) {
            assertTrue(preset.name, preset.provider in AgentSettings.PROVIDERS)
            assertTrue(preset.name, ConfigRules.isValidSecretName(preset.keyEnv))
            assertTrue(preset.name, ConfigRules.isValidBaseUrl(preset.baseUrl))
            assertEquals("${preset.name}: the device appends /chat/completions itself", preset.baseUrl, ConfigRules.normalizeBaseUrl(preset.baseUrl))
            assertTrue("${preset.name} needs an example model", preset.model.isNotBlank())
            // The Anthropic protocol and the two built-in OpenAI-compatible ones use the device's own endpoint.
            if (preset.provider != "openai") assertEquals(preset.name, "", preset.baseUrl)
            // Exactly one preset is recognised for its own settings.
            assertEquals(preset, ProviderPreset.matching(preset.applyTo(AgentSettings())))
        }

        // Switching provider replaces endpoint, key name and model together...
        val claude = AgentSettings(provider = "anthropic", model = "claude-sonnet-5-5", apiKeyEnv = "ANTHROPIC_API_KEY", maxSteps = 40)
        val minimax = ProviderPreset.ALL.first { it.name == "MiniMax" }
        val switched = minimax.applyTo(claude)
        assertEquals("openai", switched.provider)
        assertEquals(minimax.baseUrl, switched.baseUrl)
        assertEquals("MINIMAX_API_KEY", switched.apiKeyEnv)
        assertEquals(minimax.model, switched.model)
        assertEquals("the rest of the settings is untouched", 40, switched.maxSteps)
        // ...and tapping the preset that is already active keeps a model the user typed.
        val tuned = switched.copy(model = "my-own-model", baseUrl = minimax.baseUrl + "/")
        assertEquals(tuned.copy(baseUrl = minimax.baseUrl), minimax.applyTo(tuned))
        // Found on a real device: MiniMax's endpoint typed by hand with the protocol left on
        // anthropic and the key stored under the user's own name. The preset repairs the
        // protocol and keeps that key name and model, so the stored key keeps working.
        val byHand = AgentSettings(provider = "anthropic", model = "MiniMax-M3", apiKeyEnv = "MiniMaxAPI", baseUrl = "https://api.minimax.io/v1/")
        assertNull(ProviderPreset.matching(byHand))
        val repaired = minimax.applyTo(byHand)
        assertEquals(AgentSettings(provider = "openai", model = "MiniMax-M3", apiKeyEnv = "MiniMaxAPI", baseUrl = "https://api.minimax.io/v1"), repaired)
        assertEquals(minimax, ProviderPreset.matching(repaired))
        // Anything else is simply "custom": no preset claims it.
        assertNull(ProviderPreset.matching(AgentSettings(provider = "openai", baseUrl = "https://llm.example.internal/v1")))

        // Tapping the preset that is already highlighted changes nothing, whatever the key is called.
        val own = AgentSettings(provider = "anthropic", model = "claude-opus-5", apiKeyEnv = "MY_ANTHROPIC_KEY")
        val anthropic = ProviderPreset.ALL.first { it.name == "Anthropic" }
        assertEquals(anthropic, ProviderPreset.matching(own))
        assertEquals(own, anthropic.applyTo(own))
        // The device's built-in URL typed out, or the full endpoint pasted, is still that provider.
        val openrouter = ProviderPreset.ALL.first { it.name == "OpenRouter" }
        val explicit = AgentSettings(provider = "openrouter", model = "x/y", apiKeyEnv = "OR", baseUrl = "https://openrouter.ai/api/v1")
        assertEquals(openrouter, ProviderPreset.matching(explicit))
        assertEquals(explicit.copy(baseUrl = ""), openrouter.applyTo(explicit))
        assertEquals(minimax, ProviderPreset.matching(AgentSettings(provider = "openai", baseUrl = "https://api.minimax.io/v1/chat/completions")))
    }

    @Test
    fun `saving agent settings sends the chosen provider and refuses a key as a name`() = runBlocking<Unit> {
        sync()
        val device = """{"api": {"port": 8421},
            "llm": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key_env": "ANTHROPIC_API_KEY", "effort": "medium",
                    "base_url": "", "max_steps": 25, "request_timeout_sec": 120, "future_knob": 7},
            "kvm": {"stuck_threshold": 3, "vnc_idle_disconnect_sec": 60, "jpeg_quality": 75},
            "security": {"approval_default_tier": "balanced"}}"""
        router.json("GET", "/v1/config", """{"config": $device}""")
        router.on("PUT", "/v1/config") { request -> dev.lapiszero.app.jsonResponse(request.body!!.utf8()) }
        val vm = ConfigViewModel(server.api(), repo)
        vm.loadConfig()
        val loaded = AgentSettings.from(vm.config.await { it.data != null }.data!!)

        // MiniMax chosen, with the full endpoint pasted over the preset's base URL.
        val minimax = ProviderPreset.ALL.first { it.name == "MiniMax" }
        var saved: Boolean? = null
        vm.saveSettings(minimax.applyTo(loaded).copy(baseUrl = "https://api.minimax.io/v1/chat/completions/", apiKeyEnv = " MINIMAX_API_KEY ")) { saved = it }
        awaitTrue { saved != null }
        assertEquals(true, saved)
        val sent = LapisJson.parseToJsonElement(router.take("PUT /v1/config").body!!.utf8()).jsonObject.getValue("config").jsonObject
        val llm = sent.getValue("llm").jsonObject
        assertEquals("openai", llm.getValue("provider").jsonPrimitive.content)
        assertEquals("https://api.minimax.io/v1", llm.getValue("base_url").jsonPrimitive.content)
        assertEquals("MINIMAX_API_KEY", llm.getValue("api_key_env").jsonPrimitive.content)
        assertEquals("MiniMax-M3.1-Flash", llm.getValue("model").jsonPrimitive.content)
        assertEquals("keys this app does not know are sent back", "7", llm.getValue("future_knob").jsonPrimitive.content)
        assertEquals("""{"port":8421}""", sent.getValue("api").toString())
        assertEquals("75", sent.getValue("kvm").jsonObject.getValue("jpeg_quality").jsonPrimitive.content)
        assertEquals(minimax, ProviderPreset.matching(AgentSettings.from(vm.config.value.data!!)))

        // A key where the name belongs never reaches the device, and is not repeated in the reason.
        val puts = router.count("PUT /v1/config")
        for (key in keyShaped) {
            saved = null
            vm.saveSettings(AgentSettings.from(vm.config.value.data!!).copy(apiKeyEnv = key)) { saved = it }
            assertEquals(false, saved)
            assertEquals(ConfigRules.NOT_A_NAME, vm.lastFailure.value)
        }
        saved = null
        vm.saveSettings(AgentSettings.from(vm.config.value.data!!).copy(baseUrl = "https://user:pw@host/v1")) { saved = it }
        assertEquals(false, saved)
        delay(100)
        assertEquals(puts, router.count("PUT /v1/config"))
    }

    @Test
    fun `secrets are write-only and never echoed`() = runBlocking<Unit> {
        sync()
        router.json("GET", "/v1/secrets/status", """{"secrets": [{"name": "ANTHROPIC_API_KEY", "is_set": false, "masked": null}]}""")
        router.json("PATCH", "/v1/secrets", """{"secrets": [{"name": "ANTHROPIC_API_KEY", "is_set": true, "masked": "...9f2a"}]}""")
        val vm = ConfigViewModel(server.api(), repo)
        val messages = collect(vm.messages)
        vm.loadSecrets()
        assertFalse(vm.secrets.await { it.data != null }.data!!.single().isSet)

        vm.setSecret("ANTHROPIC_API_KEY", "sk-ant-api03-very-secret-9f2a")
        val after = vm.secrets.await { it.data?.single()?.isSet == true }.data!!.single()
        assertEquals("...9f2a", after.masked)
        assertTrue(router.take("PATCH /v1/secrets").body!!.utf8().contains("sk-ant-api03-very-secret-9f2a"))
        messages.awaitSize(1)
        assertFalse("the value is not repeated in UI state or messages", (vm.secrets.value.toString() + messages).contains("very-secret"))

        // Invalid input never reaches the device, and the reason is kept for the open dialog.
        assertNull(vm.lastFailure.value)
        vm.setSecret("BAD NAME", "x")
        assertTrue(vm.lastFailure.value!!, vm.lastFailure.value!!.contains("letters, digits and _"))
        vm.setSecret("OK_NAME", "")
        assertEquals("A secret must be a single non-empty line", vm.lastFailure.value)
        vm.clearFailure()
        vm.setSecret("OK_NAME", "   ")
        assertEquals("A secret must be a single non-empty line", vm.lastFailure.value)
        vm.setSecret("OK_NAME", "two\nlines")
        assertEquals("A secret must be a single non-empty line", vm.lastFailure.value)
        // A key given as the NAME of a new secret would be listed in the clear: refused.
        for (key in keyShaped) {
            vm.clearFailure()
            vm.setSecret(ConfigRules.normalizeSecretName(key), "x")
            assertEquals(ConfigRules.NOT_A_NAME, vm.lastFailure.value)
        }
        delay(100)
        assertEquals(1, router.count("PATCH /v1/secrets"))
    }

    @Test
    fun `a key for any provider is stored under its own name`() = runBlocking<Unit> {
        sync()
        router.json("GET", "/v1/secrets/status", """{"secrets": []}""")
        router.json("PATCH", "/v1/secrets", """{"secrets": [{"name": "MINIMAX_API_KEY", "is_set": true, "masked": "...c0de"}]}""")
        val vm = ConfigViewModel(server.api(), repo)
        vm.loadSecrets()
        vm.secrets.await { it.data != null }

        // Pasted with the line break that copying a key usually brings along.
        vm.setSecret("MINIMAX_API_KEY", "mm-test-key-000000c0de\n")
        vm.secrets.await { it.data?.singleOrNull()?.isSet == true }
        val sent = LapisJson.parseToJsonElement(router.take("PATCH /v1/secrets").body!!.utf8()).jsonObject
        assertEquals("mm-test-key-000000c0de", sent.getValue("set").jsonObject.getValue("MINIMAX_API_KEY").jsonPrimitive.content)
        assertNull(vm.lastFailure.value)

        // A refusal from the device is available to the dialog that is still open.
        router.on("PATCH", "/v1/secrets") { errorResponse(400, "bad_request", "Invalid secret: 'X'") }
        var result: Boolean? = null
        vm.setSecret("OPENROUTER_API_KEY", "or-test-key") { result = it }
        vm.lastFailure.await { it != null }
        assertEquals(false, result)
        assertTrue(vm.lastFailure.value!!, vm.lastFailure.value!!.startsWith("Could not store the secret"))
        assertFalse("the value is never part of an error", vm.lastFailure.value!!.contains("or-test-key"))
        vm.clearFailure()
        assertNull(vm.lastFailure.value)
    }

    @Test
    fun `a host's VNC password goes to the vault and only its name into the host`() = runBlocking<Unit> {
        sync()
        router.json("GET", "/v1/hosts", """{"hosts": {}, "active_host": ""}""")
        router.json("PATCH", "/v1/secrets", """{"secrets": [{"name": "LAPIS_VNC_PASSWORD_mac_mini", "is_set": true, "masked": "..."}]}""")
        router.on("PUT", "/v1/hosts") { request ->
            val hosts = LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject.getValue("hosts")
            dev.lapiszero.app.jsonResponse("""{"hosts": $hosts, "active_host": "mac-mini"}""")
        }
        val vm = ConfigViewModel(server.api(), repo)
        vm.loadHosts()
        vm.hosts.await { it.data != null }
        val name = ConfigRules.vncSecretName("mac-mini", "", emptySet())
        val host = dev.lapiszero.app.data.remote.Host(
            hostname = "mac-mini", tailscaleIp = "100.64.0.9", os = "macos", policy = "strict",
            vnc = dev.lapiszero.app.data.remote.HostVnc(5900, "alice", name),
            ssh = dev.lapiszero.app.data.remote.HostSsh(22, "alice", null),
        )

        var saved: Boolean? = null
        vm.saveHost(null, "mac-mini", host, "correct horse 42\n") { saved = it }
        awaitTrue { saved != null }
        assertEquals(true, saved)
        // The secret is stored first (so a failure there never leaves a host pointing at nothing)...
        val secret = LapisJson.parseToJsonElement(router.take("PATCH /v1/secrets").body!!.utf8()).jsonObject
        assertEquals("correct horse 42", secret.getValue("set").jsonObject.getValue(name).jsonPrimitive.content)
        // ...and the host carries the name only.
        val put = router.take("PUT /v1/hosts").body!!.utf8()
        assertTrue(put.contains(name))
        assertFalse("the password is not part of the host", put.contains("correct horse"))
        assertTrue(vm.secrets.value.data!!.single().isSet)

        // A password typed where a name belongs never reaches the device as a "name".
        vm.saving.await { !it }
        val puts = router.count("PUT /v1/hosts")
        vm.saveHost("mac-mini", "mac-mini", host.copy(vnc = host.vnc.copy(passwordEnv = "p@ss-word")))
        assertEquals("The VNC password must be stored under a name made of letters, digits and _", vm.lastFailure.value)
        delay(100)
        assertEquals(puts, router.count("PUT /v1/hosts"))
        // No password given: nothing is written to the vault, the host is still saved.
        val patches = router.count("PATCH /v1/secrets")
        vm.saveHost("mac-mini", "mac-mini", host.copy(policy = "balanced"))
        vm.hosts.await { it.data?.hosts?.get("mac-mini")?.policy == "balanced" }
        assertEquals(patches, router.count("PATCH /v1/secrets"))
    }

    @Test
    fun `hosts are saved as the full map and probed on demand`() = runBlocking<Unit> {
        sync()
        val hostsJson = """{"macbook-pro": {"hostname": "macbook-pro", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
            "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_macbook_pro"}, "ssh": {"port": 22, "user": "alice", "identity_file": null}}}"""
        router.json("GET", "/v1/hosts", """{"hosts": $hostsJson, "active_host": "macbook-pro"}""")
        router.on("PUT", "/v1/hosts") { request ->
            val hosts = LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject.getValue("hosts")
            dev.lapiszero.app.jsonResponse("""{"hosts": $hosts, "active_host": "macbook-pro"}""")
        }
        router.json("POST", "/v1/hosts/macbook-pro/probe", """{"host_id": "macbook-pro", "tailscale": {"ok": true, "latency_ms": 12, "detail": "mock"}, "vnc": {"ok": true, "latency_ms": 85, "detail": "mock"}, "ssh": {"ok": false, "latency_ms": null, "detail": "TimeoutError"}}""")
        val vm = ConfigViewModel(server.api(), repo)
        val messages = collect(vm.messages)
        vm.loadHosts()
        val original = vm.hosts.await { it.data != null }.data!!.hosts.getValue("macbook-pro")

        var saved: Boolean? = null
        vm.saveHost(null, "arch-dev", original.copy(hostname = "arch-dev", os = "linux")) { saved = it }
        assertEquals(setOf("macbook-pro", "arch-dev"), vm.hosts.await { it.data?.hosts?.size == 2 }.data!!.hosts.keys)
        awaitTrue { saved != null }
        assertEquals(true, saved)
        val put = LapisJson.parseToJsonElement(router.take("PUT /v1/hosts").body!!.utf8()).jsonObject.getValue("hosts").jsonObject
        assertEquals("the existing host is sent back untouched", "alice", put.getValue("macbook-pro").jsonObject.getValue("vnc").jsonObject.getValue("username").jsonPrimitive.content)

        // Renaming replaces the key; bad ids and duplicates are refused locally.
        vm.saving.await { !it }
        vm.saveHost("arch-dev", "arch-box", original.copy(hostname = "arch-box"))
        assertEquals(setOf("macbook-pro", "arch-box"), vm.hosts.await { it.data?.hosts?.containsKey("arch-box") == true }.data!!.hosts.keys)
        vm.saving.await { !it }
        val puts = router.count("PUT /v1/hosts")
        vm.saveHost(null, "bad id", original)
        vm.saveHost(null, "macbook-pro", original)
        delay(100)
        assertEquals(puts, router.count("PUT /v1/hosts"))
        assertTrue(messages.any { it.contains("already exists") })

        vm.probe("macbook-pro")
        val probe = vm.probes.await { it["macbook-pro"] is dev.lapiszero.app.ui.config.ProbeUi.Done }["macbook-pro"] as dev.lapiszero.app.ui.config.ProbeUi.Done
        assertTrue(probe.result.tailscale.ok)
        assertFalse(probe.result.ssh.ok)
    }

    @Test
    fun `schedules run now opens the resulting chat and bad cron stays local`() = runBlocking<Unit> {
        sync()
        router.json("GET", "/v1/schedules", """{"schedules": []}""")
        router.json("POST", "/v1/schedules/Daily%20Digest/run", """{"chat_id": "c_run", "task_id": "t_run"}""", code = 202)
        router.on("POST", "/v1/schedules") { errorResponse(422, "invalid", "bad cron field: minute") }
        val vm = ConfigViewModel(server.api(), repo)
        val messages = collect(vm.messages)
        val opened = collect(vm.openChat)
        vm.loadSchedules()
        vm.schedules.await { it.data != null }
        vm.runSchedule("Daily Digest")
        opened.awaitSize(1)
        assertEquals("c_run", opened.single())
        vm.saving.await { !it }

        vm.saveSchedule(null, dev.lapiszero.app.data.remote.ScheduleCreate("n", "not a cron", "macbook-pro", "p", true))
        delay(100)
        assertEquals(0, router.count("POST /v1/schedules"))
        vm.saveSchedule(null, dev.lapiszero.app.data.remote.ScheduleCreate("n", "61 8 * * *", "macbook-pro", "p", true))
        awaitTrue { messages.any { it.contains("bad cron field") } } // the daemon's verdict is shown
    }


    // ------------------------------------------------------------------ SSH login, device key, agent tools

    /** Device software that has the SSH features answers for its key; older software says 404. */
    private fun newerDevice() = router.json("GET", "/v1/device/ssh-key", Fixtures.SSH_KEY)

    /** The device stores the hosts it is sent and answers with them. */
    private fun echoHosts() = router.on("PUT", "/v1/hosts") { request ->
        jsonResponse("""{"hosts": ${LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject.getValue("hosts")}, "active_host": ""}""")
    }

    private fun json(key: String): JsonObject = LapisJson.parseToJsonElement(router.take(key).body!!.utf8()).jsonObject

    private val claude = HostAgent("claude", "Claude Code", "/opt/homebrew/bin/claude")

    @Test
    fun `ssh secret names and the authorise command`() {
        // An SSH password of the host's own gets a name of its own, by the rules of the VNC one.
        val vnc = "LAPIS_VNC_PASSWORD_mac_mini"
        assertEquals("LAPIS_SSH_PASSWORD_mac_mini", ConfigRules.sshSecretName("mac-mini", "", emptySet(), setOf(vnc)))
        assertEquals("kept: it really is a stored secret", "MY_SSH", ConfigRules.sshSecretName("mac-mini", "MY_SSH", setOf("MY_SSH"), setOf(vnc)))
        assertEquals("kept while the vault is unread", "MY_SSH", ConfigRules.sshSecretName("mac-mini", "MY_SSH", null, setOf(vnc)))
        // A password that passes for a name is not a stored secret: replaced, never kept as the name.
        assertEquals("LAPIS_SSH_PASSWORD_mac_mini", ConfigRules.sshSecretName("mac-mini", "Hunter2abc", setOf(vnc), setOf(vnc)))
        assertEquals("LAPIS_SSH_PASSWORD_mac_mini", ConfigRules.sshSecretName("mac-mini", "p@ss word", null, setOf(vnc)))
        // Unique among hosts, like the VNC one.
        assertEquals("LAPIS_SSH_PASSWORD_mac_2", ConfigRules.sshSecretName("mac", "", emptySet(), setOf("LAPIS_SSH_PASSWORD_mac")))
        assertTrue(ConfigRules.isValidSecretName(ConfigRules.sshSecretName("x".repeat(64), "", emptySet())))
        // The VNC rule itself is unchanged by sharing its code.
        assertEquals(vnc, ConfigRules.vncSecretName("mac-mini", "", emptySet()))

        assertEquals(
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo '${Fixtures.PUBLIC_KEY}' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys",
            ConfigRules.authorizeCommand(Fixtures.PUBLIC_KEY + "\n"),
        )
        assertTrue(ConfigRules.authorizeCommand("ssh-rsa AAAAB3NzaC1yc2E+/x==")!!.contains("echo 'ssh-rsa AAAAB3NzaC1yc2E+/x=='"))
        // It is pasted into a shell between single quotes: nothing that could leave them.
        for (bad in listOf("", "ssh-ed25519", "ssh-ed25519 AAAA' ; rm -rf ~ ; echo '", "ssh-ed25519 AAAA\nssh-ed25519 BBBB", "ssh-ed25519 AAAA \$(id)", "ssh-ed25519 AAAA a`id`")) {
            assertNull(bad, ConfigRules.authorizeCommand(bad))
        }
        assertEquals("AGENT", kindLabel("agent_run"))
    }

    @Test
    fun `one login for screen and shell is one vault write and names only in the host`() = runBlocking<Unit> {
        sync()
        newerDevice()
        router.json("GET", "/v1/hosts", """{"hosts": {}, "active_host": ""}""")
        router.json("PATCH", "/v1/secrets", """{"secrets": [{"name": "LAPIS_VNC_PASSWORD_mac_mini", "is_set": true, "masked": "..."}]}""")
        echoHosts()
        val vm = ConfigViewModel(server.api(), repo)
        val messages = collect(vm.messages)
        vm.loadHosts()
        vm.loadDeviceKey()
        vm.hosts.await { it.data != null }
        vm.sshSupport.await { it is SshSupport.Ready }
        val vncName = ConfigRules.vncSecretName("mac-mini", "", emptySet())
        val sshName = ConfigRules.sshSecretName("mac-mini", "", emptySet(), setOf(vncName))
        fun host(ssh: HostSsh) = Host("mac-mini", "100.64.0.9", "macos", "strict", HostVnc(5900, "alice", vncName), ssh)

        // "Same login as the screen" on: the one password typed is stored once, under the VNC
        // name, and the SSH part of the host points at that same name.
        var saved: Boolean? = null
        vm.saveHost(null, "mac-mini", host(HostSsh(22, "alice", null, vncName)), "screen pass 1\n") { saved = it }
        awaitTrue { saved != null }
        assertEquals(true, saved)
        assertEquals(mapOf(vncName to "screen pass 1"), json("PATCH /v1/secrets").getValue("set").jsonObject.mapValues { it.value.jsonPrimitive.content })
        var ssh = json("PUT /v1/hosts").getValue("hosts").jsonObject.getValue("mac-mini").jsonObject.getValue("ssh").jsonObject
        assertEquals("""{"port":22,"user":"alice","password_env":"$vncName"}""", ssh.toString())

        // Off: an SSH login of its own. Both passwords still travel in ONE vault write, before the host.
        vm.saving.await { !it }
        saved = null
        val patches = router.count("PATCH /v1/secrets")
        vm.saveHost("mac-mini", "mac-mini", host(HostSsh(22, "deploy", null, sshName)), "screen pass 2", "shell pass 3\n") { saved = it }
        awaitTrue { saved != null }
        assertEquals(true, saved)
        assertEquals(patches + 1, router.count("PATCH /v1/secrets"))
        assertEquals(
            mapOf(vncName to "screen pass 2", sshName to "shell pass 3"),
            json("PATCH /v1/secrets").getValue("set").jsonObject.mapValues { it.value.jsonPrimitive.content },
        )
        ssh = json("PUT /v1/hosts").getValue("hosts").jsonObject.getValue("mac-mini").jsonObject.getValue("ssh").jsonObject
        assertEquals("""{"port":22,"user":"deploy","password_env":"$sshName"}""", ssh.toString())
        assertTrue("the vault write comes first", router.log.lastIndexOf("PATCH /v1/secrets") < router.log.lastIndexOf("PUT /v1/hosts"))

        // Key only: no password, no name, no vault write.
        vm.saving.await { !it }
        saved = null
        vm.saveHost("mac-mini", "mac-mini", host(HostSsh(22, "deploy", null, ""))) { saved = it }
        awaitTrue { saved != null }
        assertEquals("", json("PUT /v1/hosts").getValue("hosts").jsonObject.getValue("mac-mini").jsonObject.getValue("ssh").jsonObject.getValue("password_env").jsonPrimitive.content)
        assertEquals(patches + 1, router.count("PATCH /v1/secrets"))

        // Refused before anything is sent: a password of its own under the name shared with
        // VNC (it would replace the screen's), a password where the NAME belongs, two lines.
        vm.saving.await { !it }
        val sent = router.log.size
        vm.saveHost("mac-mini", "mac-mini", host(HostSsh(22, "alice", null, vncName)), null, "shell pass 4")
        assertEquals("The SSH password must be a single line, stored under a name of its own", vm.lastFailure.value)
        vm.saveHost("mac-mini", "mac-mini", host(HostSsh(22, "alice", null, "")), null, "shell pass 5")
        assertEquals("The SSH password must be a single line, stored under a name of its own", vm.lastFailure.value)
        vm.saveHost("mac-mini", "mac-mini", host(HostSsh(22, "alice", null, sshName)), null, "shell\npass 6")
        assertEquals("The SSH password must be a single line, stored under a name of its own", vm.lastFailure.value)
        vm.saveHost("mac-mini", "mac-mini", host(HostSsh(22, "alice", null, "shell pass 7")))
        assertEquals("The SSH password must be stored under a name made of letters, digits and _", vm.lastFailure.value)
        delay(100)
        assertEquals(sent, router.log.size)

        // No password left in any request but the vault write, nor in UI state or messages.
        val passwords = (1..7).map { "pass $it" }
        assertEquals(setOf("PATCH /v1/secrets"), router.bodies.filter { (_, body) -> passwords.any(body::contains) }.map { it.first }.toSet())
        val shown = "${vm.hosts.value} ${vm.secrets.value} ${vm.lastFailure.value} ${vm.sshSupport.value} $messages"
        assertTrue(shown, passwords.none(shown::contains))
        assertTrue(messages.toString(), messages.count { it == "Host mac-mini saved" } == 3)
    }

    @Test
    fun `an existing host goes back to the device as it came`() = runBlocking<Unit> {
        sync()
        newerDevice()
        // One with the screen's login shared, one from before the field existed, one key only.
        val hostsJson = """{"shared": {"hostname": "shared", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
              "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_shared"},
              "ssh": {"port": 22, "user": "alice", "password_env": "LAPIS_VNC_PASSWORD_shared"}},
            "before": {"hostname": "before", "tailscale_ip": "100.64.10.22", "os": "linux", "policy": "strict",
              "vnc": {"port": 5901, "username": "dev", "password_env": "LAPIS_VNC_PASSWORD_before"},
              "ssh": {"port": 2222, "user": "dev", "identity_file": "/etc/lapis/lapis_ed25519"}},
            "keyonly": {"hostname": "keyonly", "tailscale_ip": "100.64.10.23", "os": "linux", "policy": "strict",
              "vnc": {"port": 5900, "username": "", "password_env": ""},
              "ssh": {"port": 22, "user": "ops", "password_env": ""}}}"""
        router.json("GET", "/v1/hosts", """{"hosts": $hostsJson, "active_host": "shared"}""")
        echoHosts()
        val vm = ConfigViewModel(server.api(), repo)
        vm.loadHosts()
        vm.loadDeviceKey()
        val loaded = vm.hosts.await { it.data != null }.data!!.hosts
        vm.sshSupport.await { it is SshSupport.Ready }

        var saved: Boolean? = null
        vm.saveHost("shared", "shared", loaded.getValue("shared")) { saved = it }
        awaitTrue { saved != null }
        assertEquals(true, saved)
        assertEquals(LapisJson.parseToJsonElement(hostsJson), json("PUT /v1/hosts").getValue("hosts"))
        assertEquals(0, router.count("PATCH /v1/secrets"))
    }

    @Test
    fun `authorising the device says what happened and keeps a refusal for the dialog`() = runBlocking<Unit> {
        sync()
        newerDevice()
        val vm = ConfigViewModel(server.api(), repo)
        val messages = collect(vm.messages)
        vm.loadDeviceKey()
        val key = (vm.sshSupport.await { it is SshSupport.Ready } as SshSupport.Ready).key
        // The device-key dialog: the fingerprint, the public key and the one line to run by hand.
        assertEquals(Fixtures.FINGERPRINT, key.fingerprint)
        assertEquals(Fixtures.PUBLIC_KEY, key.publicKey)
        assertTrue(ConfigRules.authorizeCommand(key.publicKey)!!.contains("echo '${Fixtures.PUBLIC_KEY}' >> ~/.ssh/authorized_keys"))

        router.json("POST", "/v1/hosts/macbook-pro/ssh/authorize", """{"ok": true, "already": false, "detail": "key added for alice"}""")
        var done: Boolean? = null
        vm.authorizeDevice("macbook-pro") { done = it }
        awaitTrue { done != null }
        assertEquals(true, done)
        assertEquals("{}", router.take("POST /v1/hosts/macbook-pro/ssh/authorize").body!!.utf8())
        messages.awaitSize(1)
        assertEquals("Device authorised on macbook-pro", messages.last())
        assertNull(vm.lastFailure.value)

        router.json("POST", "/v1/hosts/macbook-pro/ssh/authorize", """{"ok": true, "already": true, "detail": "already authorised"}""")
        done = null
        vm.authorizeDevice("macbook-pro") { done = it }
        awaitTrue { done != null }
        messages.awaitSize(2)
        assertEquals("The device was already authorised on macbook-pro", messages.last())

        // No login works: the reason the device gives is kept where the open dialog reads it.
        router.on("POST", "/v1/hosts/macbook-pro/ssh/authorize") {
            errorResponse(422, "ssh_login_failed", "No login works for alice@100.64.10.21: turn on Remote Login or store its password")
        }
        done = null
        vm.authorizeDevice("macbook-pro") { done = it }
        awaitTrue { done != null }
        assertEquals(false, done)
        assertEquals(
            "Could not authorise the device: No login works for alice@100.64.10.21: turn on Remote Login or store its password",
            vm.lastFailure.value,
        )
        // An answer that is not a yes is not reported as one.
        router.json("POST", "/v1/hosts/macbook-pro/ssh/authorize", """{"ok": false, "already": false, "detail": "authorized_keys is not writable"}""")
        done = null
        vm.authorizeDevice("macbook-pro") { done = it }
        awaitTrue { done != null }
        assertEquals(false, done)
        assertEquals("Could not authorise the device: authorized_keys is not writable", vm.lastFailure.value)
        assertEquals(2, messages.count { it.contains("authorised on") })
    }

    @Test
    fun `older device software gets nothing it does not know`() = runBlocking<Unit> {
        sync() // no route for /v1/device/ssh-key: 404, like the daemon before the SSH features
        val hostJson = """{"hostname": "macbook-pro", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
            "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_macbook_pro"}, "ssh": {"port": 22, "user": "alice"}}"""
        router.json("GET", "/v1/hosts", """{"hosts": {"macbook-pro": $hostJson}, "active_host": "macbook-pro"}""")
        router.json("PATCH", "/v1/secrets", """{"secrets": []}""")
        echoHosts()
        val config = ConfigViewModel(server.api(), repo)
        config.loadHosts()
        config.loadDeviceKey()
        val host = config.hosts.await { it.data != null }.data!!.hosts.getValue("macbook-pro")
        assertEquals(SshSupport.Missing, config.sshSupport.await { it == SshSupport.Missing })
        assertNull(host.ssh.passwordEnv)

        // The form does not offer them there; the view model would not send them either.
        config.saveHost("macbook-pro", "macbook-pro", host.copy(ssh = host.ssh.copy(passwordEnv = "LAPIS_VNC_PASSWORD_macbook_pro")))
        assertEquals(ChatRepository.NO_SSH_FEATURES, config.lastFailure.value)
        config.clearFailure()
        config.saveHost("macbook-pro", "macbook-pro", host.copy(ssh = host.ssh.copy(passwordEnv = "")))
        assertEquals(ChatRepository.NO_SSH_FEATURES, config.lastFailure.value)
        config.clearFailure()
        config.saveHost("macbook-pro", "macbook-pro", host, null, "shell pass")
        assertTrue("${config.lastFailure.value}", config.lastFailure.value?.contains("shell pass") == false)
        config.clearFailure()
        delay(100)
        assertEquals(0, router.count("PUT /v1/hosts") + router.count("PATCH /v1/secrets"))

        // What it does know still works, and goes back without the new field.
        var saved: Boolean? = null
        config.saveHost("macbook-pro", "macbook-pro", host.copy(policy = "strict"), "screen pass") { saved = it }
        awaitTrue { saved != null }
        assertEquals(true, saved)
        val put = json("PUT /v1/hosts").getValue("hosts").jsonObject.getValue("macbook-pro").jsonObject
        assertEquals("""{"port":22,"user":"alice"}""", put.getValue("ssh").toString())
        assertFalse(put.toString().contains("screen pass"))

        // A thread: the menu is there but closed, with the one line that says why.
        val vm = ChatThreadViewModel(repo, chatId)
        val messages = collect(vm.messages)
        val runWith = vm.runWith.await { !it.enabled }
        assertEquals(RunWith.LAPIS, runWith.current)
        assertEquals(listOf(RunWith.LAPIS), runWith.options)
        assertEquals(ChatRepository.NO_SSH_FEATURES, runWith.note)
        assertFalse(runWith.refreshable)
        vm.setAgent("claude")
        vm.setWorkdir("~/src")
        messages.awaitSize(2)
        assertEquals(listOf(ChatRepository.NO_SSH_FEATURES, ChatRepository.NO_SSH_FEATURES), messages.toList())
        vm.refreshAgents()
        delay(100)
        assertEquals(0, router.count("PATCH /v1/chats/$chatId"))
        assertEquals(0, router.count("GET /v1/hosts/macbook-pro/agents"))
        assertTrue("${router.bodies}", router.bodies.none { (_, body) -> "\"agent\"" in body || "workdir" in body })

        // Creating a chat stays what it was.
        router.json("POST", "/v1/chats", Fixtures.chat(id = "c_new"), code = 201)
        repo.createChat("macbook-pro")
        assertEquals("""{"host_id":"macbook-pro"}""", router.take("POST /v1/chats").body!!.utf8())
    }

    @Test
    fun `run with offers the tools found on the host and a choice patches the chat`() = runBlocking<Unit> {
        sync()
        newerDevice()
        router.json("GET", "/v1/hosts/macbook-pro/agents", Fixtures.AGENTS)
        router.json("GET", "/v1/chats/$chatId", """{"chat": ${Fixtures.chat(agent = "lapis")}, "messages": [], "has_more": false}""")
        val vm = ChatThreadViewModel(repo, chatId)
        val messages = collect(vm.messages)

        val menu = vm.runWith.await { it.refreshable }
        assertEquals(listOf("lapis", "claude", "opencode", "freya"), menu.options.map { it.id })
        assertEquals(listOf("Lapis agent", "Claude Code", "opencode", "Freya"), menu.options.map { it.name })
        assertEquals(RunWith.LAPIS, menu.current)
        assertFalse(menu.usesTool)
        assertNull(menu.note)
        assertTrue(menu.enabled)

        router.json("PATCH", "/v1/chats/$chatId", Fixtures.chat(agent = "claude"))
        vm.setAgent("claude")
        assertEquals("""{"agent":"claude"}""", router.take("PATCH /v1/chats/$chatId").body!!.utf8())
        val chosen = vm.runWith.await { it.usesTool }
        assertEquals(claude, chosen.current)
        assertEquals("", chosen.workdir)

        // The folder: plain text, trimmed, capped in length and nothing else.
        router.json("PATCH", "/v1/chats/$chatId", Fixtures.chat(agent = "claude", workdir = "~/projects/lapis zero"))
        vm.setWorkdir("  ~/projects/lapis zero \n")
        assertEquals("""{"workdir":"~/projects/lapis zero"}""", router.take("PATCH /v1/chats/$chatId").body!!.utf8())
        assertEquals("~/projects/lapis zero", vm.runWith.await { it.workdir.isNotEmpty() }.workdir)
        router.json("PATCH", "/v1/chats/$chatId", Fixtures.chat(agent = "claude", workdir = "/capped"))
        vm.setWorkdir("/" + "x".repeat(3000))
        assertEquals(ChatThreadViewModel.MAX_WORKDIR, json("PATCH /v1/chats/$chatId").getValue("workdir").jsonPrimitive.content.length)
        vm.runWith.await { it.workdir == "/capped" }
        router.json("PATCH", "/v1/chats/$chatId", Fixtures.chat(agent = "claude"))
        vm.setWorkdir("")
        assertEquals("""{"workdir":""}""", router.take("PATCH /v1/chats/$chatId").body!!.utf8())
        vm.runWith.await { it.workdir.isEmpty() }

        // The list is kept for the session: another thread on the same host does not ask again...
        router.json("GET", "/v1/chats/$chatId", """{"chat": ${Fixtures.chat(agent = "claude")}, "messages": [], "has_more": false}""")
        val other = ChatThreadViewModel(repo, chatId)
        other.runWith.await { it.refreshable && it.options.size == 4 }
        delay(100)
        assertEquals(1, router.count("GET /v1/hosts/macbook-pro/agents"))
        assertEquals(1, router.count("GET /v1/device/ssh-key"))
        // ...until it is asked to look again. A tool that is gone by then stays on the chat bound to it.
        router.json("GET", "/v1/hosts/macbook-pro/agents", """{"agents": [{"id": "opencode", "name": "opencode", "path": "/usr/local/bin/opencode"}]}""")
        vm.refreshAgents()
        val after = vm.runWith.await { it.refreshable && it.options.size == 3 }
        assertEquals(2, router.count("GET /v1/hosts/macbook-pro/agents"))
        assertEquals(listOf("lapis", "opencode", "claude"), after.options.map { it.id })
        assertEquals(HostAgent("claude", "claude"), after.current)

        // A refusal from the device is said, and the chat stays as it was.
        router.on("PATCH", "/v1/chats/$chatId") { errorResponse(422, "invalid", "Unknown agent: nope") }
        vm.setAgent("nope")
        messages.awaitSize(1)
        assertEquals("Unknown agent: nope", messages.single())
        assertEquals("claude", vm.runWith.await { true }.current.id)
    }

    @Test
    fun `run with says why only the lapis agent is offered when ssh is down`() = runBlocking<Unit> {
        sync()
        newerDevice()
        val reason = "SSH login failed for alice@100.64.10.21: permission denied (device key, password)"
        router.on("GET", "/v1/hosts/macbook-pro/agents") { errorResponse(409, "ssh_unavailable", reason) }
        val vm = ChatThreadViewModel(repo, chatId)
        val down = vm.runWith.await { it.note != null }
        assertEquals(reason, down.note)
        assertEquals(listOf(RunWith.LAPIS), down.options)
        assertTrue(down.enabled && down.refreshable)

        // SSH repaired (the device authorised, say): the next thread on that host asks again by itself.
        router.json("GET", "/v1/hosts/macbook-pro/agents", Fixtures.AGENTS)
        val next = ChatThreadViewModel(repo, chatId)
        val up = next.runWith.await { it.options.size == 4 }
        assertNull(up.note)
        assertEquals(2, router.count("GET /v1/hosts/macbook-pro/agents"))
    }

    @Test
    fun `a thread closed while the host is asked for its tools does not lose the answer`() = runBlocking<Unit> {
        sync()
        newerDevice()
        router.on("GET", "/v1/hosts/macbook-pro/agents") {
            MockResponse.Builder().code(200).headersDelay(300, TimeUnit.MILLISECONDS).body(Fixtures.AGENTS).build()
        }
        val closed = ChatThreadViewModel(repo, chatId)
        // Asked for, still on its way (the device looks over SSH), and the thread is left.
        awaitTrue { repo.hostAgents.value["macbook-pro"]?.loading == true }
        closed.viewModelScope.cancel()

        val next = ChatThreadViewModel(repo, chatId)
        assertEquals(4, next.runWith.await { it.refreshable }.options.size)
        assertEquals(1, router.count("GET /v1/hosts/macbook-pro/agents"))
    }

    @Test
    fun `run with menu states`() {
        val ready = SshSupport.Ready(dev.lapiszero.app.data.remote.DeviceSshKey(Fixtures.PUBLIC_KEY, Fixtures.FINGERPRINT))
        val lapisChat = LapisJson.decodeFromString<Chat>(Fixtures.chat())
        val freyaChat = LapisJson.decodeFromString<Chat>(Fixtures.chat(agent = "freya", workdir = "~/work"))
        val found = HostAgentsState(agents = listOf(claude))

        // Nothing known yet, and while the list is being fetched: only the current choice.
        assertEquals(RunWith(), buildRunWith(null, null, SshSupport.Unknown()))
        assertEquals(listOf(RunWith.LAPIS), buildRunWith(lapisChat, found.copy(loading = true), ready).options)
        assertEquals(listOf(HostAgent("freya", "freya")), buildRunWith(freyaChat, null, ready).options)
        assertFalse(buildRunWith(lapisChat, found.copy(loading = true), ready).refreshable)

        // A chat bound to a tool that is no longer found keeps showing that tool.
        val bound = buildRunWith(freyaChat, found, ready)
        assertEquals(HostAgent("freya", "freya"), bound.current)
        assertEquals(listOf("lapis", "claude", "freya"), bound.options.map { it.id })
        assertEquals("~/work", bound.workdir)
        assertTrue(bound.usesTool)
        // With SSH down it is offered the Lapis agent and nothing else, with the reason underneath.
        val down = buildRunWith(freyaChat, HostAgentsState(reason = "SSH login failed"), ready)
        assertEquals(listOf("lapis", "freya"), down.options.map { it.id })
        assertEquals("SSH login failed", down.note)
        assertEquals(listOf(RunWith.LAPIS), buildRunWith(lapisChat, HostAgentsState(reason = "SSH login failed"), ready).options)

        // Older device software: closed, whatever else is known.
        val older = buildRunWith(lapisChat, found, SshSupport.Missing)
        assertFalse(older.enabled)
        assertEquals(ChatRepository.NO_SSH_FEATURES, older.note)
        assertEquals(listOf(RunWith.LAPIS), older.options)
    }

    @Test
    fun `chats run by a tool are tagged in the list and show the tool's progress line`() = runBlocking<Unit> {
        val chats = listOf(
            LapisJson.decodeFromString<Chat>(Fixtures.chat()),
            LapisJson.decodeFromString<Chat>(Fixtures.chat(id = "c_tool", agent = "claude")),
        )
        // The device's own agent needs no tag; a tool has its id until the host's list is known.
        assertEquals(listOf(null, "claude"), buildChatRows(chats, null, emptyList(), null).map { it.agentName })
        val known = mapOf("macbook-pro" to HostAgentsState(agents = listOf(claude)))
        assertEquals(listOf(null, "Claude Code"), buildChatRows(chats, null, emptyList(), null, known).map { it.agentName })

        newerDevice()
        router.json("GET", "/v1/hosts/macbook-pro/agents", Fixtures.AGENTS)
        router.json("GET", "/v1/chats", """{"chats": [${Fixtures.chat(agent = "claude")}]}""")
        router.json("GET", "/v1/chats/$chatId", """{"chat": ${Fixtures.chat(agent = "claude")}, "messages": [], "has_more": false}""")
        sync(Fixtures.stateSync("ACTING", activeChatId = chatId, step = 0))
        val list = ChatListViewModel(repo)
        assertEquals("claude", list.ui.await { it.rows.isNotEmpty() }.rows.single().agentName)
        val vm = ChatThreadViewModel(repo, chatId)
        // Opening the thread fetched the host's tools: the list now knows the name.
        assertEquals("Claude Code", list.ui.await { it.rows.singleOrNull()?.agentName == "Claude Code" }.rows.single().agentName)

        assertNull(vm.ui.await { it.running }.progress)
        source.emit(1, Fixtures.activity(step = 0, kind = "vnc_click", summary = "click(1, 2) left"))
        source.emit(1, Fixtures.activity(step = 0, kind = "agent_run", summary = "Reading daemon/src/lapis_daemon/core.py", screenshotId = null))
        assertEquals("Reading daemon/src/lapis_daemon/core.py", vm.ui.await { it.progress != null }.progress)
        // The line of an earlier task is not shown for the one running now.
        source.emit(1, Fixtures.stateSync("ACTING", activeChatId = chatId, step = 0).replace("t_855667000", "t_next"))
        assertNull(vm.ui.await { it.progress == null }.progress)
        source.emit(1, Fixtures.stateSync("IDLE"))
        assertNull(vm.ui.await { !it.running }.progress)
    }

    // ------------------------------------------------------------------ pairing

    private val nonce = "GneYMjL9l4v_GZvPP4A8TukmQs978gPNuFg-W3Q3ZGc"
    private val phone = PhoneInfo("Pixel 9", "Google Pixel 9")

    @Test
    fun `qr pairing confirms then stores the token in the vault`() = runBlocking<Unit> {
        val store = MemorySecureStore()
        val devices = DeviceRepository(store)
        val calls = CopyOnWriteArrayList<Triple<Endpoint, String, PhoneInfo>>()
        val vm = PairingViewModel(
            devices, phone,
            exchange = { endpoint, n, info ->
                calls += Triple(endpoint, n, info)
                PairExchangeResponse("dev_77", "issued-token", "lapis-zero")
            },
        )
        assertTrue(vm.onScanned("lapis://pair?host=lapis-zero.tailnet.ts.net&nonce=$nonce&pin=482917"))
        val confirm = vm.ui.value as PairUi.Confirm
        assertEquals("482917", confirm.payload.pin)
        assertTrue("nothing is exchanged before the user confirms", calls.isEmpty())
        assertFalse("further scans are ignored while confirming", vm.onScanned("lapis://pair?host=other.ts.net&nonce=$nonce&pin=000000"))

        vm.confirm()
        assertEquals(PairUi.Done("lapis-zero"), vm.ui.await { it is PairUi.Done })
        assertEquals(Triple(Endpoint("lapis-zero.tailnet.ts.net", true), nonce, phone), calls.single())
        val device = devices.state.value.active!!
        assertEquals("issued-token", device.token)
        assertEquals("dev_77", device.remoteId)
        assertEquals("https://lapis-zero.tailnet.ts.net", device.endpoint.baseUrl)
        assertTrue(store.blob!!.contains("issued-token"))
    }

    @Test
    fun `qr pairing explains expired codes and rejects junk`() = runBlocking<Unit> {
        val devices = DeviceRepository(MemorySecureStore())
        val vm = PairingViewModel(
            devices, phone,
            exchange = { _, _, _ -> throw LapisApiException(403, "bad_nonce", "Pairing code is invalid, already used, or expired") },
        )
        assertFalse(vm.onScanned("https://example.com"))
        assertTrue(vm.ui.value is PairUi.Failed)
        assertTrue("a failure does not block the next scan", vm.onScanned("lapis://pair?host=127.0.0.1:18431&nonce=$nonce&pin=114773&tls=0"))
        vm.confirm()
        val failed = vm.ui.await { it is PairUi.Failed } as PairUi.Failed
        assertTrue(failed.message.contains("expired"))
        assertTrue(devices.state.value.devices.isEmpty())
        vm.reset()
        assertEquals(PairUi.Idle, vm.ui.value)
    }

    @Test
    fun `dev connect validates input uses http only for loopback and checks the token`() = runBlocking<Unit> {
        val devices = DeviceRepository(MemorySecureStore())
        val seen = CopyOnWriteArrayList<Pair<Endpoint, String>>()
        var answer: () -> Health = { Health(ok = true, version = "0.2.0", hostname = "devbox", mock = true, deviceId = "dev_local") }
        val vm = PairingViewModel(devices, phone, health = { endpoint, token -> seen += endpoint to token; answer() })

        vm.devConnect("not a host!", "t")
        assertTrue(vm.ui.value is PairUi.Failed)
        vm.devConnect("127.0.0.1:8421", "   ")
        assertTrue(vm.ui.value is PairUi.Failed)
        assertTrue(seen.isEmpty())

        vm.devConnect(" http://127.0.0.1:18431/ ", " dev-local-token ")
        assertEquals(PairUi.Done("devbox (mock)"), vm.ui.await { it is PairUi.Done })
        assertEquals(Endpoint("127.0.0.1:18431", tls = false) to "dev-local-token", seen.single())
        assertEquals("http://127.0.0.1:18431", devices.state.value.active?.endpoint?.baseUrl)

        // Anything that is not loopback / emulator goes over TLS, never cleartext.
        vm.reset()
        vm.devConnect("lapis-zero.tailnet.ts.net", "tok")
        vm.ui.await { it is PairUi.Done }
        assertEquals(Endpoint("lapis-zero.tailnet.ts.net", tls = true), seen.last().first)

        vm.reset()
        answer = { throw LapisApiException(401, "unauthorized", "Missing, invalid or revoked bearer token") }
        vm.devConnect("10.0.2.2:8421", "wrong")
        assertEquals(PairUi.Failed("The daemon rejected that token"), vm.ui.await { it is PairUi.Failed })
        answer = { throw IOException("boom") }
        vm.devConnect("10.0.2.2:8421", "tok")
        assertEquals(PairUi.Failed("boom"), vm.ui.await { it is PairUi.Failed && it.message == "boom" })
        assertEquals(2, devices.state.value.devices.size)
    }
}
