package dev.lapiszero.app

import dev.lapiszero.app.data.remote.AgentState
import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.LapisWsClient
import dev.lapiszero.app.data.remote.Message
import dev.lapiszero.app.data.remote.PowerAction
import dev.lapiszero.app.data.remote.UiPatch
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.DecideResult
import dev.lapiszero.app.domain.Notice
import dev.lapiszero.app.domain.PairingPayload
import dev.lapiszero.app.domain.Preview1Bit
import dev.lapiszero.app.domain.SendResult
import dev.lapiszero.app.domain.TimezoneSync
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test

/**
 * End-to-end against a real `lapis-daemon --mock`. Skipped unless told where the daemon is:
 *
 *   cd daemon && LAPIS_CONFIG_DIR=/tmp/lps-android uv run lapis-daemon --mock \
 *       --dev-pair-token dev-local-token --api-port 18431
 *   cd android && ./gradlew testDebugUnitTest --tests "*.MockDaemonIntegrationTest" \
 *       -Plapis.integration.url=http://127.0.0.1:18431 -Plapis.integration.token=dev-local-token
 *
 * (or the environment variables LAPIS_INTEGRATION_URL / LAPIS_INTEGRATION_TOKEN).
 */
class MockDaemonIntegrationTest {
    private val url: String? = System.getProperty("lapis.integration.url") ?: System.getenv("LAPIS_INTEGRATION_URL")
    private val token: String =
        System.getProperty("lapis.integration.token") ?: System.getenv("LAPIS_INTEGRATION_TOKEN") ?: "dev-local-token"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private lateinit var endpoint: Endpoint
    private lateinit var api: LapisApiClient
    private lateinit var repo: ChatRepository
    private val notices = CopyOnWriteArrayList<Notice>()

    @Before
    fun setUp() {
        assumeTrue("no mock daemon configured (set lapis.integration.url)", !url.isNullOrBlank())
        val host = url!!.removePrefix("http://").removePrefix("https://").trimEnd('/')
        endpoint = Endpoint(host, tls = url.startsWith("https://"))
        api = LapisApiClient(endpoint, { token })
        repo = ChatRepository(api, LapisWsClient(api.http, endpoint.wsUrl, scope), scope)
        scope.launch(start = CoroutineStart.UNDISPATCHED) { repo.notices.collect { notices += it } }
    }

    @After
    fun tearDown() {
        if (::repo.isInitialized) {
            runBlocking { runCatching { api.killAll() } }
            repo.stop()
        }
        scope.cancel()
    }

    private suspend fun connectAndSettle() {
        repo.start()
        repo.connection.await(15_000) { it == ConnectionState.ONLINE }
        // A previous run may have left a task behind.
        if (repo.state.value?.isRunning == true) api.killAll()
        repo.state.await(20_000) { it != null && !it.isRunning }
        repo.approvals.await(10_000) { it.isEmpty() }
        notices.clear()
    }

    @Test
    fun `health state and bearer auth`() = runBlocking<Unit> {
        val health = api.health()
        assertTrue(health.ok)
        assertTrue("this test must only ever talk to a mock daemon", health.mock)
        assertTrue(health.version.isNotBlank())
        assertTrue(health.deviceId.isNotBlank())
        val state = api.state()
        assertTrue(state.hosts.isNotEmpty())
        assertTrue(state.device.mock)

        for (bad in listOf<String?>(null, "wrong-token")) {
            try {
                LapisApiClient(endpoint, { bad }).health()
                fail("a request without a valid token must be rejected")
            } catch (e: LapisApiException) {
                assertTrue(e.isUnauthorized)
                assertEquals("unauthorized", e.code)
            }
        }
    }

