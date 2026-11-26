package dev.lapiszero.app.domain

import dev.lapiszero.app.data.remote.ClientCommand
import dev.lapiszero.app.data.remote.DeckSummary
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.EventSource
import dev.lapiszero.app.data.remote.LapisEvent
import dev.lapiszero.app.data.remote.MeshMessage
import dev.lapiszero.app.data.remote.SdrDecode
import dev.lapiszero.app.data.remote.SdrSpectrum
import dev.lapiszero.app.data.remote.WsFrame
import java.util.Locale
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** Whether the Deck screen, tab and alert feed exist for this device. */
object DeckGate {
    /**
     * The live snapshot decides; before the first one (offline at start) the kind remembered for
     * the paired device does. A Lapis-Zero never says it is a deck, so it never gets Deck UI.
     */
    fun isDeck(state: DeviceState?, device: PairedDevice): Boolean = state?.isDeck ?: device.isDeck
}

/** One line of the live SDR / mesh alert feed. */
data class DeckAlert(
    val kind: Kind,
    val title: String,
    val text: String,
    val atMs: Long,
    /** From the synthetic IQ source or a mock mesh, not from the air. */
    val synthetic: Boolean = false,
    /** Our own transmission echoed back: shown, but not counted as news. */
    val outgoing: Boolean = false,
) {
    enum class Kind { MESH, SDR }

    companion object {
        fun mesh(m: MeshMessage, atMs: Long) = DeckAlert(
            kind = Kind.MESH,
            title = "MESH ch${m.channel} · " + if (m.outgoing) "you → ${recipient(m.to)}" else m.from.ifBlank { "?" },
            text = m.text,
            atMs = atMs,
            outgoing = m.outgoing,
        )

        fun sdr(d: SdrDecode, atMs: Long) = DeckAlert(
            kind = Kind.SDR,
            title = "${d.decoder.ifBlank { "decoder" }} · ${formatMhz(d.freqHz)}",
            text = describePacket(d.packet),
            atMs = atMs,
            synthetic = d.synthetic,
        )

        /** Meshtastic's broadcast address `^all` (or nothing) reads "all". */
        fun recipient(to: String): String = if (to.isBlank() || to == "^all") "all" else to

        /** `model=Acurite-Tower id=1234 temperature_C=21.3`: the scalar fields, model first. */
        fun describePacket(packet: JsonElement?, max: Int = 8): String = when (packet) {
            null, JsonNull -> ""
            is JsonPrimitive -> packet.content
            is JsonArray -> packet.joinToString(", ") { describePacket(it, max) }
            is JsonObject -> packet.entries
                .filter { (k, v) -> v is JsonPrimitive && v !is JsonNull && k != "time" }
                .sortedBy { (k, _) -> if (k == "model" || k == "type") 0 else 1 }
                .take(max)
                .joinToString(" ") { (k, v) -> "$k=${(v as JsonPrimitive).content}" }
        }
    }
}

/** 433920000 -> "433.920 MHz". */
fun formatMhz(hz: Long): String = String.format(Locale.US, "%.3f MHz", hz / 1e6)

/**
 * Lapis-Deck live data for one session: the `deck` block of every `state_sync`, the SDR / mesh
 * alert feed, live mesh messages and the waterfall rows of a subscribed socket. Everything else
 * Deck-specific is plain REST, asked by the screen that shows it.
 *
 * Subscribes before [ChatRepository.start] opens the socket, so no frame is missed.
 */
class DeckRepository(
    private val source: EventSource,
    private val scope: CoroutineScope,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private val _summary = MutableStateFlow<DeckSummary?>(null)
    val summary: StateFlow<DeckSummary?> = _summary.asStateFlow()

    private val _alerts = MutableStateFlow<List<DeckAlert>>(emptyList())

    /** Newest first, capped at [MAX_ALERTS]. */
    val alerts: StateFlow<List<DeckAlert>> = _alerts.asStateFlow()

    private val _unseen = MutableStateFlow(0)

    /** Alerts that arrived since the Deck screen was last looked at (own transmissions excluded). */
    val unseen: StateFlow<Int> = _unseen.asStateFlow()

    private val _mesh = MutableStateFlow<List<MeshMessage>>(emptyList())

    /** Mesh messages seen on the socket this session, oldest first, capped at [MAX_MESH]. */
    val liveMesh: StateFlow<List<MeshMessage>> = _mesh.asStateFlow()

    private val _spectrum = MutableStateFlow<SdrSpectrum?>(null)
    val spectrum: StateFlow<SdrSpectrum?> = _spectrum.asStateFlow()

    @Volatile
    private var spectrumWanted = false

    @Volatile
    private var watching = false
    private var syncedEpoch = -1
    private var started = false

    @Synchronized
    fun start() {
        if (started) return
        started = true
        scope.launch(start = CoroutineStart.UNDISPATCHED) { source.frames.collect { onFrame(it) } }
    }

    internal fun onFrame(frame: WsFrame) {
        when (val event = frame.event) {
            is LapisEvent.StateSync -> {
                _summary.value = event.state.deck
                if (frame.epoch != syncedEpoch) {
                    syncedEpoch = frame.epoch
                    // The subscription belongs to the socket: a new connection must ask again.
                    if (spectrumWanted) source.send(ClientCommand.SubscribeSpectrum(true))
                }
            }

            is LapisEvent.MeshMessageEvent -> {
                val m = event.message
                _mesh.update { list -> if (list.any { it.id == m.id && m.id != 0L }) list else (list + m).takeLast(MAX_MESH) }
                add(DeckAlert.mesh(m, nowMs()))
            }

            is LapisEvent.SdrDecodeEvent -> add(DeckAlert.sdr(event.decode, nowMs()))
            is LapisEvent.SdrSpectrumEvent -> if (spectrumWanted) _spectrum.value = event.spectrum
            else -> Unit
        }
    }

    private fun add(alert: DeckAlert) {
        _alerts.update { (listOf(alert) + it).take(MAX_ALERTS) }
        if (!watching && !alert.outgoing) _unseen.update { it + 1 }
    }

    /** The Deck screen is on screen (true) or left (false): what arrives meanwhile is seen. */
    fun setWatching(on: Boolean) {
        watching = on
        if (on) _unseen.value = 0
    }

    /** Live waterfall rows while the SDR card is on screen; re-sent after every reconnect. */
    fun setSpectrumSubscribed(enabled: Boolean) {
        spectrumWanted = enabled
        source.send(ClientCommand.SubscribeSpectrum(enabled))
        if (!enabled) _spectrum.value = null
    }

    fun clearAlerts() {
        _alerts.value = emptyList()
        _unseen.value = 0
    }

    companion object {
        const val MAX_ALERTS = 100
        const val MAX_MESH = 200
    }
}
