package dev.lapiszero.app.data.remote

import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.endpoint
import dev.lapiszero.app.errorResponse
import dev.lapiszero.app.jsonResponse
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Request
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

class LapisApiClientTest {
    private val server = MockWebServer()
    private val router = Router()

    @Before
    fun setUp() {
        server.dispatcher = router
        server.start()
    }

    @After
    fun tearDown() = server.close()

    private fun body(key: String): JsonObject =
        LapisJson.parseToJsonElement(router.take(key).body!!.utf8()).jsonObject

    @Test
    fun `bearer token is attached to every device request`() = runBlocking<Unit> {
        router.json("GET", "/v1/health", """{"ok": true, "version": "0.2.0", "hostname": "h", "uptime_sec": 1, "mock": true, "device_id": "dev_local"}""")
        val health = server.api("tok-123").health()
        assertTrue(health.ok)
        assertEquals("Bearer tok-123", router.take("GET /v1/health").headers["Authorization"])
    }

    @Test
    fun `token is never sent to another origin`() = runBlocking<Unit> {
        val other = MockWebServer()
        other.enqueue(MockResponse.Builder().code(200).body("{}").build())
        other.start()
        try {
            val api = server.api("tok-123")
            // Same client (same interceptor), different port: no Authorization header.
            api.http.newCall(Request.Builder().url(other.url("/elsewhere")).build()).await()
            assertNull(other.takeRequest().headers["Authorization"])
        } finally {
            other.close()
        }
    }

    @Test
    fun `no header at all without a token`() = runBlocking<Unit> {
        router.json("GET", "/v1/state", Fixtures.stateSync())
        server.api(token = null).state()
        assertNull(router.take("GET /v1/state").headers["Authorization"])
    }

    @Test
    fun `error envelope becomes a typed exception`() = runBlocking<Unit> {
        router.on("POST", "/v1/chats/c_1/messages") { errorResponse(409, "busy", "Agent busy: a task is already running") }
        router.on("GET", "/v1/health") { errorResponse(401, "unauthorized", "Missing, invalid or revoked bearer token") }
        router.on("POST", "/v1/hosts/h/nudge") { errorResponse(409, "not_paused", "Manual assist is only available while the agent is paused") }
        router.on("GET", "/v1/device/screen.png") { errorResponse(504, "ui_unavailable", "lapis-ui did not answer the screen request") }
        router.on("POST", "/v1/approvals/req_9") { errorResponse(404, "not_found", "No pending approval with that id") }
        router.on("GET", "/v1/devices") { MockResponse.Builder().code(502).body("<html>bad gateway</html>").build() }
        val api = server.api()

        val busy = expectApiError { api.sendMessage("c_1", "hi") }
        assertEquals(409, busy.status)
        assertEquals("busy", busy.code)
        assertTrue(busy.isBusy)
        assertEquals("Agent busy: a task is already running", busy.message)

        assertTrue(expectApiError { api.health() }.isUnauthorized)
        val notPaused = expectApiError { api.nudge("h", NudgeRequest("click", 1, 2)) }
        assertTrue(notPaused.isNotPaused)
        assertFalse(notPaused.isBusy)
        assertTrue(expectApiError { api.screenPng() }.isUiUnavailable)
        assertTrue(expectApiError { api.decide("req_9", Decision.DENY) }.isNotFound)
        val gateway = expectApiError { api.devices() }
        assertEquals(502, gateway.status)
        assertEquals("http_502", gateway.code)
    }

