package dev.lapiszero.app.ui.device

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.ble.BleAccess
import dev.lapiszero.app.data.ble.BleCommandException
import dev.lapiszero.app.data.ble.BleConnection
import dev.lapiszero.app.data.ble.BleLinkLostException
import dev.lapiszero.app.data.ble.BleTimeoutException
import dev.lapiszero.app.data.ble.FoundDevice
import dev.lapiszero.app.data.ble.HandshakeException
import dev.lapiszero.app.data.ble.ProvisioningClient
import dev.lapiszero.app.data.ble.TokenResult
import dev.lapiszero.app.data.ble.WifiNetwork
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.WifiInfo
import dev.lapiszero.app.data.remote.WifiRules
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.DeviceRepository
import dev.lapiszero.app.domain.PairedDevice
import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** How the phone talks to the device about Wi-Fi: the REST API (over Tailscale) or Bluetooth. */
enum class WifiVia { API, BLE }

/**
 * Screen states of "Change Wi-Fi" for an already-paired device. None of them ever holds a Wi-Fi
 * password: it goes from the text field straight into the request.
 */
sealed interface WifiStep {
    data object Loading : WifiStep

    /** The networks the device sees, and (over the API) the one it is on and the saved ones. */
    data class Networks(
        val via: WifiVia,
        /** API only: the device's current Wi-Fi and saved profiles. */
        val info: WifiInfo? = null,
        /** BLE only: the advertised name of the device this phone is connected to. */
        val bleName: String = "",
        val networks: List<WifiNetwork> = emptyList(),
        val scanning: Boolean = false,
        val connectingTo: String? = null,
        val forgetting: String? = null,
        val error: String? = null,
    ) : WifiStep

    /** The API did not answer (or the device software has no Wi-Fi API): Bluetooth is the way. */
    data class Offline(val reason: String) : WifiStep

    /** Looking for the device over Bluetooth; `remembered` is its advertised name when known. */
    data class BleSearch(
        val devices: List<FoundDevice> = emptyList(),
        val remembered: String = "",
        val scanning: Boolean = false,
        val error: String? = null,
    ) : WifiStep

    data class BleConnecting(val name: String) : WifiStep

    /** The connect request lost its answer: the device is moving to the new network. */
    data class Switching(val ssid: String) : WifiStep

    data class Joined(val ssid: String, val ip: String, val via: WifiVia) : WifiStep

    data class Failed(val message: String) : WifiStep
}

/**
 * Changes the Wi-Fi of one paired device without pairing again: over the API while the device
 * answers, otherwise over Bluetooth in paired-phone mode (protocol section 4.2), proving this
 * phone with the token of THAT device, never with any other.
 */