    @Test
    fun `chat run with approve then deny over rest and websocket`() = runBlocking<Unit> {
        connectAndSettle()
        val host = repo.state.value!!.hosts.first()
        val chat = repo.createChat(host).getOrThrow()
        assertEquals(host, chat.hostId)
        repo.loadThread(chat.id)

        // Send a prompt: 202, the agent starts, a second prompt is refused with 409 busy.
        assertEquals(SendResult.Sent, repo.sendMessage(chat.id, "integration: open settings"))
        repo.state.await(15_000) { it?.isRunning == true && it.activeChatId == chat.id }
        assertEquals(SendResult.Busy, repo.sendMessage(chat.id, "integration: second prompt"))
        assertTrue("the refused prompt is withdrawn", repo.thread(chat.id).value.pending.isEmpty())

        // First gate: a vnc_click with a 1-bit preview and a screenshot, received over the socket.
        val click = repo.approvals.await(40_000) { it.isNotEmpty() }.first()
        assertEquals("vnc_click", click.action)
        assertEquals(chat.id, click.chatId)
        assertEquals(host, click.host)
        assertNotNull(click.clickX)
        assertNotNull(click.clickY)
        val bits = Preview1Bit.decodeBase64(click.preview1BitB64)
        assertNotNull("120x68 preview decodes", bits)
        assertEquals(120 * 68, bits!!.size)
        assertTrue("the preview is a picture, not a blank", bits.any { it } && bits.any { !it })
        assertEquals(AgentState.WAITING_APPROVAL, repo.state.await(5_000) { it?.agentState == AgentState.WAITING_APPROVAL }?.agentState)

        // The full-colour frame the crosshair is drawn on.
        val shot = api.screenshot(click.screenshotId!!)
        assertEquals(0xFF.toByte(), shot.bytes[0])
        assertEquals(0xD8.toByte(), shot.bytes[1]) // JPEG SOI
        assertTrue(shot.bytes.size > 1_000)
        assertTrue(shot.width!! in 1..1280 && shot.height!! in 1..720)
        assertTrue(click.clickX!! in 0 until shot.width && click.clickY!! in 0 until shot.height!!)

        // Approve it.
        assertEquals(DecideResult.Done, repo.decide(click.requestId, Decision.APPROVE_ONCE))
        assertEquals(DecideResult.AlreadyResolved, repo.decide(click.requestId, Decision.APPROVE_ONCE))

        // Second gate: an ssh_exec. Deny it with a note.
        val ssh = repo.approvals.await(40_000) { list -> list.any { it.requestId != click.requestId } }
            .first { it.requestId != click.requestId }
        assertEquals("ssh_exec", ssh.action)
        assertEquals(null, ssh.preview1BitB64)
        assertEquals(DecideResult.Done, repo.decide(ssh.requestId, Decision.DENY, "integration test says no"))

        // The task ends and everything returns to idle.
        withTimeout(40_000) { while (notices.none { it is Notice.TaskDone }) delay(50) }
        val finished = notices.filterIsInstance<Notice.TaskDone>().first().finished
        assertEquals(chat.id, finished.chatId)
        assertTrue(finished.status.isNotBlank())
        repo.state.await(15_000) { it?.agentState == AgentState.IDLE }
        assertTrue(repo.approvals.value.isEmpty())

        // The thread, as delivered live over the socket, tells the same story as REST.
        val live = repo.thread(chat.id).await(10_000) { t -> t.messages.any { it.role == Message.ROLE_ASSISTANT } }
        val stored = api.chat(chat.id).messages
        assertEquals(stored.map { it.id }, live.messages.map { it.id })
        assertEquals(Message.ROLE_USER, stored.first().role)
        assertEquals("integration: open settings", stored.first().content)
        val tools = stored.filter { it.role == Message.ROLE_TOOL }
        assertTrue(tools.any { it.metadata.kind == "vnc_click" && it.metadata.status == "ok" && it.metadata.screenshotId != null })
        assertTrue("the denied command is recorded as denied", tools.any { it.metadata.kind == "ssh_exec" && it.metadata.status == "denied" })
        assertTrue(repo.activity.value.any { it.kind == "vnc_click" })
        assertTrue(repo.chats.value.any { it.id == chat.id })

        repo.deleteChat(chat.id).getOrThrow()
        try {
            api.chat(chat.id)
            fail("deleted chat still exists")
        } catch (e: LapisApiException) {
            assertTrue(e.isNotFound)
        }
    }