    @Test
    fun `chat endpoints use the documented methods paths and bodies`() = runBlocking<Unit> {
        router.json("GET", "/v1/chats", """{"chats": [${Fixtures.chat()}]}""")
        router.json("POST", "/v1/chats", Fixtures.chat(id = "c_new", title = "New chat"), code = 201)
        router.json("GET", "/v1/chats/c_new", """{"chat": ${Fixtures.chat(id = "c_new")}, "messages": [], "has_more": false}""")
        router.json("PATCH", "/v1/chats/c_new", Fixtures.chat(id = "c_new", title = "Renamed"))
        router.on("DELETE", "/v1/chats/c_new") { MockResponse.Builder().code(204).build() }
        router.json("POST", "/v1/chats/c_new/messages", """{"message": ${Fixtures.userMessage(chatId = "c_new", taskId = "t_1")}, "task_id": "t_1"}""", code = 202)
        router.json("POST", "/v1/chats/c_new/stop", """{"stopped": true}""")
        val api = server.api()

        assertEquals(1, api.chats(hostId = "macbook-pro", archived = true).size)
        val list = router.take("GET /v1/chats")
        assertEquals("macbook-pro", list.url.queryParameter("host_id"))
        assertEquals("1", list.url.queryParameter("archived"))

        assertEquals("c_new", api.createChat("macbook-pro").id)
        assertEquals(mapOf("host_id" to JsonPrimitive("macbook-pro")), body("POST /v1/chats").toMap())

        api.chat("c_new", before = 40, limit = 20)
        val detail = router.take("GET /v1/chats/c_new")
        assertEquals("40", detail.url.queryParameter("before"))
        assertEquals("20", detail.url.queryParameter("limit"))

        api.patchChat("c_new", JsonObject(mapOf("policy_override" to JsonNull)))
        assertEquals(JsonNull, body("PATCH /v1/chats/c_new")["policy_override"])

        val sent = api.sendMessage("c_new", "open settings")
        assertEquals("t_1", sent.taskId)
        assertEquals("open settings", body("POST /v1/chats/c_new/messages").getValue("content").jsonPrimitive.content)
        assertTrue(api.stopChat("c_new"))
        api.deleteChat("c_new")
        assertEquals("DELETE", router.take("DELETE /v1/chats/c_new").method)
    }

    @Test
    fun `control endpoints`() = runBlocking<Unit> {
        router.json("POST", "/v1/approvals/req_1", """{"ok": true}""")
        router.json("POST", "/v1/agent/kill-all", """{"ok": true}""")
        router.json("POST", "/v1/agent/pause", """{"agent_state": "PAUSED"}""")
        router.json("POST", "/v1/agent/resume", """{"agent_state": "ACTING"}""")
        router.json("PUT", "/v1/agent/active-host", """{"active_host": "arch-dev"}""")
        router.json("POST", "/v1/hosts/arch-dev/nudge", """{"ok": true}""")
        router.json("POST", "/v1/hosts/arch-dev/probe", """{"host_id": "arch-dev", "tailscale": {"ok": true, "latency_ms": 12, "detail": "mock"}, "vnc": {"ok": true, "latency_ms": 85, "detail": "mock"}, "ssh": {"ok": false, "latency_ms": null, "detail": "x"}}""")
        val api = server.api()

        assertTrue(api.decide("req_1", Decision.DENY, "not that button"))
        val decision = body("POST /v1/approvals/req_1")
        assertEquals("deny", decision.getValue("decision").jsonPrimitive.content)
        assertEquals("not that button", decision.getValue("reason").jsonPrimitive.content)
        api.decide("req_1", Decision.APPROVE_SESSION, "  ")
        assertEquals(setOf("decision"), body("POST /v1/approvals/req_1").keys)

        api.killAll()
        assertEquals("POST", router.take("POST /v1/agent/kill-all").method)
        assertEquals("PAUSED", api.pause())
        assertEquals("ACTING", api.resume())
        assertEquals("arch-dev", api.setActiveHost("arch-dev"))
        assertEquals("arch-dev", body("PUT /v1/agent/active-host").getValue("host_id").jsonPrimitive.content)
        assertTrue(api.nudge("arch-dev", NudgeRequest(action = "type", text = "hello")))
        assertEquals(setOf("action", "text"), body("POST /v1/hosts/arch-dev/nudge").keys)
        assertFalse(api.probe("arch-dev").ssh.ok)
    }

