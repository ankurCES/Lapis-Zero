package dev.lapiszero.app

import dev.lapiszero.app.data.local.SecureStore
import dev.lapiszero.app.data.remote.ClientCommand
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.EventSource
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisEvent
import dev.lapiszero.app.data.remote.WsFrame
import dev.lapiszero.app.data.remote.WsStatus
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.withTimeout
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import org.junit.rules.TestWatcher
import org.junit.runner.Description

/** In-memory stand-in for the Keystore-backed vault. */
class MemorySecureStore(var blob: String? = null) : SecureStore {
    var writes = 0

    override fun read(): String? = blob

    override fun write(value: String) {
        writes++
        blob = value
    }

    override fun clear() {
        blob = null
    }
}

/** A scripted event stream: tests push frames and inspect what the app tried to send. */
class FakeEventSource : EventSource {
    private val _frames = MutableSharedFlow<WsFrame>(extraBufferCapacity = 64)
    override val frames = _frames
    override val status = MutableStateFlow<WsStatus>(WsStatus.Idle)
    val sent = CopyOnWriteArrayList<ClientCommand>()
    var started = false
    var reconnects = 0

    /** When false, `send` behaves like a closed socket. */
    var open = true

    override fun start() {
        started = true
    }

    override fun stop() {
        started = false
    }

    override fun reconnectNow() {
        reconnects++
    }

    override fun send(command: ClientCommand): Boolean {
        if (!open) return false
        sent += command
        return true
    }

    suspend fun emit(epoch: Int, json: String) {
        val event = LapisEvent.parse(json) ?: error("unparseable test frame: $json")
        if (event is LapisEvent.StateSync) status.value = WsStatus.Connected(epoch)
        _frames.emit(WsFrame(epoch, event))
    }
}

/** Routes MockWebServer requests by "METHOD /path" and remembers every request. */
class Router : Dispatcher() {
    private val handlers = HashMap<String, (RecordedRequest) -> MockResponse>()
    val requests = LinkedBlockingQueue<RecordedRequest>()
    val log = CopyOnWriteArrayList<String>()

    /** Every request body with its "METHOD /path", for "this never leaves in any other request". */
    val bodies = CopyOnWriteArrayList<Pair<String, String>>()

    fun on(method: String, path: String, handler: (RecordedRequest) -> MockResponse) {
        handlers["$method $path"] = handler
    }

    fun json(method: String, path: String, body: String, code: Int = 200) =
        on(method, path) { jsonResponse(body, code) }

    override fun dispatch(request: RecordedRequest): MockResponse {
        val key = "${request.method} ${request.url.encodedPath}"
        log += key
        requests += request
        request.body?.let { bodies += key to it.utf8() }
        return handlers[key]?.invoke(request)
            ?: jsonResponse("""{"error":{"code":"not_found","message":"no route for $key"}}""", 404)
    }

    fun count(key: String): Int = log.count { it == key }

    fun take(key: String, timeoutMs: Long = 5_000): RecordedRequest {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (true) {
            val left = deadline - System.currentTimeMillis()
            val request = requests.poll(left.coerceAtLeast(1), TimeUnit.MILLISECONDS)
                ?: error("no request for $key within $timeoutMs ms; saw $log")
            if ("${request.method} ${request.url.encodedPath}" == key) return request
        }
    }
}

fun jsonResponse(body: String, code: Int = 200): MockResponse =
    MockResponse.Builder().code(code).setHeader("Content-Type", "application/json").body(body).build()

fun errorResponse(code: Int, slug: String, message: String): MockResponse =
    jsonResponse("""{"error":{"code":"$slug","message":"$message"}}""", code)

fun MockWebServer.endpoint(): Endpoint = Endpoint("${url("/").host}:$port", tls = false)

fun MockWebServer.api(token: String? = "test-token"): LapisApiClient = LapisApiClient(endpoint(), { token })

/** Waits (in real time) until the flow shows a value matching `predicate`. */
suspend fun <T> Flow<T>.await(timeoutMs: Long = 5_000, predicate: (T) -> Boolean): T =
    withTimeout(timeoutMs) { first(predicate) }

