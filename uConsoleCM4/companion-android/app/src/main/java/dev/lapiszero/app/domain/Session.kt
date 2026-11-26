package dev.lapiszero.app.domain

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import dev.lapiszero.app.data.local.ChatCache
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.LapisWsClient
import dev.lapiszero.app.data.remote.UiPatch
import java.io.IOException
import java.util.TimeZone
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.distinctUntilChangedBy
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/** A decoded target-screen frame plus where the agent clicked in it (pixels of this bitmap). */
class Shot(val bitmap: Bitmap, val clickX: Int?, val clickY: Int?)

/** In-memory cache of `/v1/screenshots/{id}.jpg`; frames are immutable so ids are stable keys. */
class ScreenshotStore(private val api: LapisApiClient) {
    private val cache = object : LruCache<Long, Shot>(24 * 1024 * 1024) {
        override fun sizeOf(key: Long, value: Shot): Int = value.bitmap.allocationByteCount
    }
    private val locks = Mutex()

    fun peek(id: Long): Shot? = cache.get(id)

    /** Throws [IOException] (including "evicted on the device", a 404) when it cannot be loaded. */
    suspend fun load(id: Long): Shot {
        cache.get(id)?.let { return it }
        return locks.withLock {
            cache.get(id) ?: run {
                val raw = api.screenshot(id)
                val bitmap = withContext(Dispatchers.Default) {
                    BitmapFactory.decodeByteArray(raw.bytes, 0, raw.bytes.size)
                } ?: throw IOException("The device sent an image this phone cannot decode")
                Shot(bitmap, raw.clickX, raw.clickY).also { cache.put(id, it) }
            }
        }
    }
}

/**
 * Hands the phone's timezone to a freshly paired device so its clock shows local time
 * (`PATCH /v1/device/ui {"timezone": ...}`, spec 5.4). Best effort by design: failures are
 * never shown to the user.
 */
class TimezoneSync(
    private val api: LapisApiClient,
    private val phoneZone: () -> String = { TimeZone.getDefault().id },
) {
    /**
     * Sends the zone when it differs from what the device reports.
     *
     * @return true when the hand-over is finished (already equal, accepted, or answered with an
     * HTTP error such as 422 for a zone the device does not know); false when the device could
     * not be reached at all, so the next connection may try again.
     */
    suspend fun syncIfNeeded(state: DeviceState): Boolean {
        val zone = phoneZone()
        if (zone.isBlank() || zone == state.ui.timezone) return true
        return try {
            api.patchUi(UiPatch(timezone = zone))
            true
        } catch (e: LapisApiException) {
            true
        } catch (e: IOException) {
            false
        }
    }
}

/** Everything that talks to one paired device. Closed when another device becomes active. */
class LapisSession(
    val device: PairedDevice,
    cache: ChatCache,
    parent: CoroutineScope,
    private val onTimezoneSynced: () -> Unit = {},
    /** The kind ([DeviceKind]) the device reports, whenever it differs from the stored one. */
    private val onKind: (String) -> Unit = {},
) {
    val scope: CoroutineScope =
        CoroutineScope(SupervisorJob(parent.coroutineContext.job) + Dispatchers.Default)
    val api = LapisApiClient(device.endpoint, { device.token })
    private val ws = LapisWsClient(api.http, device.endpoint.wsUrl, scope)
    val repo = ChatRepository(api, ws, scope, cache, device.id)

    /** Lapis-Deck live data; stays empty (and unused) for a Lapis-Zero. */
    val deck = DeckRepository(ws, scope)
    val shots = ScreenshotStore(api)

    fun start() {
        // Subscribes to the socket's frames before ChatRepository opens it.
        deck.start()
        repo.start()
        // Remembered so the Deck tab is there (or not) before the next connection's first snapshot.
        scope.launch {
            repo.state.filterNotNull().map { it.deviceKind }.distinctUntilChanged().collect { kind ->
                if (kind != device.kind) onKind(kind)
            }
        }
        if (!device.timezoneSynced) {
            // Once per pairing (QR, BLE or dev connect), on the first snapshot from the device.
            scope.launch {
                val state = repo.state.filterNotNull().first()
                if (TimezoneSync(api).syncIfNeeded(state)) onTimezoneSynced()
            }
        }
    }

    fun close() {
        repo.stop()
        scope.cancel()
    }
}

/** Keeps exactly one [LapisSession] alive: the one for the active paired device. */
class SessionManager(
    private val devices: DeviceRepository,
    private val cache: ChatCache,
    private val scope: CoroutineScope,
) {
    private val _current = MutableStateFlow<LapisSession?>(null)
    val current: StateFlow<LapisSession?> = _current.asStateFlow()

    fun start() {
        scope.launch {
            devices.state.map { it.active }
                // A rename must not tear the connection down; a new token or host must.
                .distinctUntilChangedBy { d -> d?.let { listOf(it.id, it.host, it.tls, it.token) } }
                .collect { device ->
                    _current.value?.close()
                    _current.value = device?.let {
                        LapisSession(
                            it, cache, scope,
                            onTimezoneSynced = { devices.markTimezoneSynced(it.id) },
                            onKind = { kind -> devices.setKind(it.id, kind) },
                        ).also(LapisSession::start)
                    }
                }
        }
    }

    /** For notification actions: decide on the device the alert came from, active or not. */
    suspend fun decide(deviceId: String, requestId: String, decision: String): DecideResult {
        _current.value?.takeIf { it.device.id == deviceId }?.let {
            return it.repo.decide(requestId, decision)
        }
        val device = devices.find(deviceId)?.takeIf { it.isProvisioned }
            ?: return DecideResult.Failed("That device is no longer paired")
        return try {
            LapisApiClient(device.endpoint, { device.token }).decide(requestId, decision)
            DecideResult.Done
        } catch (e: LapisApiException) {
            if (e.isNotFound) DecideResult.AlreadyResolved else DecideResult.Failed(ChatRepository.describe(e))
        } catch (e: IOException) {
            DecideResult.Failed(ChatRepository.describe(e))
        }
    }

    /** Unpairs locally and forgets the cached chats of that device. */
    suspend fun forget(deviceId: String) {
        devices.remove(deviceId)
        cache.clearDevice(deviceId)
    }
}
