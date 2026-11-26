package dev.lapiszero.app.data.remote

import java.util.Base64
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/*
 * Lapis-Deck additions to the API (`/v1/deck/...`, the `deck` block of `state_sync`, and the
 * `mesh_message` / `sdr_decode` / `sdr_spectrum` frames). A Lapis-Zero daemon sends none of this.
 * Same rules as ProtocolModels.kt: snake_case via SerialName, unknown keys ignored, every field
 * optional so a newer daemon keeps decoding.
 */

// ---------------------------------------------------------------- power

/** One switchable supply. `on` is null when the state cannot be read back (bias tee, 4G). */
@Serializable
data class Rail(
    val name: String,
    val on: Boolean? = null,
    val switchable: Boolean = false,
    val detail: String = "",
) {
    /** Switching it off cuts the link this phone may be using to reach the deck. */
    val cutsPhoneLink: Boolean get() = name in LINK_RAILS

    companion object {
        val LINK_RAILS = setOf("wifi", "bluetooth")

        /** The order the HUD's top bar uses. */
        val ORDER = listOf("gps", "lora", "sdr", "usb_hub", "bias_tee", "lte", "wifi", "bluetooth")

        fun label(name: String): String = when (name) {
            "gps" -> "GPS"
            "lora" -> "LoRa"
            "sdr" -> "SDR"
            "usb_hub" -> "USB hub"
            "bias_tee" -> "Bias tee"
            "lte" -> "4G LTE"
            "wifi" -> "Wi-Fi"
            "bluetooth" -> "Bluetooth"
            else -> name
        }
    }
}

@Serializable
data class RailRequest(val on: Boolean)

@Serializable
data class RailResponse(val ok: Boolean = false, val rail: String = "", val on: Boolean = false)

@Serializable
data class DeckBattery(
    val percent: Int = 0,
    @SerialName("voltage_v") val voltageV: Double = 0.0,
    @SerialName("current_a") val currentA: Double? = null,
    @SerialName("power_w") val powerW: Double? = null,
    val charging: Boolean = false,
    val status: String = "",
)

// ---------------------------------------------------------------- state_sync.deck

@Serializable
data class DeckSummary(
    /** `"up"` or `"down"`. */
    val harness: String = "",
    @SerialName("battery_w") val batteryW: Double? = null,
    val rails: List<Rail> = emptyList(),
    val summary: DeckSubsystems = DeckSubsystems(),
) {
    val harnessUp: Boolean get() = harness == "up"
}

@Serializable
data class DeckSubsystems(
    val sdr: SdrSummary? = null,
    val mesh: MeshSummary? = null,
    val gps: GpsSummary? = null,
)

@Serializable
data class SdrSummary(
    val status: String = "",
    @SerialName("center_hz") val centerHz: Long = 0,
    val synthetic: Boolean = false,
)

@Serializable
data class MeshSummary(val status: String = "", val nodes: Int = 0, val messages: Int = 0)

@Serializable
data class GpsSummary(
    val status: String = "",
    /** "3D", "2D", "none", or null before the first fix report. */
    val fix: String? = null,
    val used: Int = 0,
    @SerialName("pps_locked") val ppsLocked: Boolean = false,
)

/** `GET /v1/deck/status`. */
@Serializable
data class DeckStatus(
    val rails: List<Rail> = emptyList(),
    val battery: DeckBattery? = null,
    val net: JsonElement? = null,
    val harness: Boolean = false,
    val deck: DeckSubsystems = DeckSubsystems(),
    val mock: Boolean = false,
)

// ---------------------------------------------------------------- tools

/** `GET /v1/deck/tools`: the typed cyberdeck tools and their policy tier. */
@Serializable
data class DeckTool(
    val name: String,
    val description: String = "",
    val tier: Int = 0,
    @SerialName("input_schema") val inputSchema: JsonObject = JsonObject(emptyMap()),
)

@Serializable
data class DeckToolsResponse(val tools: List<DeckTool> = emptyList())

@Serializable
data class ToolContext(
    val source: String = SOURCE_ANDROID,
    @SerialName("chat_id") val chatId: String? = null,
) {
    companion object {
        const val SOURCE_ANDROID = "android"
    }
}

@Serializable
data class ToolInvokeRequest(
    val tool: String,
    val args: JsonObject = JsonObject(emptyMap()),
    val context: ToolContext = ToolContext(),
)

/**
 * `POST /v1/deck/tools/invoke`. Always 200: `ok` false with `decision` `policy_deny`, `deny`,
 * `timeout`, `invalid`, ... and `error` when the gate refused it or the tool failed.
 */
@Serializable
data class ToolInvokeResult(
    val ok: Boolean = false,
    val tier: Int? = null,
    val decision: String = "",
    val result: JsonElement? = null,
    val error: String? = null,
    val by: String? = null,
)

// ---------------------------------------------------------------- SDR

@Serializable
data class SdrSource(
    val kind: String = "",
    @SerialName("center_hz") val centerHz: Long = 0,
    @SerialName("sample_rate") val sampleRate: Long = 0,
    @SerialName("gain_db") val gainDb: Double = 0.0,
    val synthetic: Boolean = false,
)

@Serializable
data class SdrPeak(
    @SerialName("freq_hz") val freqHz: Double = 0.0,
    @SerialName("power_db") val powerDb: Double = 0.0,
    @SerialName("snr_db") val snrDb: Double = 0.0,
)