    @Test
    fun `ssh key authorise agent tools and run-with endpoints`() = runBlocking<Unit> {
        router.json("GET", "/v1/device/ssh-key", Fixtures.SSH_KEY)
        router.json("POST", "/v1/hosts/arch-dev/ssh/authorize", """{"ok": true, "already": false, "detail": "key added for dev"}""")
        router.json("GET", "/v1/hosts/arch-dev/agents", Fixtures.AGENTS)
        router.json("PATCH", "/v1/chats/c_1", Fixtures.chat(id = "c_1", agent = "claude", workdir = "~/src"))
        val api = server.api()

        assertEquals(Fixtures.FINGERPRINT, api.deviceSshKey()?.fingerprint)
        assertEquals("GET", router.take("GET /v1/device/ssh-key").method)

        val done = api.authorizeSsh("arch-dev")
        assertTrue(done.ok && !done.already)
        assertTrue("nothing about the login is sent: the device has it", body("POST /v1/hosts/arch-dev/ssh/authorize").isEmpty())

        assertEquals(listOf("Claude Code", "opencode", "Freya"), api.hostAgents("arch-dev").map { it.name })
        assertEquals("GET", router.take("GET /v1/hosts/arch-dev/agents").method)

        // Each PATCH carries exactly the one key it changes.
        assertEquals("claude", api.setChatAgent("c_1", "claude").agent)
        assertEquals(mapOf("agent" to JsonPrimitive("claude")), body("PATCH /v1/chats/c_1").toMap())
        assertEquals("~/src", api.setChatWorkdir("c_1", "~/src").workdir)
        assertEquals(mapOf("workdir" to JsonPrimitive("~/src")), body("PATCH /v1/chats/c_1").toMap())

        // Device software from before these features answers 404: for its key that is "no key",
        // not an error. Anything else is one.
        router.on("GET", "/v1/device/ssh-key") { errorResponse(404, "not_found", "404: Not Found") }
        assertNull(api.deviceSshKey())
        router.on("GET", "/v1/device/ssh-key") { errorResponse(500, "internal", "boom") }
        assertEquals(500, expectApiError { api.deviceSshKey() }.status)
        // A host without working SSH answers 4xx with the reason.
        assertTrue(expectApiError { api.hostAgents("gone") }.isNotFound)
        router.on("GET", "/v1/hosts/arch-dev/agents") { errorResponse(409, "ssh_unavailable", "SSH login failed for dev@100.64.20.42: permission denied") }
        assertEquals("SSH login failed for dev@100.64.20.42: permission denied", expectApiError { api.hostAgents("arch-dev") }.message)
        router.on("POST", "/v1/hosts/arch-dev/ssh/authorize") { errorResponse(422, "invalid", "No login works for dev@100.64.20.42: store its password or turn on Remote Login") }
        assertEquals(422, expectApiError { api.authorizeSsh("arch-dev") }.status)
    }

