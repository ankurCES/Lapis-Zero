package dev.lapiszero.app.data.ble

import dev.lapiszero.app.data.remote.LapisJson
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement

/** GATT profile of `lapis-ble-v1` (BLE_PROVISIONING_PROTOCOL.md section 1). */
object BleUuids {
    val SERVICE: UUID = UUID.fromString("7c4f0001-9a3b-4c6e-8e21-51a0c0de0001")
    val HANDSHAKE: UUID = UUID.fromString("7c4f0001-9a3b-4c6e-8e21-51a0c0de0002")
    val CMD_TX: UUID = UUID.fromString("7c4f0001-9a3b-4c6e-8e21-51a0c0de0003")
    val EVT_RX: UUID = UUID.fromString("7c4f0001-9a3b-4c6e-8e21-51a0c0de0004")
    const val REQUESTED_MTU = 517
    const val NAME_PREFIX = "LapisZero-"
}

@Serializable
data class WifiNetwork(
    val ssid: String,
    val rssi: Int = -100,
    val secured: Boolean = true,
)

/** Decrypted device -> phone events, plus two local transport signals. */
sealed interface BleEvent {
    class PinVerified(val proofHex: String, val deviceId: String, val apiToken: String) : BleEvent {
        override fun toString(): String = "PinVerified(deviceId=$deviceId)"
    }

    data class WifiList(val networks: List<WifiNetwork>) : BleEvent
    data class WifiStatus(val state: String, val localIp: String) : BleEvent
    data class TailscaleAuth(val url: String) : BleEvent
    data class Ready(val tailscaleFqdn: String, val tailscaleIp: String) : BleEvent
    data class SecretsSaved(val keys: List<String>) : BleEvent
    data object Bye : BleEvent

    /** `{"evt":"error","cmd":"<command name>","message":"<text>"}`; the session stays usable. */
    data class Error(val cmd: String, val message: String) : BleEvent
    data class Unknown(val evt: String) : BleEvent

    /** The device sent a reset frame (`flags = 0x80`): it dropped what we were sending. */
    data object TransportReset : BleEvent

    /** The GATT link is gone. */
    data object Disconnected : BleEvent

    companion object {
        fun parse(plaintext: ByteArray): BleEvent? {
            val obj = runCatching {
                LapisJson.parseToJsonElement(plaintext.toString(Charsets.UTF_8))
            }.getOrNull() as? JsonObject ?: return null
            fun str(key: String): String = (obj[key] as? JsonPrimitive)?.contentOrNull.orEmpty()
            return when (val evt = str("evt")) {
                "pin_verified" -> PinVerified(str("proof"), str("device_id"), str("api_token"))
                "wifi_list" -> WifiList(
                    (obj["networks"] as? JsonArray).orEmpty().mapNotNull {
                        runCatching { LapisJson.decodeFromJsonElement<WifiNetwork>(it) }.getOrNull()
                    },
                )
                "wifi_status" -> WifiStatus(str("state"), str("local_ip"))
                "tailscale_auth" -> TailscaleAuth(str("url"))
                "ready" -> Ready(str("tailscale_fqdn"), str("tailscale_ip"))
                "secrets_saved" -> SecretsSaved(
                    (obj["keys"] as? JsonArray).orEmpty()
                        .mapNotNull { (it as? JsonPrimitive)?.contentOrNull },
                )
                "bye" -> Bye
                "error" -> Error(str("cmd"), str("message"))
                "" -> null
                else -> Unknown(evt)
            }
        }
    }
}

internal fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it.toInt() and 0xFF) }

internal fun String.hexToBytesOrNull(): ByteArray? {
    if (length % 2 != 0) return null
    val out = ByteArray(length / 2)
    for (i in out.indices) {
        val hi = Character.digit(this[2 * i], 16)
        val lo = Character.digit(this[2 * i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}
