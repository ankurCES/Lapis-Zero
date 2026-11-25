package dev.lapiszero.app.domain

import dev.lapiszero.app.FakeEventSource
import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.await
import dev.lapiszero.app.awaitSize
import dev.lapiszero.app.data.local.ChatCache
import dev.lapiszero.app.data.remote.Chat
import dev.lapiszero.app.data.remote.ClientCommand
import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.data.remote.Message
import dev.lapiszero.app.data.remote.WsStatus
import dev.lapiszero.app.errorResponse
import dev.lapiszero.app.jsonResponse
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
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
import org.junit.Test

class ChatRepositoryTest {
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
        router.json("GET", "/v1/chats", """{"chats": [${Fixtures.chat()}]}""")
        router.json("GET", "/v1/chats/$chatId", """{"chat": ${Fixtures.chat()}, "messages": [], "has_more": false}""")
        repo = ChatRepository(server.api(), source, scope)
        repo.start()
    }

    @After
    fun tearDown() {
        scope.cancel()
        server.close()
    }

    private suspend fun sync(epoch: Int = 1, json: String = Fixtures.stateSync()) {
        source.emit(epoch, json)
        repo.state.await { it != null }
    }

    @Test
    fun `start subscribes then opens the socket`() {
        assertTrue(source.started)
        assertEquals(ConnectionState.CONNECTING, repo.connection.value)
    }

    @Test
    fun `first state_sync resyncs chats and connection state`() = runBlocking<Unit> {
        sync()
        assertEquals(ConnectionState.ONLINE, repo.connection.await { it == ConnectionState.ONLINE })
        assertEquals(listOf(chatId), repo.chats.await { it.isNotEmpty() }.map { it.id })
        assertEquals("macbook-pro", repo.state.value?.activeHost)
        // Later state_syncs on the same connection do not refetch.
        source.emit(1, Fixtures.stateSync("THINKING", activeChatId = chatId, step = 1))
        repo.state.await { it?.agentState == "THINKING" }
        delay(150)
        assertEquals(1, router.count("GET /v1/chats"))
    }

    @Test
    fun `approval arrives is shown then resolved elsewhere and cleared`() = runBlocking<Unit> {
        sync()
        val resolutions = CopyOnWriteArrayList<String>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { repo.resolutions.collect { resolutions += "${it.requestId}:${it.by}" } }

        source.emit(1, Fixtures.approval("req_1"))
        val shown = repo.approvals.await { it.size == 1 }.single()
        assertEquals("req_1", shown.requestId)
        assertEquals(840, shown.clickX)

        // The same request again (state_sync and approval_request both carry it) is not doubled.
        source.emit(1, Fixtures.stateSync("WAITING_APPROVAL", chatId, 5, approvals = Fixtures.approval("req_1")))
        source.emit(1, Fixtures.approval("req_1"))
        delay(100)
        assertEquals(1, repo.approvals.value.size)

        // Somebody presses the button on the CardputerZero.
        source.emit(1, Fixtures.approvalResolved("req_1", "approve_once", "device"))
        repo.approvals.await { it.isEmpty() }
        assertEquals(listOf("req_1:device"), resolutions.awaitSize(1).toList())
        assertEquals(0, router.count("POST /v1/approvals/req_1"))
    }

    @Test
    fun `state_sync is authoritative for pending approvals`() = runBlocking<Unit> {
        sync(json = Fixtures.stateSync("WAITING_APPROVAL", chatId, 5, approvals = Fixtures.approval("req_1")))
        assertEquals(listOf("req_1"), repo.approvals.await { it.isNotEmpty() }.map { it.requestId })
        val resolutions = CopyOnWriteArrayList<String>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { repo.resolutions.collect { resolutions += it.requestId } }
        // The kill switch cleared it; this client only sees the next snapshot.
        source.emit(1, Fixtures.stateSync("IDLE"))
        repo.approvals.await { it.isEmpty() }
        assertEquals(listOf("req_1"), resolutions.awaitSize(1).toList())
    }

    @Test
    fun `reconnect replaces stale state from the new snapshot`() = runBlocking<Unit> {
        sync(epoch = 1, json = Fixtures.stateSync("WAITING_APPROVAL", chatId, 5, approvals = Fixtures.approval("req_1")))
        repo.chats.await { it.isNotEmpty() }
        repo.loadThread(chatId)
        repo.setTelemetrySubscribed(true)
        source.sent.clear()

        // Socket drops; meanwhile req_1 was resolved and req_2 raised.
        source.status.value = WsStatus.Waiting(0, 500)
        assertEquals(ConnectionState.OFFLINE, repo.connection.await { it == ConnectionState.OFFLINE })
        router.json("GET", "/v1/chats", """{"chats": [${Fixtures.chat()}, ${Fixtures.chat(id = "c_new", title = "made on device", updatedAt = 1795549999)}]}""")
        router.json("GET", "/v1/chats/$chatId", """{"chat": ${Fixtures.chat()}, "messages": [${Fixtures.userMessage()}, ${Fixtures.toolMessage()}], "has_more": false}""")
        source.emit(2, Fixtures.stateSync("WAITING_APPROVAL", chatId, 6, approvals = Fixtures.approval("req_2", action = "ssh_exec")))

        assertEquals(listOf("req_2"), repo.approvals.await { it.singleOrNull()?.requestId == "req_2" }.map { it.requestId })
        assertEquals(listOf("c_new", chatId), repo.chats.await { it.size == 2 }.map { it.id })
        // The open thread is reloaded so nothing from the gap is missing.
        assertEquals(listOf(1L, 3L), repo.thread(chatId).await { it.messages.size == 2 }.messages.map { it.id })
        // The telemetry subscription is re-established on the new socket.
        assertTrue(source.sent.contains(ClientCommand.SubscribeTelemetry(true)))
        assertEquals(ConnectionState.ONLINE, repo.connection.value)
    }

    @Test
    fun `deciding here clears the approval once the daemon accepts it`() = runBlocking<Unit> {
        sync()
        source.emit(1, Fixtures.approval("req_1"))
        repo.approvals.await { it.size == 1 }
        router.json("POST", "/v1/approvals/req_1", """{"ok": true}""")

        assertEquals(DecideResult.Done, repo.decide("req_1", Decision.DENY, "wrong window"))
        assertTrue(repo.approvals.value.isEmpty())
        val body = LapisJson.parseToJsonElement(router.take("POST /v1/approvals/req_1").body!!.utf8()).jsonObject
        assertEquals("deny", body.getValue("decision").jsonPrimitive.content)
        assertEquals("wrong window", body.getValue("reason").jsonPrimitive.content)
        // The broadcast that follows is harmless.
        source.emit(1, Fixtures.approvalResolved("req_1", "deny", "android:Pixel"))
        delay(50)
        assertTrue(repo.approvals.value.isEmpty())
    }

    @Test
    fun `deciding something already resolved clears it too`() = runBlocking<Unit> {
        sync()
        source.emit(1, Fixtures.approval("req_1"))
        repo.approvals.await { it.size == 1 }
        router.on("POST", "/v1/approvals/req_1") { errorResponse(404, "not_found", "No pending approval with that id") }
        assertEquals(DecideResult.AlreadyResolved, repo.decide("req_1", Decision.APPROVE_ONCE))
        assertTrue(repo.approvals.value.isEmpty())
    }

    @Test
    fun `a rejected decision keeps the approval on screen`() = runBlocking<Unit> {
        sync()
        source.emit(1, Fixtures.approval("req_1"))
        repo.approvals.await { it.size == 1 }
        router.on("POST", "/v1/approvals/req_1") { errorResponse(422, "invalid", "decision must be approve_once, approve_session or deny") }
        assertTrue(repo.decide("req_1", "maybe") is DecideResult.Failed)
        assertEquals(1, repo.approvals.value.size)
    }

    @Test
    fun `decision and kill switch fall back to the socket when rest is unreachable`() = runBlocking<Unit> {
        sync()
        source.emit(1, Fixtures.approval("req_1"))
        repo.approvals.await { it.size == 1 }
        server.close() // REST is gone at the network level; the socket is still up

        assertEquals(DecideResult.Done, repo.decide("req_1", Decision.APPROVE_ONCE))
        assertEquals(ClientCommand.ApprovalDecision("req_1", Decision.APPROVE_ONCE, null), source.sent.last())
        // Not removed optimistically: approval_resolved over the socket does that.
        assertEquals(1, repo.approvals.value.size)
        source.emit(1, Fixtures.approvalResolved("req_1"))
        repo.approvals.await { it.isEmpty() }

        assertTrue(repo.killAll())
        assertEquals(ClientCommand.KillAll, source.sent.last())

        source.open = false
        assertFalse("nothing could carry the kill", repo.killAll())
        assertTrue(repo.decide("req_2", Decision.DENY) is DecideResult.Failed)
    }

    @Test
    fun `kill switch posts to kill-all`() = runBlocking<Unit> {
        sync()
        router.json("POST", "/v1/agent/kill-all", """{"ok": true}""")
        assertTrue(repo.killAll())
        assertEquals("POST", router.take("POST /v1/agent/kill-all").method)
        assertTrue("REST worked, no socket fallback", source.sent.none { it is ClientCommand.KillAll })
    }

    @Test
    fun `kill switch reports a refusal`() = runBlocking<Unit> {
        sync()
        router.on("POST", "/v1/agent/kill-all") { errorResponse(401, "unauthorized", "revoked") }
        val notice = async(start = CoroutineStart.UNDISPATCHED) { repo.notices.await { true } }
        assertFalse(repo.killAll())
        assertTrue((notice.await() as Notice.Toast).text.contains("Kill switch failed"))
    }

    @Test
    fun `optimistic send shows the prompt immediately and confirms it`() = runBlocking<Unit> {
        sync()
        repo.loadThread(chatId)
        router.on("POST", "/v1/chats/$chatId/messages") {
            MockResponse.Builder().code(202).setHeader("Content-Type", "application/json")
                .headersDelay(400, TimeUnit.MILLISECONDS)
                .body("""{"message": ${Fixtures.userMessage(id = 11, content = "open settings", taskId = "t_1")}, "task_id": "t_1"}""")
                .build()
        }
        val sending = async { repo.sendMessage(chatId, "  open settings ") }

        val optimistic = repo.thread(chatId).await { it.pending.isNotEmpty() }
        assertEquals("open settings", optimistic.pending.single().content)
        assertTrue("not confirmed yet", optimistic.messages.isEmpty())

        assertEquals(SendResult.Sent, sending.await())
        val settled = repo.thread(chatId).value
        assertTrue(settled.pending.isEmpty())
        assertEquals(listOf(11L), settled.messages.map { it.id })
        assertEquals("t_1", settled.messages.single().taskId)

        // The daemon's echo of the same message over the socket does not duplicate it.
        source.emit(1, Fixtures.messageCreated(chatId, Fixtures.userMessage(id = 11, content = "open settings")))
        delay(100)
        assertEquals(1, repo.thread(chatId).value.messages.size)
        assertEquals("the richer REST copy keeps its task id", "t_1", repo.thread(chatId).value.messages.single().taskId)
    }

    @Test
    fun `the socket echo arriving first retires the optimistic bubble`() = runBlocking<Unit> {
        sync()
        repo.loadThread(chatId)
        router.on("POST", "/v1/chats/$chatId/messages") {
            MockResponse.Builder().code(202).setHeader("Content-Type", "application/json")
                .headersDelay(400, TimeUnit.MILLISECONDS)
                .body("""{"message": ${Fixtures.userMessage(id = 11, taskId = "t_1")}, "task_id": "t_1"}""").build()
        }
        val sending = async { repo.sendMessage(chatId, "open settings") }
        repo.thread(chatId).await { it.pending.isNotEmpty() }
        source.emit(1, Fixtures.messageCreated(chatId, Fixtures.userMessage(id = 11)))
        val echoed = repo.thread(chatId).await { it.messages.isNotEmpty() }
        assertTrue("one bubble, not two", echoed.pending.isEmpty())
        assertEquals(SendResult.Sent, sending.await())
        assertEquals(listOf(11L), repo.thread(chatId).value.messages.map { it.id })
    }

    @Test
    fun `409 busy withdraws the optimistic prompt`() = runBlocking<Unit> {
        sync()
        repo.loadThread(chatId)
        router.on("POST", "/v1/chats/$chatId/messages") {
            MockResponse.Builder().code(409).setHeader("Content-Type", "application/json")
                .headersDelay(300, TimeUnit.MILLISECONDS)
                .body("""{"error": {"code": "busy", "message": "Agent busy: a task is already running"}}""").build()
        }
        val sending = async { repo.sendMessage(chatId, "again") }
        repo.thread(chatId).await { it.pending.size == 1 }
        assertEquals(SendResult.Busy, sending.await())
        assertTrue(repo.thread(chatId).value.pending.isEmpty())
        assertTrue(repo.thread(chatId).value.messages.isEmpty())
    }

    @Test
    fun `other send failures also withdraw and explain`() = runBlocking<Unit> {
        sync()
        router.on("POST", "/v1/chats/$chatId/messages") { errorResponse(404, "not_found", "Unknown chat") }
        val failed = repo.sendMessage(chatId, "hi") as SendResult.Failed
        assertEquals("Unknown chat", failed.message)
        assertTrue(repo.thread(chatId).value.pending.isEmpty())
        assertTrue(repo.sendMessage(chatId, "   ") is SendResult.Failed)
        assertEquals(1, router.count("POST /v1/chats/$chatId/messages"))
    }

    @Test
    fun `live messages update the thread the list order and the preview`() = runBlocking<Unit> {
        router.json("GET", "/v1/chats", """{"chats": [${Fixtures.chat(id = "c_other", title = "other", updatedAt = 1795549000)}, ${Fixtures.chat(updatedAt = 1795541000)}]}""")
        sync()
        repo.chats.await { it.size == 2 }
        repo.loadThread(chatId)
        assertEquals("c_other", repo.chats.value.first().id)

        source.emit(1, Fixtures.messageCreated(chatId, Fixtures.toolMessage(id = 3).replace("1795541258}", "1795549999}")))
        source.emit(1, Fixtures.messageCreated(chatId, Fixtures.userMessage(id = 1)))
        val thread = repo.thread(chatId).await { it.messages.size == 2 }
        assertEquals("kept in id order even when delivered out of order", listOf(1L, 3L), thread.messages.map { it.id })
        val chats = repo.chats.await { it.first().id == chatId }
        assertEquals(Message.ROLE_USER, chats.first().lastMessage?.role)

        // chat_updated has no last_message and no live fields: ours are kept.
        source.emit(1, """{"type": "chat_updated", "chat": ${Fixtures.chat(title = "renamed", updatedAt = 1795549999, live = false)}}""")
        val renamed = repo.chats.await { it.first().title == "renamed" }.first()
        assertEquals(1L, renamed.lastMessage?.id)

        source.emit(1, """{"type": "chat_deleted", "chat_id": "c_other"}""")
        assertEquals(listOf(chatId), repo.chats.await { it.size == 1 }.map { it.id })
    }

    @Test
    fun `activity telemetry pairing toasts and task results are exposed`() = runBlocking<Unit> {
        sync()
        source.emit(1, Fixtures.activity(step = 1, kind = "ssh_exec", summary = "uname -a", screenshotId = null))
        source.emit(1, Fixtures.activity(step = 2))
        assertEquals(listOf(1, 2), repo.activity.await { it.size == 2 }.map { it.step })

        source.emit(1, """{"type": "telemetry_tick", "telemetry": ${Fixtures.TELEMETRY}}""")
        assertEquals(84, repo.telemetry.await { it != null }?.battery?.percent)

        source.emit(1, """{"type": "pairing", "mode": "pin", "pin": "560443", "device_name": "LapisZero-MOCK", "status": "waiting", "expires_in": 300, "expires_at": 1}""")
        assertEquals("pin", repo.pairing.await { it != null }?.mode)
        source.emit(1, """{"type": "pairing", "mode": "off"}""")
        assertNull(repo.pairing.await { it == null })

        val notices = CopyOnWriteArrayList<Notice>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { repo.notices.collect { notices += it } }
        source.emit(1, """{"type": "toast", "level": "warn", "text": "Stuck loop detected"}""")
        source.emit(1, """{"type": "task_finished", "task_id": "t_1", "chat_id": "$chatId", "host": "macbook-pro", "status": "done", "summary": "ok"}""")
        source.emit(1, """{"type": "from_the_future", "x": 1}""")
        notices.awaitSize(2)
        assertEquals(Notice.Toast("warn", "Stuck loop detected"), notices[0])
        assertEquals("done", (notices[1] as Notice.TaskDone).finished.status)
    }

    @Test
    fun `the head follows the frames and its holds and beats run out by the clock`() = runBlocking<Unit> {
        assertEquals(HeadState.IDLE, repo.head.value)
        val seen = CopyOnWriteArrayList<HeadState>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { repo.head.collect { seen += it } }

        // A screenshot that has come and gone between two frames is still seen: 1.2 s of it,
        // counted from when it was first seen (so from no earlier than `began`).
        val began = System.nanoTime()
        sync(json = Fixtures.stateSync("ACTING", activeChatId = chatId, step = 1, action = "screenshot"))
        repo.head.await { it == HeadState.FETCHING }
        source.emit(1, Fixtures.stateSync("THINKING", activeChatId = chatId, step = 1, action = ""))
        repo.state.await { it?.agentState == "THINKING" }
        repo.head.await { it == HeadState.THINKING }
        val held = (System.nanoTime() - began) / 1_000_000
        assertTrue("held for $held ms", held >= HeadTrack.STEP_HOLD_MS - 1)

        // The task ends: one beat, then idle, with no frame to say so.
        source.emit(1, Fixtures.stateSync("IDLE"))
        seen.awaitSize(4) // (the collector has seen the idle before the beat replaces it)
        val ended = System.nanoTime()
        source.emit(1, Fixtures.taskFinished("done"))
        repo.head.await { it == HeadState.DONE }
        repo.head.await { it == HeadState.IDLE }
        val beat = (System.nanoTime() - ended) / 1_000_000
        assertTrue("the beat lasted $beat ms", beat >= HeadTrack.BEAT_MS - 1)
        assertEquals(
            listOf(HeadState.IDLE, HeadState.FETCHING, HeadState.THINKING, HeadState.IDLE, HeadState.DONE, HeadState.IDLE),
            seen.awaitSize(6).toList(),
        )

        // Older software: no `action`, so the kind of the step that last reported back.
        source.emit(1, Fixtures.stateSync("ACTING", activeChatId = chatId, step = 2))
        repo.head.await { it == HeadState.WORKING }
        source.emit(1, Fixtures.activity(kind = "ssh_exec", summary = "uname -a", screenshotId = null))
        repo.head.await { it == HeadState.COMMAND }

        // A new connection starts the head over from its snapshot: nothing is carried across.
        source.emit(2, Fixtures.stateSync("PAUSED", activeChatId = chatId, step = 2))
        repo.head.await { it == HeadState.PAUSED }
        source.emit(3, Fixtures.stateSync("THINKING", activeChatId = chatId, step = 2))
        assertEquals(HeadState.THINKING, repo.head.await { it != HeadState.PAUSED }) // not the old step's hold
    }

    @Test
    fun `the step ticker is rebuilt from the active chat after a resync`() = runBlocking<Unit> {
        router.json(
            "GET", "/v1/chats/$chatId",
            """{"chat": ${Fixtures.chat()}, "messages": [${Fixtures.userMessage()}, ${Fixtures.toolMessage(id = 3, screenshotId = 7)}, ${Fixtures.assistantMessage()}], "has_more": false}""",
        )
        sync(json = Fixtures.stateSync("ACTING", activeChatId = chatId, step = 2))
        val seeded = repo.activity.await { it.isNotEmpty() }.single()
        assertEquals("vnc_click", seeded.kind)
        assertEquals("click(160, 24) left", seeded.summary)
        assertEquals(7L, seeded.screenshotId)
        assertEquals(2, seeded.step)
    }

    @Test
    fun `chat crud goes through the api and updates the list`() = runBlocking<Unit> {
        sync()
        repo.chats.await { it.isNotEmpty() }
        router.json("POST", "/v1/chats", Fixtures.chat(id = "c_new", title = "New chat", updatedAt = 1795549999), code = 201)
        val created = repo.createChat("macbook-pro").getOrThrow()
        assertEquals("c_new", created.id)
        assertEquals("c_new", repo.chats.value.first().id)

        router.json("PATCH", "/v1/chats/c_new", Fixtures.chat(id = "c_new", title = "Deploy check", updatedAt = 1795549999))
        assertEquals("Deploy check", repo.renameChat("c_new", " Deploy check ").getOrThrow().title)
        assertEquals("Deploy check", repo.chats.value.first { it.id == "c_new" }.title)

        router.on("PATCH", "/v1/chats/c_new") { request ->
            assertTrue(request.body!!.utf8().contains("\"policy_override\":null"))
            jsonResponse(Fixtures.chat(id = "c_new"))
        }
        assertTrue(repo.setChatPolicy("c_new", null).isSuccess)

        router.on("DELETE", "/v1/chats/c_new") { MockResponse.Builder().code(204).build() }
        assertTrue(repo.deleteChat("c_new").isSuccess)
        assertTrue(repo.chats.value.none { it.id == "c_new" })

        router.on("POST", "/v1/chats") { errorResponse(422, "invalid", "Unknown host: nope") }
        assertEquals("Unknown host: nope", repo.createChat("nope").exceptionOrNull()?.message)
    }

    @Test
    fun `older pages are prepended and cached chats are offered while offline`() = runBlocking<Unit> {
        val cache = object : ChatCache by ChatCache.None {
            override suspend fun chats(deviceId: String): List<Chat> =
                listOf(LapisJson.decodeFromString(Fixtures.chat(id = "c_cached", title = "from cache")))
        }
        val offline = ChatRepository(server.api(), FakeEventSource(), scope, cache, "dev")
        offline.start()
        assertEquals("from cache", offline.chats.await { it.isNotEmpty() }.single().title)

        sync()
        router.on("GET", "/v1/chats/$chatId") { request ->
            if (request.url.queryParameter("before") == "40") {
                jsonResponse("""{"chat": ${Fixtures.chat()}, "messages": [${Fixtures.userMessage(id = 10)}], "has_more": false}""")
            } else {
                jsonResponse("""{"chat": ${Fixtures.chat()}, "messages": [${Fixtures.toolMessage(id = 40)}, ${Fixtures.assistantMessage(id = 41)}], "has_more": true}""")
            }
        }
        repo.loadThread(chatId)
        assertTrue(repo.thread(chatId).value.hasMore)
        repo.loadOlder(chatId)
        val thread = repo.thread(chatId).value
        assertEquals(listOf(10L, 40L, 41L), thread.messages.map { it.id })
        assertFalse(thread.hasMore)
    }

    @Test
    fun `a failed thread load keeps the screen usable`() = runBlocking<Unit> {
        sync()
        router.on("GET", "/v1/chats/$chatId") { errorResponse(404, "not_found", "Unknown chat") }
        repo.loadThread(chatId)
        val thread = repo.thread(chatId).value
        assertFalse(thread.loading)
        assertEquals("Unknown chat", thread.error)
    }
}
