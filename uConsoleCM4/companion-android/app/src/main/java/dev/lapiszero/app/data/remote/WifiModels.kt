package dev.lapiszero.app.data.remote

import dev.lapiszero.app.data.ble.WifiNetwork
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/*
 * `/v1/wifi*` (both daemons, identical JSON). Scan rows are the BLE `wifi_list` rows
 * ([WifiNetwork]). The pre-shared key only ever travels in the connect request body; no answer
 * carries it.
 */

@Serializable
data class WifiSaved(val name: String, val active: Boolean = false)

/** The phone Wi-Fi window: the device listens for paired phones over Bluetooth until `expires_at`. */
@Serializable
data class WifiBleWindow(
    val open: Boolean = false,
    /** Epoch seconds; 0 when closed. */
    @SerialName("expires_at") val expiresAt: Long = 0,
)

/** `GET /v1/wifi` (also the answer of forget and of opening the Bluetooth window). */
@Serializable
data class WifiInfo(
    val ssid: String = "",
    val ip: String = "",
    val rssi: Int? = null,
    /** NetworkManager Wi-Fi profiles; `active` is the one in use. */
    val saved: List<WifiSaved> = emptyList(),
    val ble: WifiBleWindow = WifiBleWindow(),
)

@Serializable
data class WifiScanResponse(val networks: List<WifiNetwork> = emptyList())

@Serializable
data class WifiConnectResult(
    /** "connected" or "failed". */
    val state: String = "",
    @SerialName("local_ip") val localIp: String = "",
)

@Serializable
data class WifiForgetRequest(val name: String)

object WifiRules {
    /** What the daemons accept (422 otherwise): 8..63 printable ASCII characters, or 64 hex digits. */
    fun isValidPsk(psk: String): Boolean =
        (psk.length in 8..63 && psk.all { it in ' '..'~' }) ||
            (psk.length == 64 && psk.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' })

    /** At most 32 bytes, as the daemons check. */
    fun isValidSsid(ssid: String): Boolean = ssid.isNotEmpty() && ssid.toByteArray(Charsets.UTF_8).size <= 32
}
