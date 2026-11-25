package dev.lapiszero.app.data.remote

import java.util.concurrent.atomic.AtomicInteger
import kotlin.math.min
import kotlin.random.Random
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/** One decoded server frame. `epoch` changes on every new connection. */
data class WsFrame(val epoch: Int, val event: LapisEvent)

sealed interface WsStatus {
    data object Idle : WsStatus
    data class Connecting(val attempt: Int) : WsStatus

    /** The socket is open and the first `state_sync` of this connection has arrived. */
    data class Connected(val epoch: Int) : WsStatus

    /** Disconnected; the next attempt starts in `delayMs`. */
    data class Waiting(val attempt: Int, val delayMs: Long) : WsStatus

    /** The daemon answered 401: the token was revoked. Reconnecting would not help. */
    data object Unauthorized : WsStatus
}

/** Exponential backoff: base * factor^attempt, capped, with optional +/- jitter. */
class Backoff(
    private val baseMs: Long = 500,
    private val maxMs: Long = 30_000,
    private val factor: Double = 2.0,
    private val jitterRatio: Double = 0.2,
    private val random: Random = Random.Default,
) {
    fun delayFor(attempt: Int): Long {
        var value = baseMs.toDouble()
        repeat(attempt.coerceIn(0, 32)) { value = min(value * factor, maxMs.toDouble()) }
        val capped = min(value, maxMs.toDouble())
        val jitter = if (jitterRatio > 0) capped * jitterRatio * (random.nextDouble() * 2 - 1) else 0.0
        return (capped + jitter).toLong().coerceIn(0, maxMs)
    }
}

/** What the repositories need from the live event stream; faked in unit tests. */
interface EventSource {
    val frames: SharedFlow<WsFrame>
    val status: StateFlow<WsStatus>

    fun start()

    fun stop()

    fun reconnectNow()

    /** Returns false when the command could not be queued on an open socket. */
    fun send(command: ClientCommand): Boolean
}

/**
 * Reconnecting client for `/v1/ws` (spec 5.4).
 *
 * The first frame of every connection is a full `state_sync`; consumers compare
 * [WsFrame.epoch] with the last one they synced and rebuild their state when it changes.
 * `http` must already carry the bearer interceptor.
 */
class LapisWsClient(
    private val http: OkHttpClient,
    private val url: String,
    private val scope: CoroutineScope,
    private val backoff: Backoff = Backoff(),
    private val pingIntervalMs: Long = 15_000,
    private val idleTimeoutMs: Long = 45_000,
    private val nowMs: () -> Long = { System.nanoTime() / 1_000_000 },
) : EventSource {
    private val _frames = MutableSharedFlow<WsFrame>(extraBufferCapacity = 64)
    override val frames: SharedFlow<WsFrame> = _frames.asSharedFlow()

    private val _status = MutableStateFlow<WsStatus>(WsStatus.Idle)
    override val status: StateFlow<WsStatus> = _status.asStateFlow()

    private val epochs = AtomicInteger(0)
    private val kick = Channel<Unit>(Channel.CONFLATED)
    private var job: Job? = null

    @Volatile
    private var socket: WebSocket? = null

    @Synchronized
    override fun start() {
        if (job?.isActive == true) return
        job = scope.launch { runLoop() }
    }

    @Synchronized
    override fun stop() {
        job?.cancel()
        job = null
        socket?.cancel()
        socket = null
        _status.value = WsStatus.Idle
    }

    /** Skip the current backoff wait (the app came to the foreground, the network returned). */
    override fun reconnectNow() {
        if (_status.value is WsStatus.Unauthorized) {
            stop()
            start()
        } else {
            kick.trySend(Unit)
        }
    }

    /** Returns false when there is no open socket; callers fall back to REST. */
    override fun send(command: ClientCommand): Boolean {
        if (_status.value !is WsStatus.Connected) return false
        return socket?.send(command.encode()) ?: false
    }

    private suspend fun runLoop() {
        var attempt = 0
        while (scope.isActive) {
            _status.value = WsStatus.Connecting(attempt)
            val outcome = runConnection()
            if (outcome.unauthorized) {
                _status.value = WsStatus.Unauthorized
                return
            }
            // A connection that reached state_sync was healthy: start the ladder again.
            attempt = if (outcome.synced) 0 else attempt + 1
            val wait = backoff.delayFor(attempt)
            _status.value = WsStatus.Waiting(attempt, wait)
            withTimeoutOrNull(wait) { kick.receive() }
        }
    }

    private class Outcome(val synced: Boolean, val unauthorized: Boolean)

    private sealed interface Signal {
        class Text(val text: String) : Signal
        class Ended(val httpCode: Int?) : Signal
    }

    private suspend fun runConnection(): Outcome = coroutineScope {
        val epoch = epochs.incrementAndGet()
        val signals = Channel<Signal>(Channel.UNLIMITED)
        val ws = http.newWebSocket(
            Request.Builder().url(url).build(),
            object : WebSocketListener() {
                override fun onMessage(webSocket: WebSocket, text: String) {
                    signals.trySend(Signal.Text(text))
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(1000, null)
                    signals.trySend(Signal.Ended(null))
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    signals.trySend(Signal.Ended(null))
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    signals.trySend(Signal.Ended(response?.code))
                }
            },
        )
        socket = ws
        var synced = false
        var unauthorized = false
        var lastRx = nowMs()
        val pinger = launch {
            while (isActive) {
                delay(pingIntervalMs)
                if (nowMs() - lastRx > idleTimeoutMs) {
                    // Half-open connection: nothing (not even a pong) arrived for too long.
                    ws.cancel()
                    signals.trySend(Signal.Ended(null))
                    break
                }
                ws.send(ClientCommand.Ping.encode())
            }
        }
        try {
            for (signal in signals) {
                when (signal) {
                    is Signal.Text -> {
                        lastRx = nowMs()
                        val event = LapisEvent.parse(signal.text) ?: continue
                        if (event is LapisEvent.Pong) continue
                        if (!synced) {
                            // Spec: the first frame is the full snapshot. Anything before it
                            // would be applied to stale state, so it is dropped.
                            if (event !is LapisEvent.StateSync) continue
                            synced = true
                            _status.value = WsStatus.Connected(epoch)
                        }
                        _frames.emit(WsFrame(epoch, event))
                    }

                    is Signal.Ended -> {
                        unauthorized = signal.httpCode == 401
                        break
                    }
                }
            }
        } finally {
            pinger.cancel()
            ws.cancel()
            if (socket === ws) socket = null
        }
        Outcome(synced, unauthorized)
    }
}