/** Waits until a list filled by a background collector has at least `count` entries. */
suspend fun <T> List<T>.awaitSize(count: Int, timeoutMs: Long = 5_000): List<T> {
    withTimeout(timeoutMs) { while (size < count) delay(10) }
    return this
}

/** Polls `condition` with a deadline, so a missing event fails the test instead of hanging it. */
suspend fun awaitTrue(timeoutMs: Long = 5_000, condition: () -> Boolean) {
    withTimeout(timeoutMs) { while (!condition()) delay(10) }
}

/** Gives `viewModelScope` a Main dispatcher on the JVM. */
@OptIn(ExperimentalCoroutinesApi::class)
class MainDispatcherRule : TestWatcher() {
    override fun starting(description: Description) = Dispatchers.setMain(Dispatchers.Unconfined)

    override fun finished(description: Description) = Dispatchers.resetMain()
}

/** Realistic frames and bodies, captured from `lapis-daemon --mock` 0.2.0. */
object Fixtures {
    const val PREVIEW_B64_PLACEHOLDER = "@@PREVIEW@@"

    fun stateSync(
        agentState: String = "IDLE",
        activeChatId: String? = null,
        step: Int = 0,
        approvals: String = "",
        pairing: String = "null",
        /** The step being executed right now; null leaves the field out, as older software does. */
        action: String? = null,
    ): String {
        val task = if (activeChatId == null) "null" else
            """{"id": "t_855667000", "title": "open settings", "step": $step, "max_steps": 25}"""
        val chat = activeChatId?.let { "\"$it\"" } ?: "null"
        val acting = action?.let { """ "action": "$it",""" }.orEmpty()
        return """{"type": "state_sync", "agent_state": "$agentState",$acting "active_host": "macbook-pro",
            "hosts": ["macbook-pro", "arch-dev"], "active_task": $task, "active_chat_id": $chat,
            "next_cron": "", "wifi_ssid": "HomeWiFi", "local_ip": "192.168.1.42",
            "tailscale_state": "connected", "tailscale_ip": "100.91.12.4",
            "tailscale_fqdn": "lapis-zero.tailnet.ts.net", "tailscale_auth_url": null,
            "api_key_ready": true, "battery_pct": 84, "battery_charging": false,
            "pending_approvals": [$approvals], "pairing": $pairing,
            "ui": {"theme": "paper", "brightness": 100, "mute_audio": false},
            "device": {"hostname": "lapis-zero", "version": "0.2.0", "mock": true}}"""
    }

    fun approval(
        requestId: String = "req_1",
        chatId: String = "c_9e838c02",
        action: String = "vnc_click",
        preview: String? = null,
    ): String {
        val click = if (action == "vnc_click") "\"click_x\": 840, \"click_y\": 412" else "\"click_x\": null, \"click_y\": null"
        val previewJson = preview?.let { "\"$it\"" } ?: "null"
        val summary = if (action == "vnc_click") "Click Retry Build at (840, 412)" else "systemctl status worker"
        return """{"type": "approval_request", "request_id": "$requestId", "host": "macbook-pro",
            "action": "$action", "summary": "$summary", "risk": "high",
            "preview_1bit_120x68_b64": $previewJson, "task_id": "t_855667000", "chat_id": "$chatId",
            "step": 5, "screenshot_id": 2, $click}"""
    }

    fun taskFinished(status: String = "done", chatId: String = "c_9e838c02") =
        """{"type": "task_finished", "task_id": "t_855667000", "chat_id": "$chatId", "host": "macbook-pro",
            "status": "$status", "summary": "Mock run complete: 6 scripted steps."}"""

    fun approvalResolved(requestId: String = "req_1", decision: String = "approve_once", by: String = "device") =
        """{"type": "approval_resolved", "request_id": "$requestId", "decision": "$decision", "by": "$by"}"""