    @Test
    fun `kill switch stops a running task`() = runBlocking<Unit> {
        connectAndSettle()
        val chat = repo.createChat(repo.state.value!!.hosts.first()).getOrThrow()
        assertEquals(SendResult.Sent, repo.sendMessage(chat.id, "integration: kill me"))
        repo.state.await(15_000) { it?.isRunning == true }
        assertTrue(repo.killAll())
        repo.state.await(20_000) { it?.agentState == AgentState.IDLE }
        withTimeout(20_000) { while (notices.none { it is Notice.TaskDone }) delay(50) }
        assertFalse(repo.state.value!!.isRunning)
        // Manual assist is refused when nothing is paused.
        try {
            api.nudge(chat.hostId, dev.lapiszero.app.data.remote.NudgeRequest("key", combo = "Return"))
            fail("nudge must be refused while idle")
        } catch (e: LapisApiException) {
            assertTrue(e.isNotPaused)
        }
        repo.deleteChat(chat.id)
    }

    @Test
    fun `qr pair exchange issues a working token that can be revoked`() = runBlocking<Unit> {
        api.power(PowerAction.START_QR_PAIRING)
        var payload: String? = null
        withTimeout(10_000) {
            while (payload == null) {
                payload = api.state().pairing?.takeIf { it.mode == "qr" }?.payload
                if (payload == null) delay(100)
            }
        }
        val parsed = (PairingPayload.parse(payload!!) as PairingPayload.ParseResult.Ok).payload
        assertFalse("a development daemon advertises plain http", parsed.tls)
        assertEquals(endpoint.host.substringAfter(':'), parsed.host.substringAfter(':'))

        val paired = LapisApiClient.pairExchange(parsed.endpoint, parsed.nonce, "Integration Phone", "JUnit")
        assertTrue(paired.apiToken.length >= 32)
        assertNotEquals(token, paired.apiToken)

        val mine = LapisApiClient(parsed.endpoint, { paired.apiToken })
        assertEquals(paired.deviceId, mine.health().deviceId)
        val listed = mine.devices().single { it.current }
        assertEquals("Integration Phone", listed.name)
        assertEquals(paired.deviceId, listed.id)

        // The nonce is single-use.
        try {
            LapisApiClient.pairExchange(parsed.endpoint, parsed.nonce, "Replay", "JUnit")
            fail("a pairing nonce must be single-use")
        } catch (e: LapisApiException) {
            assertEquals(403, e.status)
            assertEquals("bad_nonce", e.code)
        }

        // Revoke it (from the original phone) and the token is dead at once.
        api.revokeDevice(paired.deviceId)
        try {
            mine.health()
            fail("a revoked token must stop working")
        } catch (e: LapisApiException) {
            assertTrue(e.isUnauthorized)
        }
        runCatching { api.power(PowerAction.STOP_PAIRING) }
    }

