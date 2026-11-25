package dev.lapiszero.app.data.remote

import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.api
import dev.lapiszero.app.await
import dev.lapiszero.app.endpoint
import dev.lapiszero.app.errorResponse
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class LapisWsClientTest {
    private val server = MockWebServer()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val frames = CopyOnWriteArrayList<WsFrame>()
    private val statuses = CopyOnWriteArrayList<WsStatus>()
    private lateinit var client: LapisWsClient

    /** What the fake daemon received from the app, per connection. */
    private val received = LinkedBlockingQueue<String>()
    private val upgrades = CopyOnWriteArrayList<RecordedRequest>()

    @Before
    fun setUp() = server.start()

    @After
    fun tearDown() {
        if (::client.isInitialized) client.stop()
        scope.cancel()
        server.close()
    }

    private fun newClient(
        token: String? = "ws-token",
        // Each wait is long enough for the (conflating) status flow to be observed reliably.
        backoff: Backoff = Backoff(baseMs = 100, maxMs = 800, jitterRatio = 0.0),
        pingIntervalMs: Long = 60_000,
        idleTimeoutMs: Long = 120_000,
    ): LapisWsClient {
        client = LapisWsClient(
            http = server.api(token).http,
            url = server.endpoint().wsUrl,
            scope = scope,
            backoff = backoff,
            pingIntervalMs = pingIntervalMs,
            idleTimeoutMs = idleTimeoutMs,
        )
        scope.launch(start = CoroutineStart.UNDISPATCHED) { client.frames.collect { frames += it } }
        scope.launch(start = CoroutineStart.UNDISPATCHED) { client.status.collect { statuses += it } }
        return client
    }

    /** A daemon-side socket that runs `script` when the phone connects. */
    private fun socket(script: (WebSocket) -> Unit, onText: (WebSocket, String) -> Unit = { _, _ -> }): MockResponse =
        MockResponse.Builder().webSocketUpgrade(
            object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) = script(webSocket)

                override fun onMessage(webSocket: WebSocket, text: String) {
                    received += text
                    onText(webSocket, text)
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(1000, null)
                }
            },
        ).build()

    private fun serve(vararg responses: (Int) -> MockResponse) {
        val calls = AtomicInteger(0)
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                upgrades += request
                val index = calls.getAndIncrement()
                return responses[minOf(index, responses.size - 1)](index)
            }
        }
    }

    private suspend fun awaitFrames(count: Int, timeoutMs: Long = 5_000) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (frames.size < count) {
            check(System.currentTimeMillis() < deadline) { "only ${frames.size} of $count frames; statuses=$statuses" }
            delay(10)
        }
    }

    @Test
    fun `connects with the bearer token and starts from state_sync`() = runBlocking<Unit> {
        serve({ _ ->
            socket({ ws ->
                ws.send(Fixtures.stateSync())
                ws.send(Fixtures.activity())
            })
        })
        newClient().start()
        awaitFrames(2)
        assertEquals("/v1/ws", upgrades.first().url.encodedPath)
        assertEquals("Bearer ws-token", upgrades.first().headers["Authorization"])
        assertTrue(frames[0].event is LapisEvent.StateSync)
        assertTrue(frames[1].event is LapisEvent.ActivityEvent)
        assertEquals(listOf(1, 1), frames.map { it.epoch })
        assertEquals(WsStatus.Connected(1), client.status.value)
        assertEquals(WsStatus.Connecting(0), statuses.first { it !is WsStatus.Idle })
    }

    @Test
    fun `frames before the first state_sync are dropped`() = runBlocking<Unit> {
        serve({ _ ->
            socket({ ws ->
                ws.send(Fixtures.activity(step = 9))
                ws.send("""{"type": "toast", "level": "info", "text": "early"}""")
                ws.send(Fixtures.stateSync())
                ws.send(Fixtures.activity(step = 1))
            })
        })
        newClient().start()
        awaitFrames(2)
        delay(100)
        assertEquals(2, frames.size)
        assertTrue(frames[0].event is LapisEvent.StateSync)
        assertEquals(1, (frames[1].event as LapisEvent.ActivityEvent).activity.step)
    }

    @Test
    fun `reconnects with exponential backoff and resyncs with a new epoch`() = runBlocking<Unit> {
        serve(
            // 1: healthy connection that the daemon then drops (restart).
            { _ ->
                socket({ ws ->
                    ws.send(Fixtures.stateSync("ACTING", activeChatId = "c_1", step = 2))
                    ws.close(1001, "daemon restarting")
                })
            },
            // 2-4: the daemon is still down.
            { _ -> MockResponse.Builder().code(503).build() },
            { _ -> MockResponse.Builder().code(503).build() },
            { _ -> MockResponse.Builder().code(503).build() },
            // 5: back up, and the task is gone: the first frame is a fresh full snapshot.
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync("IDLE")) }) },
        )
        newClient().start()
        awaitFrames(2, timeoutMs = 10_000)

        val first = frames[0]
        val second = frames[1]
        assertEquals(1, first.epoch)
        assertEquals("ACTING", (first.event as LapisEvent.StateSync).state.agentState)
        assertEquals("a later connection has a later epoch", 5, second.epoch)
        assertEquals("IDLE", (second.event as LapisEvent.StateSync).state.agentState)
        assertEquals(5, upgrades.size)
        assertTrue("every attempt carries the token", upgrades.all { it.headers["Authorization"] == "Bearer ws-token" })

        // After a healthy connection the ladder restarts at the base delay, then doubles.
        val waits = statuses.filterIsInstance<WsStatus.Waiting>()
        assertEquals(listOf(0, 1, 2, 3), waits.map { it.attempt })
        assertEquals(listOf(100L, 200L, 400L, 800L), waits.map { it.delayMs })
        assertEquals(WsStatus.Connected(5), client.status.await { it is WsStatus.Connected && it.epoch == 5 })
        // The ladder is visible to the UI as it happens.
        assertTrue(statuses.indexOf(WsStatus.Connected(1)) < statuses.indexOfFirst { it is WsStatus.Waiting })
    }

    @Test
    fun `the ladder restarts after each healthy connection`() = runBlocking<Unit> {
        serve(
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync()); ws.close(1001, null) }) },
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync()); ws.close(1001, null) }) },
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync()) }) },
        )
        newClient().start()
        awaitFrames(3)
        assertEquals(listOf(1, 2, 3), frames.map { it.epoch })
        assertEquals(listOf(100L, 100L), statuses.filterIsInstance<WsStatus.Waiting>().map { it.delayMs })
    }

    @Test
    fun `a revoked token stops the client instead of hammering the device`() = runBlocking<Unit> {
        serve({ _ -> errorResponse(401, "unauthorized", "Missing, invalid or revoked bearer token") })
        newClient().start()
        client.status.await { it is WsStatus.Unauthorized }
        delay(600) // several backoff periods
        assertEquals(1, upgrades.size)
        assertTrue(frames.isEmpty())
        assertFalse(client.send(ClientCommand.KillAll))
    }

    @Test
    fun `sends an application ping on the interval and survives the pong`() = runBlocking<Unit> {
        serve({ _ ->
            socket(
                script = { ws -> ws.send(Fixtures.stateSync()) },
                onText = { ws, text -> if (text.contains("ping")) ws.send("""{"type": "pong", "ts": 1795541300}""") },
            )
        })
        newClient(pingIntervalMs = 60, idleTimeoutMs = 1_000).start()
        assertEquals("""{"type":"ping"}""", received.poll(3, TimeUnit.SECONDS))
        assertEquals("""{"type":"ping"}""", received.poll(3, TimeUnit.SECONDS))
        // Pongs keep the link alive and are not forwarded as events.
        delay(300)
        assertEquals(1, upgrades.size)
        assertEquals(1, frames.size)
    }

    @Test
    fun `a silent half-open connection is dropped and re-established`() = runBlocking<Unit> {
        serve(
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync()) }) }, // answers nothing, ever
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync()) }) },
        )
        newClient(pingIntervalMs = 50, idleTimeoutMs = 150).start()
        awaitFrames(2, timeoutMs = 5_000)
        assertEquals(listOf(1, 2), frames.take(2).map { it.epoch })
    }

    @Test
    fun `send only works on a synced connection and reaches the daemon`() = runBlocking<Unit> {
        assertFalse(newClient().send(ClientCommand.KillAll))
        serve({ _ -> socket({ ws -> ws.send(Fixtures.stateSync()) }) })
        client.start()
        client.status.await { it is WsStatus.Connected }
        assertTrue(client.send(ClientCommand.SubscribeTelemetry(true)))
        assertTrue(client.send(ClientCommand.ApprovalDecision("req_1", "deny", "nope")))
        assertEquals("""{"type":"subscribe_telemetry","enabled":true}""", received.poll(3, TimeUnit.SECONDS))
        assertEquals(
            """{"type":"approval_decision","request_id":"req_1","decision":"deny","reason":"nope"}""",
            received.poll(3, TimeUnit.SECONDS),
        )
    }

    @Test
    fun `reconnectNow skips the backoff wait`() = runBlocking<Unit> {
        serve(
            { _ -> MockResponse.Builder().code(503).build() },
            { _ -> socket({ ws -> ws.send(Fixtures.stateSync()) }) },
        )
        newClient(backoff = Backoff(baseMs = 60_000, maxMs = 60_000, jitterRatio = 0.0)).start()
        client.status.await { it is WsStatus.Waiting }
        client.reconnectNow()
        awaitFrames(1, timeoutMs = 3_000) // far sooner than the 60 s wait
        assertEquals(2, upgrades.size)
    }

    @Test
    fun `stop closes the socket and start is idempotent`() = runBlocking<Unit> {
        serve({ _ -> socket({ ws -> ws.send(Fixtures.stateSync()) }) })
        newClient().start()
        client.start()
        client.status.await { it is WsStatus.Connected }
        assertEquals(1, upgrades.size)
        client.stop()
        assertEquals(WsStatus.Idle, client.status.value)
        assertFalse(client.send(ClientCommand.Ping))
    }

    @Test
    fun `backoff doubles from the base up to the cap`() {
        val plain = Backoff(baseMs = 500, maxMs = 30_000, jitterRatio = 0.0)
        assertEquals(
            listOf(500L, 1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L),
            (0..7).map(plain::delayFor),
        )
        assertEquals(30_000L, plain.delayFor(1_000))
        val jittered = Backoff(baseMs = 500, maxMs = 30_000, jitterRatio = 0.2, random = Random(7))
        repeat(200) {
            assertTrue(jittered.delayFor(0) in 400..600)
            assertTrue(jittered.delayFor(3) in 3_200..4_800)
            assertTrue(jittered.delayFor(20) in 24_000..30_000)
        }
        // Jitter actually varies.
        assertTrue((1..50).map { jittered.delayFor(2) }.toSet().size > 5)
    }
}
