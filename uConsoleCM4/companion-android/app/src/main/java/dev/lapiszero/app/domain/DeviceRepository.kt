package dev.lapiszero.app.domain

import dev.lapiszero.app.data.local.SecureStore
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.LapisJson
import java.util.UUID
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * One paired CardputerZero, Lapis-Deck (or development daemon). Persisted only inside [SecureStore].
 *
 * `host` is empty while BLE provisioning has proven the PIN (and so already owns a token) but
 * the device has not joined Tailscale yet.
 */
@Serializable
data class PairedDevice(
    val id: String,
    val name: String,
    val host: String,
    val tls: Boolean,
    val token: String,
    @SerialName("remote_id") val remoteId: String = "",
    @SerialName("added_at") val addedAt: Long = 0,
    /** Whether this phone's timezone was already offered to the device after pairing. */
    @SerialName("timezone_synced") val timezoneSynced: Boolean = false,
    /**
     * [DeviceKind.DECK] or [DeviceKind.ZERO] once known (advertised BLE name, `/v1/health`, then
     * every `state_sync`); "" for a pairing stored by an app version from before the Deck.
     */
    val kind: String = "",
    /**
     * The name the device advertises over Bluetooth (`LapisZero-XXXX` / `LapisDeck-XXXX`) when this
     * phone paired with it over BLE or changed its Wi-Fi that way; "" when unknown (QR or dev
     * pairing, or a vault from before this field). Used to preselect it in a Bluetooth scan.
     */
    @SerialName("ble_name") val bleName: String = "",
) {
    val isProvisioned: Boolean get() = host.isNotEmpty()
    val isDeck: Boolean get() = DeviceKind.isDeck(kind)
    val endpoint: Endpoint get() = Endpoint(host, tls)

    // A data class toString would print the bearer token; this one must be safe to log.
    override fun toString(): String = "PairedDevice(id=$id, name=$name, host=$host, tls=$tls)"
}

@Serializable
private data class Vault(
    val devices: List<PairedDevice> = emptyList(),
    @SerialName("active_id") val activeId: String? = null,
)

data class DevicesState(
    val devices: List<PairedDevice> = emptyList(),
    val activeId: String? = null,
) {
    /** Devices that can be talked to (provisioning finished). */
    val usable: List<PairedDevice> get() = devices.filter { it.isProvisioned }
    val active: PairedDevice? get() = usable.firstOrNull { it.id == activeId } ?: usable.firstOrNull()
}

/** Multi-device switcher: more than one CardputerZero or Lapis-Deck can be paired at once. */
class DeviceRepository(private val store: SecureStore) {
    private val lock = Any()
    private val _state = MutableStateFlow(load())
    val state: StateFlow<DevicesState> = _state.asStateFlow()

    private fun load(): DevicesState {
        val vault = store.read()
            ?.let { runCatching { LapisJson.decodeFromString<Vault>(it) }.getOrNull() }
            ?: Vault()
        return DevicesState(vault.devices, vault.activeId)
    }

    private fun update(transform: (DevicesState) -> DevicesState): DevicesState = synchronized(lock) {
        val current = _state.value
        val next = transform(current)
        if (next == current) return@synchronized current // no Keystore round trip for a no-op
        if (next.devices.isEmpty()) {
            store.clear()
        } else {
            store.write(LapisJson.encodeToString(Vault(next.devices, next.activeId)))
        }
        _state.value = next
        next
    }

    fun find(id: String): PairedDevice? = _state.value.devices.firstOrNull { it.id == id }

    /**
     * Adds a device, or replaces the entry for the same daemon identity (same host and the same
     * remote device id), and makes it the active one when it is usable.
     */
    fun add(
        name: String,
        endpoint: Endpoint?,
        token: String,
        remoteId: String,
        nowSec: Long = System.currentTimeMillis() / 1000,
        kind: String = "",
        bleName: String = "",
    ): PairedDevice {
        require(token.isNotEmpty()) { "token must not be empty" }
        val device = PairedDevice(
            id = UUID.randomUUID().toString(),
            name = name.ifBlank { endpoint?.host ?: "CardputerZero" },
            host = endpoint?.host.orEmpty(),
            tls = endpoint?.tls ?: true,
            token = token,
            remoteId = remoteId,
            addedAt = nowSec,
            kind = kind,
            bleName = bleName,
        )
        update { s ->
            val others = s.devices.filterNot {
                device.isProvisioned && it.host == device.host && it.tls == device.tls
            }
            s.copy(
                devices = others + device,
                activeId = if (device.isProvisioned) device.id else s.activeId,
            )
        }
        return device
    }

    /** BLE provisioning learned the Tailscale name: the pending entry becomes usable. */
    fun completeProvisioning(id: String, endpoint: Endpoint, name: String? = null): PairedDevice? {
        var result: PairedDevice? = null
        update { s ->
            val current = s.devices.firstOrNull { it.id == id } ?: return@update s
            val done = current.copy(
                host = endpoint.host,
                tls = endpoint.tls,
                name = name?.takeIf { it.isNotBlank() } ?: current.name,
            )
            result = done
            s.copy(
                devices = s.devices.filterNot {
                    it.id == id || (it.host == done.host && it.tls == done.tls)
                } + done,
                activeId = done.id,
            )
        }
        return result
    }

    fun rename(id: String, name: String) {
        if (name.isBlank()) return
        update { s -> s.copy(devices = s.devices.map { if (it.id == id) it.copy(name = name.trim()) else it }) }
    }

    /** What the device said it is in its latest snapshot; written only when it changed. */
    fun setKind(id: String, kind: String) {
        if (kind.isEmpty() || find(id)?.kind == kind || find(id) == null) return
        update { s -> s.copy(devices = s.devices.map { if (it.id == id) it.copy(kind = kind) else it }) }
    }

    /** The advertised Bluetooth name, learned from a session that proved it is this device. */
    fun setBleName(id: String, bleName: String) {
        val current = find(id) ?: return
        if (bleName.isEmpty() || current.bleName == bleName) return
        update { s -> s.copy(devices = s.devices.map { if (it.id == id) it.copy(bleName = bleName) else it }) }
    }

    /** The one-time timezone hand-over after pairing is done for this device. */
    fun markTimezoneSynced(id: String) {
        if (find(id)?.timezoneSynced != false) return
        update { s -> s.copy(devices = s.devices.map { if (it.id == id) it.copy(timezoneSynced = true) else it }) }
    }

    fun setActive(id: String) {
        update { s -> if (s.usable.any { it.id == id }) s.copy(activeId = id) else s }
    }

    fun remove(id: String) {
        update { s ->
            val rest = s.devices.filterNot { it.id == id }
            s.copy(
                devices = rest,
                activeId = if (s.activeId == id) rest.firstOrNull { it.isProvisioned }?.id else s.activeId,
            )
        }
    }

    /** Drops entries left behind by an abandoned BLE wizard; their token is useless without a host. */
    fun purgeUnprovisioned(except: String? = null) {
        update { s -> s.copy(devices = s.devices.filter { it.isProvisioned || it.id == except }) }
    }
}