    @Test
    fun `configuration cockpit and write-only secrets`() = runBlocking<Unit> {
        val hosts = api.hosts()
        assertTrue(hosts.hosts.isNotEmpty())
        assertTrue(hosts.activeHost in hosts.hosts.keys)
        val probe = api.probe(hosts.activeHost)
        assertEquals(hosts.activeHost, probe.hostId)
        assertTrue(probe.tailscale.ok && probe.vnc.ok && probe.ssh.ok)

        // lapis.toml round trip: writing back what was read changes nothing.
        val config = api.config()
        assertTrue(config.containsKey("llm"))
        assertEquals(config, api.putConfig(config))

        // Secrets: set, see only the masked tail, unset.
        val name = "LAPIS_ITEST_SECRET"
        val value = "itest-secret-value-1234abcd"
        val afterSet = api.patchSecrets(set = mapOf(name to value)).single { it.name == name }
        assertTrue(afterSet.isSet)
        assertEquals("...abcd", afterSet.masked)
        val status = api.secrets()
        assertFalse("no endpoint ever returns the plaintext", status.toString().contains(value))
        assertFalse(api.config().toString().contains(value))
        assertTrue(api.patchSecrets(unset = listOf(name)).none { it.name == name && it.isSet })

        // Schedules CRUD + run now.
        val job = "itest job ${System.nanoTime()}"
        val created = api.createSchedule(
            dev.lapiszero.app.data.remote.ScheduleCreate(job, "0 8 * * *", hosts.activeHost, "integration: scheduled", false),
        )
        assertTrue(created.any { it.name == job && !it.enabled })
        val enabled = api.patchSchedule(job, dev.lapiszero.app.data.remote.SchedulePatch(enabled = true))
        assertNotNull(enabled.single { it.name == job }.nextRun)
        try {
            api.patchSchedule(job, dev.lapiszero.app.data.remote.SchedulePatch(schedule = "not a cron"))
            fail("a bad cron expression must be rejected")
        } catch (e: LapisApiException) {
            assertEquals(422, e.status)
        }
        api.deleteSchedule(job)
        assertTrue(api.schedules().none { it.name == job })

        // Device cockpit.
        val telemetry = api.telemetry()
        assertTrue(telemetry.memory.totalMb > 0)
        assertTrue(telemetry.services.isNotEmpty())
        val before = api.state().ui
        val patched = api.patchUi(UiPatch(brightness = 42))
        assertEquals(42, patched.brightness)
        assertEquals(42, api.state().ui.brightness)
        api.patchUi(UiPatch(brightness = before.brightness))
        try {
            api.patchUi(UiPatch(brightness = 400))
            fail("brightness outside 0..100 must be rejected")
        } catch (e: LapisApiException) {
            assertEquals(422, e.status)
        }
        // No lapis-ui is attached to a bare mock daemon: the mirror answers 504, not garbage.
        try {
            val png = api.screenPng()
            assertEquals(0x89.toByte(), png[0]) // a simulator is attached: then it must be a PNG
        } catch (e: LapisApiException) {
            assertTrue(e.isUiUnavailable)
        }
        assertTrue(api.devices().any { it.current })
    }

    @Test
    fun `timezone hand-over after pairing`() = runBlocking<Unit> {
        val state = api.state()
        assumeTrue("this daemon predates utc_offset_min / ui.timezone", state.utcOffsetMin != null)
        val original = state.ui.timezone
        val target = if (original == "Asia/Tokyo") "Europe/Berlin" else "Asia/Tokyo"
        try {
            assertTrue(TimezoneSync(api) { target }.syncIfNeeded(state))
            val after = api.state()
            assertEquals(target, after.ui.timezone)
            assertEquals(if (target == "Asia/Tokyo") 540 else after.utcOffsetMin, after.utcOffsetMin)
            // Unknown zones are refused with 422 and swallowed by the sync.
            try {
                api.patchUi(UiPatch(timezone = "Mars/Olympus_Mons"))
                fail("an unknown timezone must be rejected")
            } catch (e: LapisApiException) {
                assertEquals(422, e.status)
            }
            assertTrue(TimezoneSync(api) { "Mars/Olympus_Mons" }.syncIfNeeded(after))
            assertEquals(target, api.state().ui.timezone)
        } finally {
            if (original.isNotEmpty()) runCatching { api.patchUi(UiPatch(timezone = original)) }
        }
    }

    private fun assertNotEquals(unexpected: Any?, actual: Any?) = org.junit.Assert.assertNotEquals(unexpected, actual)
}
