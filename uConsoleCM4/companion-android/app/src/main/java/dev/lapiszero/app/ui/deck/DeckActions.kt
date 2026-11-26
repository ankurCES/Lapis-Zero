package dev.lapiszero.app.ui.deck

import dev.lapiszero.app.data.remote.MeshMessage
import dev.lapiszero.app.domain.DeckAlert
import java.util.Locale
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put

/** A one-tap cyberdeck tool call on the Deck screen. */
data class QuickAction(val key: String, val label: String, val tool: String, val args: JsonObject)

object QuickActions {
    const val MESH_SEND = "mesh.send_message"

    val ISM = QuickAction(
        "ism", "ISM decode 433.92", "sdr.decode_ism",
        buildJsonObject {
            put("freq_hz", 433_920_000L)
            put("duration_s", 10)
        },
    )
    val ADSB = QuickAction("adsb", "ADS-B aircraft", "sdr.adsb_snapshot", JsonObject(emptyMap()))
    val WIFI = QuickAction("wifi", "Passive Wi-Fi survey", "net.wifi_scan_passive", JsonObject(emptyMap()))
    val PEERS = QuickAction("peers", "Tailscale peers", "net.tailscale_peers", JsonObject(emptyMap()))

    val SDR = listOf(ISM, ADSB)
    val NETOPS = listOf(WIFI, PEERS)

    /** The daemon's tiers, used until `GET /v1/deck/tools` has answered. */
    val DEFAULT_TIERS = mapOf(
        ISM.tool to 0, ADSB.tool to 0, WIFI.tool to 0, PEERS.tool to 0, MESH_SEND to 1,
    )
}

/** Receive-only retunes of the live waterfall (no gate). */
data class TunePreset(val label: String, val hz: Long)

val TUNE_PRESETS = listOf(
    TunePreset("FM 100.0", 100_000_000),
    TunePreset("Air 121.5", 121_500_000),
    TunePreset("APRS 144.39", 144_390_000),
    TunePreset("NOAA 162.40", 162_400_000),
    TunePreset("ISM 433.92", 433_920_000),
    TunePreset("ISM 915", 915_000_000),
    TunePreset("ADS-B 1090", 1_090_000_000),
)

/** Client-side checks that match the daemon's own (one LoRa packet, channels 0..7). */
object MeshRules {
    const val MAX_BYTES = 228
    const val MAX_CHANNEL = 7

    fun bytes(text: String): Int = text.trim().toByteArray(Charsets.UTF_8).size

    /** Null when the message can be sent, otherwise why not. */
    fun problem(channel: Int, text: String): String? = when {
        text.isBlank() -> "Nothing to send"
        bytes(text) > MAX_BYTES -> "Too long for one LoRa packet (${bytes(text)} / $MAX_BYTES bytes)"
        channel !in 0..MAX_CHANNEL -> "Channel must be 0 to $MAX_CHANNEL"
        else -> null
    }

    fun args(channel: Int, text: String): JsonObject = buildJsonObject {
        put("channel", channel)
        put("text", text.trim())
    }

    /**
     * The REST snapshot plus what arrived on the socket since, without duplicates, oldest first.
     * Messages have ids; one without (0) is matched on sender, time and text.
     */
    fun merge(snapshot: List<MeshMessage>, live: List<MeshMessage>): List<MeshMessage> =
        (snapshot + live)
            .distinctBy { if (it.id != 0L) "#${it.id}" else "${it.from}|${it.rxTime}|${it.text}" }
            .sortedWith(compareBy<MeshMessage> { it.rxTime }.thenBy { it.id })
}

/** One line of a tool's result, as the Deck screen lists it. */
data class ResultRow(val title: String, val detail: String, val dim: Boolean = false)