    @Test
    fun `screenshot bytes and click headers`() = runBlocking<Unit> {
        val jpeg = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE0.toByte(), 1, 2, 3)
        router.on("GET", "/v1/screenshots/2.jpg") {
            MockResponse.Builder().code(200)
                .setHeader("Content-Type", "image/jpeg")
                .setHeader("X-Lapis-Width", "1280").setHeader("X-Lapis-Height", "720")
                .setHeader("X-Lapis-Click-X", "840").setHeader("X-Lapis-Click-Y", "412")
                .body(Buffer().write(jpeg)).build()
        }
        router.on("GET", "/v1/screenshots/3.jpg") {
            MockResponse.Builder().code(200).setHeader("X-Lapis-Width", "1280").setHeader("X-Lapis-Height", "720")
                .body(Buffer().write(jpeg)).build()
        }
        router.on("GET", "/v1/device/screen.png") { MockResponse.Builder().code(200).body(Buffer().write(byteArrayOf(1, 2, 3))).build() }
        val api = server.api()
        val shot = api.screenshot(2)
        assertArrayEquals(jpeg, shot.bytes)
        assertEquals(1280, shot.width)
        assertEquals(720, shot.height)
        assertEquals(840, shot.clickX)
        assertEquals(412, shot.clickY)
        assertNull(api.screenshot(3).clickX)
        assertArrayEquals(byteArrayOf(1, 2, 3), api.screenPng())
        assertTrue(expectApiError { api.screenshot(99) }.isNotFound)
    }

    @Test
    fun `configuration endpoints`() = runBlocking<Unit> {
        router.json("GET", "/v1/config", """{"config": {"llm": {"model": "m"}, "x": 1}}""")
        router.on("PUT", "/v1/config") { request -> jsonResponse(request.body!!.utf8()) }
        router.json("GET", "/v1/hosts", """{"hosts": {}, "active_host": ""}""")
        router.on("PUT", "/v1/hosts") { request ->
            jsonResponse("""{"hosts": ${LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject.getValue("hosts")}, "active_host": "box"}""")
        }
        router.json("GET", "/v1/secrets/status", """{"secrets": [{"name": "ANTHROPIC_API_KEY", "is_set": false, "masked": null}]}""")
        router.json("PATCH", "/v1/secrets", """{"secrets": [{"name": "ANTHROPIC_API_KEY", "is_set": true, "masked": "...cret"}]}""")
        router.json("GET", "/v1/schedules", """{"schedules": []}""")
        router.json("POST", "/v1/schedules", """{"schedules": [{"name": "Daily Digest", "schedule": "0 8 * * *", "host": "h", "prompt": "p", "enabled": true, "next_run": 5, "last_run_at": null, "last_chat_id": null}]}""", code = 201)
        router.json("PATCH", "/v1/schedules/Daily%20Digest", """{"schedules": []}""")
        router.on("DELETE", "/v1/schedules/Daily%20Digest") { MockResponse.Builder().code(204).build() }
        router.json("POST", "/v1/schedules/Daily%20Digest/run", """{"chat_id": "c_7", "task_id": "t_7"}""", code = 202)
        router.json("GET", "/v1/devices", """{"devices": []}""")
        router.on("DELETE", "/v1/devices/dev_2") { MockResponse.Builder().code(204).build() }
        router.json("GET", "/v1/device/telemetry", Fixtures.TELEMETRY)
        router.json("PATCH", "/v1/device/ui", """{"theme": "dark", "brightness": 40, "mute_audio": false}""")
        router.json("POST", "/v1/device/power", """{"ok": true, "action": "start_qr_pairing"}""", code = 202)
        val api = server.api()

        val config = api.config()
        val saved = api.putConfig(config)
        assertEquals(config, saved)
        assertEquals(config, body("PUT /v1/config").getValue("config"))

        assertTrue(api.hosts().hosts.isEmpty())
        assertEquals("box", api.putHosts(mapOf("box" to Host("box", "100.1.1.1"))).activeHost)

        assertFalse(api.secrets().single().isSet)
        val status = api.patchSecrets(set = mapOf("ANTHROPIC_API_KEY" to "sk-secret"))
        assertEquals("...cret", status.single().masked)
        val patch = body("PATCH /v1/secrets")
        assertEquals("sk-secret", patch.getValue("set").jsonObject.getValue("ANTHROPIC_API_KEY").jsonPrimitive.content)

        assertTrue(api.schedules().isEmpty())
        assertEquals(1, api.createSchedule(ScheduleCreate("Daily Digest", "0 8 * * *", "h", "p", true)).size)
        // Names with spaces are percent-encoded into one path segment.
        api.patchSchedule("Daily Digest", SchedulePatch(enabled = false))
        assertEquals(setOf("enabled"), body("PATCH /v1/schedules/Daily%20Digest").keys)
        assertEquals("c_7", api.runSchedule("Daily Digest").chatId)
        api.deleteSchedule("Daily Digest")
        assertEquals(1, router.count("DELETE /v1/schedules/Daily%20Digest"))

        assertTrue(api.devices().isEmpty())
        api.revokeDevice("dev_2")
        assertEquals(1, router.count("DELETE /v1/devices/dev_2"))
        assertEquals(84, api.telemetry().battery?.percent)
        assertEquals(40, api.patchUi(UiPatch(brightness = 40)).brightness)
        assertEquals(setOf("brightness"), body("PATCH /v1/device/ui").keys)
        assertEquals("start_qr_pairing", api.power(PowerAction.START_QR_PAIRING).action)
    }

    @Test
    fun `path segments cannot escape their slot`() {
        assertEquals("a%2Fb", LapisApiClient.seg("a/b"))
        assertEquals("..%2F..%2Fagent%2Fkill-all", LapisApiClient.seg("../../agent/kill-all"))
        assertEquals("x%3Fy=1", LapisApiClient.seg("x?y=1"))
        assertEquals("Daily%20Digest", LapisApiClient.seg("Daily Digest"))
    }

    @Test
    fun `pair exchange is unauthenticated and refuses cleartext to other hosts`() = runBlocking<Unit> {
        router.json("POST", "/v1/pair/exchange", """{"device_id": "dev_1", "api_token": "fresh-token", "hostname": "lapis-zero"}""")
        val result = LapisApiClient.pairExchange(server.endpoint(), "n".repeat(43), "Pixel", "Google Pixel 9")
        assertEquals("fresh-token", result.apiToken)
        val request = router.take("POST /v1/pair/exchange")
        assertNull(request.headers["Authorization"])
        assertEquals(
            setOf("nonce", "device_name", "device_model"),
            LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject.keys,
        )

        for (bad in listOf(Endpoint("192.168.1.20:8421", tls = false), Endpoint("lapis-zero.tailnet.ts.net", tls = false))) {
            val e = expectApiError { LapisApiClient.pairExchange(bad, "n", "p", "m") }
            assertEquals("cleartext_refused", e.code)
        }
        assertEquals("bad_host", expectApiError { LapisApiClient.pairExchange(Endpoint("evil.com/x", true), "n", "p", "m") }.code)
        assertEquals(1, router.count("POST /v1/pair/exchange"))
    }

    @Test
    fun `pair exchange surfaces bad nonce and lockout`() = runBlocking<Unit> {
        router.on("POST", "/v1/pair/exchange") { errorResponse(403, "bad_nonce", "Pairing code is invalid, already used, or expired") }
        assertEquals("bad_nonce", expectApiError { LapisApiClient.pairExchange(server.endpoint(), "n", "p", "m") }.code)
        router.on("POST", "/v1/pair/exchange") { errorResponse(429, "locked_out", "Too many failed pairing attempts; wait a minute") }
        assertEquals(429, expectApiError { LapisApiClient.pairExchange(server.endpoint(), "n", "p", "m") }.status)
    }

    @Test
    fun `endpoint host validation`() {
        for (good in listOf("lapis-zero.tailnet.ts.net", "127.0.0.1:8421", "localhost:1", "10.0.2.2:65535", "a")) {
            assertTrue(good, Endpoint.isValidHost(good))
        }
        for (bad in listOf("", "a b", "http://x", "x/y", "user@x", "x:", "x:0", "x:70000", "x:12:3", "-x", "x-", "x#y", "x?y", "[::1]")) {
            assertFalse(bad, Endpoint.isValidHost(bad))
        }
        assertTrue(Endpoint.isCleartextAllowed("127.0.0.1:8421"))
        assertTrue(Endpoint.isCleartextAllowed("LOCALHOST:8421"))
        assertTrue(Endpoint.isCleartextAllowed("10.0.2.2"))
        assertFalse(Endpoint.isCleartextAllowed("10.0.2.3:8421"))
        assertFalse(Endpoint.isCleartextAllowed("127.0.0.1.evil.com"))
        assertFalse(Endpoint.isCleartextAllowed("lapis-zero.tailnet.ts.net"))
    }

    private suspend fun expectApiError(block: suspend () -> Any?): LapisApiException {
        try {
            block()
        } catch (e: LapisApiException) {
            return e
        }
        fail("expected LapisApiException")
        throw AssertionError()
    }
}