    fun chat(
        id: String = "c_9e838c02",
        title: String = "open settings",
        updatedAt: Long = 1795541264,
        lastMessage: String = "null",
        live: Boolean = true,
        /** Null leaves `agent` and `workdir` out, as device software from before them does. */
        agent: String? = null,
        workdir: String = "",
    ): String {
        val liveFields = if (live) """, "active": false, "step": null, "pending_approvals": 0""" else ""
        val runWith = if (agent != null) """, "agent": "$agent", "workdir": "$workdir"""" else ""
        return """{"id": "$id", "title": "$title", "host_id": "macbook-pro", "model_override": null,
            "policy_override": null, "archived": false, "created_at": 1795541257,
            "updated_at": $updatedAt, "last_message": $lastMessage$liveFields$runWith}"""
    }

    /** `GET /v1/device/ssh-key` of device software that has the SSH features; a made-up key. */
    const val PUBLIC_KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMadeUpKeyForTestsOnly0123456789abcdefghijkl lapis-zero@lapis-zero"
    const val FINGERPRINT = "SHA256:MadeUpFingerprintForTestsOnly0123456789abcd"
    const val SSH_KEY = """{"public_key": "$PUBLIC_KEY", "fingerprint": "$FINGERPRINT"}"""

    const val AGENTS = """{"agents": [{"id": "claude", "name": "Claude Code", "path": "/opt/homebrew/bin/claude"},
        {"id": "opencode", "name": "opencode", "path": "/usr/local/bin/opencode"},
        {"id": "freya", "name": "Freya", "path": "/Users/alice/.local/bin/freya"}]}"""

    fun userMessage(id: Long = 1, chatId: String = "c_9e838c02", content: String = "open settings", taskId: String? = null) =
        """{"id": $id, "chat_id": "$chatId", "role": "user", "content": "$content",
            "task_id": ${taskId?.let { "\"$it\"" } ?: "null"},
            "metadata": {"source": "android:Pixel"}, "created_at": 1795541257}"""

    fun toolMessage(id: Long = 3, chatId: String = "c_9e838c02", screenshotId: Long? = 1) =
        """{"id": $id, "chat_id": "$chatId", "role": "tool", "content": "click(160, 24) left",
            "task_id": "t_855667000", "metadata": {"kind": "vnc_click", "status": "ok",
            "detail": "Click header", "screenshot_id": ${screenshotId ?: "null"}, "step": 2, "ts": 1795541258},
            "created_at": 1795541258}"""

    fun assistantMessage(id: Long = 8, chatId: String = "c_9e838c02") =
        """{"id": $id, "chat_id": "$chatId", "role": "assistant",
            "content": "Mock run complete: 6 scripted steps.", "task_id": "t_855667000",
            "metadata": {"status": "done"}, "created_at": 1795541264}"""

    fun messageCreated(chatId: String, message: String) =
        """{"type": "chat_message_created", "chat_id": "$chatId", "message": $message}"""

    fun activity(
        step: Int = 2,
        kind: String = "vnc_click",
        summary: String = "click(160, 24) left",
        screenshotId: Long? = 1,
        status: String = "ok",
    ) =
        """{"type": "activity_event", "ts": 1795541258, "task_id": "t_855667000", "kind": "$kind",
            "summary": "$summary", "detail": "Click header", "status": "$status", "chat_id": "c_9e838c02",
            "step": $step, "screenshot_id": ${screenshotId ?: "null"}}"""

    const val TELEMETRY = """{"battery": {"percent": 84, "voltage_v": 3.9, "charging": false},
        "cpu": {"temp_c": 47.5, "load1": 0.32},
        "memory": {"total_mb": 496, "available_mb": 301, "zram_total_mb": 496, "zram_used_mb": 0,
                   "daemon_rss_mb": 61, "ui_rss_mb": 6},
        "wifi": {"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi_dbm": -58},
        "tailscale": {"state": "connected", "ip": "100.91.12.4", "fqdn": "lapis-zero.tailnet.ts.net", "peers": 3},
        "uptime_sec": 9,
        "services": {"lapis-ui": "active", "lapis-daemon": "active", "tailscaled": "active",
                     "NetworkManager": "active", "bluetooth": "active"}}"""
}