/** Readable rows for the quick actions' results; anything else falls back to its scalar fields. */
object ToolResults {
    private fun JsonElement?.obj(): JsonObject? = this as? JsonObject
    private fun JsonElement?.arr(): List<JsonElement> = (this as? JsonArray).orEmpty()
    private fun JsonObject.str(key: String): String? =
        (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull?.takeIf { it.isNotBlank() }

    fun synthetic(result: JsonElement?): Boolean =
        (result.obj()?.get("synthetic") as? JsonPrimitive)?.booleanOrNull == true ||
            (result.obj()?.get("mock") as? JsonPrimitive)?.booleanOrNull == true

    fun rows(tool: String, result: JsonElement?): List<ResultRow> {
        val o = result.obj() ?: return listOfNotNull(result?.let { ResultRow(DeckAlert.describePacket(it), "") })
        return when (tool) {
            "net.wifi_scan_passive" -> o["networks"].arr().mapNotNull { it.obj() }.map { n ->
                ResultRow(
                    n.str("ssid") ?: "(hidden)",
                    listOfNotNull(
                        n.str("channel")?.let { "ch $it" },
                        n.str("band"),
                        n.str("rssi_dbm")?.let { "$it dBm" },
                        n.str("security"),
                    ).joinToString(" · "),
                )
            }

            "net.tailscale_peers" -> o["peers"].arr().mapNotNull { it.obj() }.map { p ->
                val online = (p["online"] as? JsonPrimitive)?.booleanOrNull == true
                ResultRow(
                    p.str("hostname") ?: p.str("dns_name") ?: "?",
                    listOfNotNull(p.str("ip"), p.str("os"), if (online) "online" else "offline").joinToString(" · "),
                    dim = !online,
                )
            }

            "sdr.decode_ism" -> o["packets"].arr().map { p ->
                ResultRow(p.obj()?.str("model") ?: "packet", DeckAlert.describePacket(p))
            }

            "sdr.decode_pager" -> o["messages"].arr().map { ResultRow("page", DeckAlert.describePacket(it)) }

            "sdr.adsb_snapshot" -> (o["aircraft"] ?: o["table"].obj()?.get("aircraft")).arr()
                .mapNotNull { it.obj() }
                .map { a ->
                    ResultRow(
                        a.str("flight") ?: a.str("hex") ?: "?",
                        listOfNotNull(
                            a.str("hex")?.takeIf { a.str("flight") != null },
                            a.str("alt_baro")?.let { if (it == "ground") "on ground" else "$it ft" },
                            a.str("gs")?.let { "$it kt" },
                            a.str("squawk")?.let { "sq $it" },
                        ).joinToString(" · "),
                    )
                }

            else -> listOf(ResultRow("result", DeckAlert.describePacket(o, max = 12)))
        }
    }

    /** "7 networks", "no packets", ... for the line above the rows. */
    fun headline(tool: String, rows: Int): String {
        val noun = when (tool) {
            "net.wifi_scan_passive" -> "network"
            "net.tailscale_peers" -> "peer"
            "sdr.decode_ism" -> "packet"
            "sdr.decode_pager" -> "page"
            "sdr.adsb_snapshot" -> "aircraft"
            else -> return "done"
        }
        val plural = if (rows == 1 || noun == "aircraft") noun else noun + "s"
        return if (rows == 0) "no $plural" else "$rows $plural"
    }
}

/** "2.31 W", "n/a". */
fun formatWatts(w: Double?): String = w?.let { String.format(Locale.US, "%.2f W", it) } ?: "n/a"

/** "37.77490, -122.41940". */
fun formatLatLon(lat: Double?, lon: Double?): String =
    if (lat == null || lon == null) "n/a" else String.format(Locale.US, "%.5f, %.5f", lat, lon)

/** What the gate said, for a snackbar: "denied by android:Pixel", "timed out", ... */
fun describeDecision(decision: String, by: String?): String = when (decision) {
    "deny" -> "denied" + (by?.takeIf { it.isNotBlank() }?.let { " by $it" }.orEmpty())
    "timeout" -> "not decided in time"
    "policy_deny" -> "refused by policy"
    "kill_switch" -> "stopped by the kill switch"
    "invalid" -> "rejected as invalid"
    "" -> "failed"
    else -> decision.replace('_', ' ')
}