/** `GET /v1/deck/sdr`. */
@Serializable
data class SdrSnapshot(
    val status: String = "",
    val source: SdrSource = SdrSource(),
    val peaks: List<SdrPeak> = emptyList(),
    @SerialName("floor_db") val floorDb: Double = 0.0,
    @SerialName("ceil_db") val ceilDb: Double = 0.0,
    @SerialName("fft_size") val fftSize: Int = 0,
)

@Serializable
data class TuneRequest(
    @SerialName("center_hz") val centerHz: Long,
    @SerialName("gain_db") val gainDb: Double? = null,
)

@Serializable
data class TuneResponse(
    val ok: Boolean = false,
    @SerialName("center_hz") val centerHz: Long? = null,
    val error: String? = null,
)

/** `sdr_decode`: one packet from `rtl_433`, `multimon-ng`, ... as the decoder printed it. */
@Serializable
data class SdrDecode(
    val decoder: String = "",
    @SerialName("freq_hz") val freqHz: Long = 0,
    val packet: JsonElement? = null,
    val synthetic: Boolean = false,
)

@Serializable
internal data class SdrSpectrumWire(
    @SerialName("center_hz") val centerHz: Long = 0,
    @SerialName("sample_rate") val sampleRate: Long = 0,
    @SerialName("gain_db") val gainDb: Double = 0.0,
    val synthetic: Boolean = false,
    @SerialName("floor_db") val floorDb: Double = 0.0,
    @SerialName("ceil_db") val ceilDb: Double = 0.0,
    val bins: String = "",
    val peaks: List<SdrPeak> = emptyList(),
)

/** One waterfall row: [bins] are 0..255 levels between [floorDb] and [ceilDb]. */
class SdrSpectrum(
    val centerHz: Long,
    val sampleRate: Long,
    val synthetic: Boolean,
    val floorDb: Double,
    val ceilDb: Double,
    val bins: IntArray,
    val peaks: List<SdrPeak>,
) {
    companion object {
        internal fun from(w: SdrSpectrumWire): SdrSpectrum {
            val raw = runCatching { Base64.getDecoder().decode(w.bins) }.getOrDefault(ByteArray(0))
            return SdrSpectrum(
                centerHz = w.centerHz,
                sampleRate = w.sampleRate,
                synthetic = w.synthetic,
                floorDb = w.floorDb,
                ceilDb = w.ceilDb,
                bins = IntArray(raw.size) { raw[it].toInt() and 0xFF },
                peaks = w.peaks,
            )
        }
    }
}

// ---------------------------------------------------------------- mesh

@Serializable
data class MeshNode(
    val id: String = "",
    val num: Long = 0,
    @SerialName("long_name") val longName: String = "",
    @SerialName("short_name") val shortName: String = "",
    val snr: Double? = null,
    val rssi: Int? = null,
    val hops: Int? = null,
    @SerialName("last_heard") val lastHeard: Long = 0,
    val lat: Double? = null,
    val lon: Double? = null,
    @SerialName("alt_m") val altM: Int? = null,
) {
    val displayName: String get() = longName.ifBlank { shortName.ifBlank { id } }
}

@Serializable
data class MeshMessage(
    val id: Long = 0,
    val from: String = "",
    val to: String = "",
    val channel: Int = 0,
    val text: String = "",
    @SerialName("rx_time") val rxTime: Long = 0,
    val snr: Double? = null,
    val rssi: Int? = null,
    val hops: Int? = null,
    val outgoing: Boolean = false,
)

/** `GET /v1/deck/mesh`. */
@Serializable
data class MeshSnapshot(
    val status: String = "",
    @SerialName("my_node") val myNode: String? = null,
    val nodes: List<MeshNode> = emptyList(),
    val messages: List<MeshMessage> = emptyList(),
)

// ---------------------------------------------------------------- GPS

@Serializable
data class GpsFix(
    val mode: Int = 0,
    /** "3D", "2D" or "none". */
    val fix: String = "none",
    val lat: Double? = null,
    val lon: Double? = null,
    @SerialName("alt_m") val altM: Double? = null,
    @SerialName("speed_mps") val speedMps: Double? = null,
    @SerialName("track_deg") val trackDeg: Double? = null,
    val time: String? = null,
    @SerialName("eph_m") val ephM: Double? = null,
) {
    val hasFix: Boolean get() = mode >= 2 && lat != null && lon != null
}

@Serializable
data class GpsSatellite(
    val prn: Int? = null,
    val el: Double? = null,
    val az: Double? = null,
    val snr: Double? = null,
    val used: Boolean = false,
    val gnss: Int? = null,
)

/** `GET /v1/deck/gps`. */
@Serializable
data class GpsSnapshot(
    val status: String = "",
    val device: String = "",
    val fix: GpsFix? = null,
    val satellites: List<GpsSatellite> = emptyList(),
    val visible: Int = 0,
    val used: Int = 0,
    val hdop: Double? = null,
    @SerialName("pps_offset_ns") val ppsOffsetNs: Long? = null,
    @SerialName("pps_locked") val ppsLocked: Boolean = false,
)

// ---------------------------------------------------------------- audit

/** One line of the daemon's audit log (already redacted on the device). */
@Serializable
data class AuditEntry(
    val ts: UnixSeconds = 0,
    val event: String = "",
    val data: JsonElement? = null,
)

@Serializable
data class AuditResponse(val entries: List<AuditEntry> = emptyList())