class WifiViewModel(
    private val device: PairedDevice,
    private val api: LapisApiClient,
    private val ble: BleAccess,
    private val devices: DeviceRepository,
    /** Outlives the ViewModel so the GATT link is closed even when the screen is left. */
    private val cleanupScope: CoroutineScope,
    private val tokenTimeoutMs: Long = ProvisioningClient.PIN_TIMEOUT_MS,
) : ViewModel() {
    private val _step = MutableStateFlow<WifiStep>(WifiStep.Loading)
    val step: StateFlow<WifiStep> = _step.asStateFlow()

    private var scanJob: Job? = null
    private var pumpJob: Job? = null
    private var link: BleConnection? = null
    private var client: ProvisioningClient? = null

    /** Set while a disconnect is part of the plan (leaving, `bye`). */
    private var expectDisconnect = false

    init {
        load()
    }

    /** Asks the device over the API; also "try the network again" from any Bluetooth step. */
    fun load() {
        scanJob?.cancel()
        _step.value = WifiStep.Loading
        viewModelScope.launch {
            closeLink()
            _step.value = try {
                WifiStep.Networks(WifiVia.API, info = api.wifi())
            } catch (e: LapisApiException) {
                WifiStep.Offline(
                    if (e.isNotFound) {
                        "Its software cannot change Wi-Fi over the network yet."
                    } else {
                        ChatRepository.describe(e)
                    },
                )
            } catch (e: IOException) {
                WifiStep.Offline(ChatRepository.describe(e))
            }
            if (_step.value is WifiStep.Networks) scan()
        }
    }

    fun scan() {
        val current = _step.value as? WifiStep.Networks ?: return
        if (current.connectingTo != null) return
        val via = current.via
        update(via) { it.copy(scanning = true, error = null) }
        viewModelScope.launch {
            try {
                val found = when (via) {
                    WifiVia.API -> api.wifiScan()
                    WifiVia.BLE -> (client ?: return@launch).wifiScan()
                }
                update(via) { it.copy(networks = found, scanning = false) }
            } catch (e: BleLinkLostException) {
                return@launch // onLinkLost says so
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val reason = when (e) {
                    is IOException -> "The device could not scan: ${ChatRepository.describe(e)}"
                    else -> describeBle(e, "The device could not scan for networks")
                }
                update(via) { it.copy(scanning = false, error = reason) }
            }
        }
    }

    /** `psk` is used once and dropped; null or empty for an open network. */
    fun connect(ssid: String, psk: String?) {
        val current = _step.value as? WifiStep.Networks ?: return
        if (current.connectingTo != null) return
        val key = psk?.takeIf { it.isNotEmpty() }
        if (!WifiRules.isValidSsid(ssid)) return
        if (key != null && !WifiRules.isValidPsk(key)) {
            _step.value = current.copy(error = PSK_RULE)
            return
        }
        _step.value = current.copy(connectingTo = ssid, error = null)
        viewModelScope.launch {
            when (current.via) {
                WifiVia.API -> connectOverApi(current, ssid, key)
                WifiVia.BLE -> connectOverBle(current, ssid, key)
            }
        }
    }

    private suspend fun connectOverApi(current: WifiStep.Networks, ssid: String, psk: String?) {
        _step.value = try {
            val result = api.wifiConnect(ssid, psk)
            if (result.state == "connected") {
                WifiStep.Joined(ssid, result.localIp, WifiVia.API)
            } else {
                current.copy(connectingTo = null, error = "Could not join $ssid. Check the password.")
            }
        } catch (e: LapisApiException) {
            // A proxy error without the daemon's own body: the device went away mid-request.
            if (e.status in 502..504 && e.code.startsWith("http_")) {
                WifiStep.Switching(ssid)
            } else {
                current.copy(connectingTo = null, error = "Could not join $ssid: ${ChatRepository.describe(e)}")
            }
        } catch (e: java.net.ConnectException) {
            current.copy(connectingTo = null, error = "Could not join $ssid: ${ChatRepository.describe(e)}")
        } catch (e: java.net.UnknownHostException) {
            current.copy(connectingTo = null, error = "Could not join $ssid: ${ChatRepository.describe(e)}")
        } catch (e: IOException) {
            // The request went out and the answer never came (timeout, dropped connection): the
            // device is leaving the network this phone reached it through. Not a failure.
            WifiStep.Switching(ssid)
        }
    }

    private suspend fun connectOverBle(current: WifiStep.Networks, ssid: String, psk: String?) {
        val session = client ?: return
        try {
            val status = session.wifiConnect(ssid, psk)
            if (status.state == "connected") {
                finishLink()
                _step.value = WifiStep.Joined(ssid, status.localIp, WifiVia.BLE)
            } else {
                update(WifiVia.BLE) { it.copy(connectingTo = null, error = "Could not join $ssid. Check the password.") }
            }
        } catch (e: BleLinkLostException) {
            return // onLinkLost says so
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            update(WifiVia.BLE) { it.copy(connectingTo = null, error = describeBle(e, "Could not join $ssid")) }
        }
    }

    /** API only: removes a saved profile. */
    fun forget(name: String) {
        val current = _step.value as? WifiStep.Networks ?: return
        if (current.via != WifiVia.API || current.forgetting != null || current.connectingTo != null) return
        _step.value = current.copy(forgetting = name, error = null)
        viewModelScope.launch {
            try {
                val info = api.wifiForget(name)
                update(WifiVia.API) { it.copy(info = info, forgetting = null) }
            } catch (e: IOException) {
                update(WifiVia.API) { it.copy(forgetting = null, error = "Could not forget $name: ${ChatRepository.describe(e)}") }
            }
        }
    }

    /**
     * Goes the Bluetooth way. A device that still answers is not listening for phones by itself
     * (it does so only once it has been offline for 30 s), so it is asked to open its window first.
     */
    fun useBluetooth() {
        val online = (_step.value as? WifiStep.Networks)?.via == WifiVia.API
        scanJob?.cancel()
        _step.value = WifiStep.BleSearch(remembered = device.bleName)
        viewModelScope.launch {
            closeLink()
            if (online) {
                try {
                    api.openWifiBle()
                } catch (e: IOException) {
                    // Best effort: the scan below says whether the device can be found.
                }
            }
        }
    }

    /** Called by the screen once Bluetooth is permitted and on. */
    fun startBleScan() {
        val current = _step.value as? WifiStep.BleSearch ?: return
        scanJob?.cancel()
        _step.value = current.copy(scanning = true, error = null)
        scanJob = viewModelScope.launch {
            ble.scan()
                .catch { e ->
                    _step.update { s ->
                        if (s is WifiStep.BleSearch) s.copy(scanning = false, error = e.message ?: "Bluetooth scan failed") else s
                    }
                }
                .collect { found ->
                    _step.update { s ->
                        // The remembered device first; the others stay pickable.
                        if (s is WifiStep.BleSearch) s.copy(devices = found.sortedByDescending { it.name == s.remembered }) else s
                    }
                }
        }
    }

    fun select(found: FoundDevice) {
        if (_step.value !is WifiStep.BleSearch) return
        scanJob?.cancel()
        _step.value = WifiStep.BleConnecting(found.name)
        viewModelScope.launch {
            try {
                closeLink()
                expectDisconnect = false
                val opened = ble.connect(found)
                link = opened
                val session = ProvisioningClient(opened, pinTimeoutMs = tokenTimeoutMs)
                client = session
                pumpJob = viewModelScope.launch {
                    session.pump()
                    onLinkLost()
                }
                session.handshake()
                // This device's token, and only it: a second guess would count as another failed attempt.
                when (session.verifyToken(device.token)) {
                    is TokenResult.Verified -> {
                        if (found.hasLapisName) {
                            try {
                                devices.setBleName(device.id, found.name)
                            } catch (e: IOException) {
                                // Only a convenience for the next time.
                            }
                        }
                        _step.value = WifiStep.Networks(WifiVia.BLE, bleName = found.name)
                        scan()
                    }

                    TokenResult.NotPaired -> fail(NOT_PAIRED)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: HandshakeException) {
                fail("${e.message}. Nothing was sent to it.")
            } catch (e: BleLinkLostException) {
                fail(LINK_LOST)
            } catch (e: Exception) {
                fail(e.message ?: "Could not connect over Bluetooth")
            }
        }
    }

    private fun onLinkLost() {
        if (expectDisconnect) return
        val step = _step.value
        if (step is WifiStep.BleConnecting || (step is WifiStep.Networks && step.via == WifiVia.BLE)) fail(LINK_LOST)
    }

    private inline fun update(via: WifiVia, crossinline change: (WifiStep.Networks) -> WifiStep.Networks) {
        _step.update { s -> if (s is WifiStep.Networks && s.via == via) change(s) else s }
    }

    private fun describeBle(e: Exception, fallback: String): String = when (e) {
        is BleCommandException -> "$fallback: ${e.message}"
        is BleTimeoutException -> "$fallback: the device did not answer in time"
        else -> fallback
    }

    private fun fail(message: String) {
        _step.value = WifiStep.Failed(message)
        cleanupScope.launch { closeLink() }
    }

    /**
     * `finish_provisioning`, only after the device joined: it ends the device's Bluetooth window,
     * and the device opens one by itself only once per offline episode.
     */
    private suspend fun finishLink() {
        expectDisconnect = true
        try {
            client?.finish()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // The link already dropped.
        }
        closeLink()
    }

    private suspend fun closeLink() {
        expectDisconnect = true
        pumpJob?.cancel()
        pumpJob = null
        client?.close()
        client = null
        val open = link
        link = null
        try {
            open?.close()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // Nothing left to close.
        }
    }

    override fun onCleared() {
        scanJob?.cancel()
        expectDisconnect = true
        val open = link
        val session = client
        link = null
        client = null
        cleanupScope.launch {
            session?.close()
            runCatching { open?.close() }
        }
    }

    companion object {
        const val NOT_PAIRED = "This device was not paired with this phone (or its pairing was revoked)."
        const val LINK_LOST = "The Bluetooth connection was lost. Keep the phone next to the device and try again."
        const val PSK_RULE = "A Wi-Fi password has 8 to 63 characters, or is 64 hexadecimal digits."

        /** When the device listens for this phone over Bluetooth by itself. */
        const val LISTENING = "The device listens for this phone over Bluetooth by itself once it has been " +
            "offline for 30 s (for 5 minutes), or while its pairing screen is open."
    }
}
