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

    // ------------------------------------------------------------------ Lapis-Deck
    // Captured from `lapis-daemon --mock-hw` (Lapis-Deck 0.1.0), trimmed where noted.

    const val DECK_RAILS = """[{"detail": "mock", "name": "gps", "on": true, "switchable": true},
        {"detail": "mock", "name": "lora", "on": true, "switchable": true},
        {"detail": "mock", "name": "sdr", "on": true, "switchable": true},
        {"detail": "mock", "name": "usb_hub", "on": true, "switchable": true},
        {"detail": "rtl_biast, not readable", "name": "bias_tee", "on": null, "switchable": true},
        {"detail": "mock", "name": "lte", "on": false, "switchable": true},
        {"detail": "mock", "name": "wifi", "on": true, "switchable": true},
        {"detail": "mock", "name": "bluetooth", "on": true, "switchable": true}]"""

    const val DECK_SUBSYSTEMS = """{"gps": {"fix": "3D", "pps_locked": true, "status": "connected (mock)", "used": 7},
        "mesh": {"messages": 3, "nodes": 4, "status": "connected (mock)"},
        "sdr": {"center_hz": 100000000, "status": "synthetic", "synthetic": true}}"""

    /** A Lapis-Deck `state_sync`: the Lapis-Zero fields plus `device.kind` and `deck`. */
    fun deckStateSync(agentState: String = "IDLE", approvals: String = ""): String =
        stateSync(agentState = agentState, approvals = approvals)
            .replace(""""device": {"hostname": "lapis-zero", "version": "0.2.0", "mock": true}}""",
                """"device": {"hostname": "lapis-deck", "version": "0.1.0", "mock": true, "kind": "lapis-deck"},
                "deck": {"battery_w": 3.21, "harness": "down", "rails": $DECK_RAILS, "summary": $DECK_SUBSYSTEMS}}""")

    const val DECK_HEALTH = """{"device_id": "dev_local", "harness": false, "hostname": "lapis-deck", "kind": "lapis-deck",
        "mock": true, "ok": true, "uptime_sec": 8, "version": "0.1.0"}"""

    const val DECK_STATUS = """{"battery": {"charging": false, "current_a": -0.82, "percent": 78, "power_w": 3.21,
        "status": "Discharging", "voltage_v": 3.92}, "deck": $DECK_SUBSYSTEMS, "harness": false, "mock": true,
        "net": {"tailscale_state": "connected", "wifi_ssid": "lapis-lab"}, "rails": $DECK_RAILS}"""

    /** Trimmed to the tools the Deck screen offers. */
    const val DECK_TOOLS = """{"tools": [
        {"name": "sdr.decode_ism", "description": "Decode ISM-band sensors (rtl_433) for a while.", "tier": 0,
         "input_schema": {"type": "object", "properties": {"freq_hz": {"type": "integer"}, "duration_s": {"type": "integer", "maximum": 60}}, "required": [], "additionalProperties": false}},
        {"name": "sdr.adsb_snapshot", "description": "Aircraft currently decoded by readsb (ADS-B, 1090 MHz).", "tier": 0, "input_schema": {"type": "object"}},
        {"name": "mesh.send_message", "description": "Transmit a text message on a Meshtastic channel (LoRa TX).", "tier": 1,
         "input_schema": {"type": "object", "properties": {"channel": {"type": "integer", "maximum": 7}, "text": {"type": "string", "maxLength": 228}, "to": {"type": "string"}}, "required": ["channel", "text"]}},
        {"name": "net.wifi_scan_passive", "description": "Wi-Fi networks the radio has already heard (no probe requests).", "tier": 0, "input_schema": {"type": "object"}},
        {"name": "net.tailscale_peers", "description": "Tailscale peers with IP, OS and online state.", "tier": 0, "input_schema": {"type": "object"}},
        {"name": "hw.gpio_rail", "description": "Switch a power rail.", "tier": 2, "input_schema": {"type": "object"}}]}"""

    const val DECK_MESH = """{"messages": [{"channel": 0, "from": "!a1b2c3d4", "hops": 0, "id": 1, "outgoing": false, "rssi": -90,
        "rx_time": 1795653416, "snr": 5.0, "text": "net check 19:00, all stations", "to": "^all"},
        {"channel": 0, "from": "!0badf00d", "hops": 1, "id": 2, "outgoing": false, "rssi": -90, "rx_time": 1795653417,
        "snr": 5.0, "text": "relay up on the ridge, 3 bars", "to": "^all"}],
        "my_node": "!decafbad",
        "nodes": [{"alt_m": 30, "hops": 1, "id": "!0badf00d", "last_heard": 1795653416, "lat": 37.7925, "lon": -122.438,
        "long_name": "Ridge Relay", "num": 195948557, "rssi": -96, "short_name": "RDG", "snr": 4.25},
        {"alt_m": 30, "hops": 0, "id": "!a1b2c3d4", "last_heard": 1795653416, "lat": 37.779, "lon": -122.417,
        "long_name": "Base Camp", "num": 2712847316, "rssi": -71, "short_name": "BASE", "snr": 9.5},
        {"alt_m": 30, "hops": 0, "id": "!decafbad", "last_heard": 1795653416, "lat": 37.7749, "lon": -122.4194,
        "long_name": "Lapis Deck", "num": 3737844653, "rssi": null, "short_name": "LDK", "snr": null}],
        "status": "connected (mock)"}"""

    const val DECK_GPS = """{"device": "/dev/ttyS0", "fix": {"alt_m": 16.2, "eph_m": 3.1, "fix": "3D", "lat": 37.7749,
        "lon": -122.4194, "mode": 3, "speed_mps": 0.31, "time": "2026-11-25T18:00:00.000Z", "track_deg": 12.5},
        "hdop": 0.92, "pps_locked": true, "pps_offset_ns": 120,
        "satellites": [{"az": 288, "el": 61, "gnss": 0, "prn": 2, "snr": 41, "used": true},
        {"az": 40, "el": 12, "gnss": 0, "prn": 5, "snr": 22, "used": false}],
        "status": "connected (mock)", "used": 7, "visible": 9}"""

    const val DECK_SDR = """{"ceil_db": -31.06, "fft_size": 1024, "floor_db": -80.70,
        "peaks": [{"freq_hz": 100171000.0, "power_db": -35.26, "snr_db": 39.51}, {"freq_hz": 100751000.0, "power_db": -37.85, "snr_db": 36.92}],
        "source": {"center_hz": 100000000, "gain_db": 30.0, "kind": "synthetic", "sample_rate": 2048000, "synthetic": true},
        "status": "synthetic"}"""

    const val DECK_AUDIT = """{"entries": [{"data": {"by": "android:Dev phone", "decision": "deny", "host": "deck",
        "request_id": "req_2", "summary": "gps rail OFF", "tier": 2, "tool": "hw.gpio_rail"}, "event": "approval", "ts": 1795653458},
        {"data": {"by": "android:Dev phone", "decision": "approve_once", "host": "deck", "request_id": "req_1",
        "summary": "mesh.send_message", "tier": 1, "tool": "mesh.send_message"}, "event": "approval", "ts": 1795653456}]}"""

    const val INVOKE_WIFI = """{"decision": "auto", "ok": true, "result": {"mock": true, "networks": [
        {"band": "5 GHz", "bssid": "AA:BB:CC:00:11:22", "channel": 36, "freq_mhz": 5180, "rssi_dbm": -59, "security": "WPA2", "signal_pct": 82, "ssid": "lapis-lab"},
        {"band": "2.4 GHz", "bssid": "10:20:30:40:50:60", "channel": 11, "freq_mhz": 2462, "rssi_dbm": -80, "security": "--", "signal_pct": 41, "ssid": "cafe-guest"}]}, "tier": 0}"""

    const val INVOKE_ADSB = """{"decision": "auto", "ok": true, "result": {"synthetic": true, "table": {"aircraft": [
        {"alt_baro": 34000, "flight": "UAL1543", "gs": 451.2, "hex": "a1b2c3", "squawk": "4213", "seen": 0.4},
        {"alt_baro": "ground", "flight": null, "gs": null, "hex": "abc123", "squawk": null, "seen": 12.0}],
        "messages": 48211, "now": 1764100000.0}}, "tier": 0}"""

    const val INVOKE_ISM = """{"decision": "auto", "ok": true, "result": {"freq_hz": 433920000, "packets": [
        {"battery_ok": 1, "channel": "A", "humidity": 61, "id": 11425, "mic": "CHECKSUM", "model": "Acurite-Tower", "temperature_C": 18.4, "time": "1764100000"}],
        "synthetic": true}, "tier": 0}"""

    const val INVOKE_MESH_OK = """{"decision": "approve_once", "ok": true, "result": {"channel": 0, "id": 1275100968, "queued": true, "to": "^all"}, "tier": 1}"""

    const val INVOKE_DENIED = """{"by": "android:Dev phone", "decision": "deny", "error": "hw.gpio_rail needs hold-to-confirm", "ok": false, "tier": 2}"""

    /** A Deck approval: Tier 2 waits for a hold, Tier 1 for a tap; both have no chat and no step. */
    fun deckApproval(requestId: String = "req_2", tier: Int = 2): String {
        val (action, summary, reason, hold, risk) = if (tier >= 2) {
            listOf("hw.gpio_rail", "gps rail OFF", "hw.gpio_rail needs hold-to-confirm", "1500", "high")
        } else {
            listOf("mesh.send_message", "Send a LoRa message on ch0", "mesh.send_message needs a [Y/N]", "0", "low")
        }
        return """{"type": "approval_request", "request_id": "$requestId", "host": "deck", "action": "$action",
            "summary": "$summary", "risk": "$risk", "preview_1bit_120x68_b64": null, "task_id": "", "chat_id": null,
            "step": 0, "screenshot_id": null, "click_x": null, "click_y": null, "tier": $tier,
            "reason": "$reason", "detail": "", "source": "android", "hold_ms": $hold, "expires_at": 1795654057}"""
    }

    fun meshMessageFrame(id: Long = 7, text: String = "rover checking in", outgoing: Boolean = false) =
        """{"type": "mesh_message", "message": {"channel": 0, "from": "!c0ffee01", "hops": 2, "id": $id,
            "outgoing": $outgoing, "rssi": -114, "rx_time": 1795653500, "snr": -3.5, "text": "$text", "to": "^all"}}"""

    const val SDR_DECODE_FRAME = """{"type": "sdr_decode", "decoder": "rtl_433", "freq_hz": 433920000,
        "packet": {"model": "Acurite-Tower", "id": 11425, "temperature_C": 18.4, "humidity": 61, "time": "1764100000"},
        "synthetic": true}"""

    /** 512 bins, base64; a peak in the middle. */
    fun spectrumFrame(centerHz: Long = 100_000_000): String {
        val bins = ByteArray(512) { i -> if (i in 250..262) 240.toByte() else 30 }
        val b64 = java.util.Base64.getEncoder().encodeToString(bins)
        return """{"type": "sdr_spectrum", "center_hz": $centerHz, "sample_rate": 2048000, "gain_db": 30.0,
            "synthetic": true, "source": "synthetic", "floor_db": -80.7, "ceil_db": -31.0, "bins": "$b64",
            "peaks": [{"freq_hz": 100171000.0, "power_db": -35.2, "snr_db": 39.5}]}"""
    }
}
