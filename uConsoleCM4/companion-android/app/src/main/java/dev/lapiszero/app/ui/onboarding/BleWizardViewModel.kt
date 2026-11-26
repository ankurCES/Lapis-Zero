package dev.lapiszero.app.ui.onboarding

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.ble.BleUuids
import dev.lapiszero.app.data.ble.BleCommandException
import dev.lapiszero.app.data.ble.BleLink
import dev.lapiszero.app.data.ble.BleLinkLostException
import dev.lapiszero.app.data.ble.BleProvisioner
import dev.lapiszero.app.data.ble.BleTimeoutException
import dev.lapiszero.app.data.ble.FoundDevice
import dev.lapiszero.app.data.ble.HandshakeException
import dev.lapiszero.app.data.ble.PinResult
import dev.lapiszero.app.data.ble.ProvisioningClient
import dev.lapiszero.app.data.ble.TailscaleProgress
import dev.lapiszero.app.data.ble.WifiNetwork
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.DeviceRepository
import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.launch

sealed interface BleStep {
    /** Waiting for permissions / Bluetooth; the screen drives this part. */
    data object Intro : BleStep
    data class Scanning(val devices: List<FoundDevice> = emptyList(), val error: String? = null) : BleStep
    data class Connecting(val deviceName: String, val notice: String? = null) : BleStep
    data class EnterPin(
        val deviceName: String,
        val attemptsLeft: Int,
        val message: String? = null,
        val verifying: Boolean = false,
    ) : BleStep

    data class Wifi(
        val networks: List<WifiNetwork> = emptyList(),
        val scanning: Boolean = false,
        val connectingTo: String? = null,
        val error: String? = null,
    ) : BleStep

    data class Tailscale(
        val waiting: Boolean = false,
        val authUrl: String? = null,
        val error: String? = null,
    ) : BleStep

    data class Secrets(val saving: Boolean = false, val error: String? = null) : BleStep
    /** "Securing connection…": waiting for the first HTTPS answer over the tailnet. */
    data class Verifying(
        val fqdn: String,
        val checking: Boolean = true,
        val attempt: Int = 1,
        val error: String? = null,
    ) : BleStep

    /** `verified` is false when the user left before the HTTPS check succeeded. */
    data class Done(val deviceName: String, val verified: Boolean = true) : BleStep
    data class Failed(val message: String) : BleStep
}

sealed interface BleWizardEvent {
    /** Open the Tailscale login page in a Custom Tab. */
    data class OpenUrl(val url: String) : BleWizardEvent
}

/**
 * Zero-cable onboarding (spec 4.1): scan -> connect -> PIN -> Wi-Fi -> Tailscale -> optional
 * API keys -> HTTPS check over the tailnet -> `finish_provisioning`.
 */
class BleWizardViewModel(
    private val ble: BleProvisioner,
    private val devices: DeviceRepository,
    private val phone: PhoneInfo,
    /** Outlives the ViewModel so the GATT link is closed even when the screen is left. */
    private val cleanupScope: CoroutineScope,
) : ViewModel() {
    private val _step = MutableStateFlow<BleStep>(BleStep.Intro)
    val step: StateFlow<BleStep> = _step.asStateFlow()

    private val _events = MutableSharedFlow<BleWizardEvent>(extraBufferCapacity = 4)
    val events: SharedFlow<BleWizardEvent> = _events.asSharedFlow()

    private var scanJob: Job? = null
    private var pumpJob: Job? = null
    private var tailscaleJob: Job? = null
    private var verifyJob: Job? = null
    private var link: BleLink? = null
    private var client: ProvisioningClient? = null
    private var target: FoundDevice? = null
    private var pendingId: String? = null
    private var token: String? = null
    private var fqdn: String? = null

    /** Set while a disconnect is part of the plan (lockout reconnect, `bye`). */
    private var expectDisconnect = false

    /**
     * Leaves the wizard (back, cancel, or after "Open workspace"): the GATT link is closed and
     * the next visit starts from the beginning. This ViewModel outlives the screen, so without
     * this a second pairing would open on the previous one's last step.
     */
    fun reset() {
        scanJob?.cancel()
        verifyJob?.cancel()
        expectDisconnect = true
        target = null
        pendingId = null
        token = null
        fqdn = null
        _step.value = BleStep.Intro
        cleanupScope.launch {
            closeLink()
            // An entry that never learned its Tailscale name is useless: drop it and its token.
            devices.purgeUnprovisioned()
        }
    }

    fun startScan() {
        scanJob?.cancel()
        _step.value = BleStep.Scanning()
        scanJob = viewModelScope.launch {
            ble.scan()
                .catch { e -> _step.value = BleStep.Scanning(error = e.message ?: "Bluetooth scan failed") }
                .collect { found ->
                    if (_step.value is BleStep.Scanning) _step.value = BleStep.Scanning(found)
                }
        }
    }

    fun select(device: FoundDevice) {
        scanJob?.cancel()
        target = device
        viewModelScope.launch { connectAndHandshake(null) }
    }

    private suspend fun connectAndHandshake(notice: String?) {
        val device = target ?: return
        _step.value = BleStep.Connecting(device.name, notice)
        try {
            closeLink()
            expectDisconnect = false
            val opened = ble.connect(device.device)
            link = opened
            val session = ProvisioningClient(opened)
            client = session
            pumpJob = viewModelScope.launch {
                session.pump()
                onLinkLost()
            }
            session.handshake()
            _step.value = BleStep.EnterPin(device.name, ProvisioningClient.MAX_PIN_FAILURES, notice)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            fail(e.message ?: "Could not start pairing")
        }
    }

    private fun onLinkLost() {
        if (expectDisconnect) return
        when (_step.value) {
            is BleStep.Done, is BleStep.Failed, is BleStep.Intro, is BleStep.Scanning -> Unit
            // After `ready` the device is reachable over Tailscale; BLE is no longer essential.
            is BleStep.Verifying, is BleStep.Secrets -> Unit
            else -> fail(
                "The Bluetooth connection was lost. Keep the phone next to the device and start again " +
                    "(on the device: Menu -> Pair Android App).",
            )
        }
    }

    fun submitPin(pin: String) {
        val current = _step.value as? BleStep.EnterPin ?: return
        val session = client ?: return
        if (current.verifying) return
        _step.value = current.copy(verifying = true, message = null)
        viewModelScope.launch {
            try {
                // A drop during the PIN check is reported by verifyPin itself (the device counts
                // failures across connections, so its third strike may be our first).
                expectDisconnect = true
                when (val result = session.verifyPin(pin, phone.name, phone.model)) {
                    is PinResult.Verified -> {
                        expectDisconnect = false
                        token = result.apiToken
                        // Spec 4: the token is in the Keystore vault before any other command.
                        pendingId = devices.add(
                            current.deviceName, null, result.apiToken, result.deviceId,
                            kind = BleUuids.kindOf(current.deviceName),
                        ).id
                        _step.value = BleStep.Wifi(scanning = true)
                        scanWifi()
                    }

                    is PinResult.Rejected -> {
                        expectDisconnect = false
                        _step.value = current.copy(
                            attemptsLeft = result.attemptsLeft,
                            verifying = false,
                            message = "That PIN was not accepted. Check the code on the device screen.",
                        )
                    }

                    PinResult.LockedOut -> {
                        expectDisconnect = true
                        _step.value = BleStep.Connecting(
                            current.deviceName,
                            "The device closed the connection. After three wrong PINs it locks for a few " +
                                "seconds and shows a new PIN. Reconnecting…",
                        )
                        closeLink()
                        delay(ProvisioningClient.LOCKOUT_COOLDOWN_MS + 1_500)
                        connectAndHandshake("Enter the PIN that is on the device screen now.")
                    }
                }
            } catch (e: HandshakeException) {
                fail("Pairing was aborted: ${e.message}")
            } catch (e: BleLinkLostException) {
                fail("The Bluetooth connection was lost while checking the PIN.")
            } catch (e: IOException) {
                fail(e.message ?: "The token could not be stored securely on this phone.")
            }
        }
    }

    fun scanWifi() {
        val session = client ?: return
        _step.value = BleStep.Wifi(scanning = true)
        viewModelScope.launch {
            _step.value = try {
                BleStep.Wifi(networks = session.wifiScan())
            } catch (e: BleLinkLostException) {
                return@launch
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                BleStep.Wifi(error = describe(e, "The device could not scan for Wi-Fi networks"))
            }
        }
    }

    fun connectWifi(ssid: String, psk: String?) {
        val session = client ?: return
        val current = _step.value as? BleStep.Wifi ?: return
        if (current.connectingTo != null) return
        _step.value = current.copy(connectingTo = ssid, error = null)
        viewModelScope.launch {
            _step.value = try {
                val status = session.wifiConnect(ssid, psk)
                if (status.state == "connected") {
                    BleStep.Tailscale()
                } else {
                    current.copy(connectingTo = null, error = "Could not join $ssid. Check the password.")
                }
            } catch (e: BleLinkLostException) {
                return@launch
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                current.copy(connectingTo = null, error = describe(e, "Could not join $ssid"))
            }
        }
    }

    /** The device is already online (re-pairing, Ethernet gadget, seeded Wi-Fi). */
    fun skipWifi() {
        if (_step.value is BleStep.Wifi) _step.value = BleStep.Tailscale()
    }

    /** `authKey == null`: interactive login, the URL arrives over BLE and opens in a Custom Tab. */
    fun tailscaleUp(authKey: String?) {
        val session = client ?: return
        tailscaleJob?.cancel()
        _step.value = BleStep.Tailscale(waiting = true)
        tailscaleJob = viewModelScope.launch {
            try {
                session.tailscaleUp(authKey).collect { progress ->
                    when (progress) {
                        is TailscaleProgress.AuthUrl -> {
                            _step.value = BleStep.Tailscale(waiting = true, authUrl = progress.url)
                            if (progress.url.startsWith("https://")) {
                                _events.emit(BleWizardEvent.OpenUrl(progress.url))
                            }
                        }

                        is TailscaleProgress.Ready -> onTailscaleReady(progress)
                    }
                }
            } catch (e: BleLinkLostException) {
                return@launch
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                _step.value = BleStep.Tailscale(error = describe(e, "Tailscale did not come up"))
            }
        }
    }

    private fun onTailscaleReady(ready: TailscaleProgress.Ready) {
        val name = ready.fqdn.trim().trimEnd('.')
        val id = pendingId
        if (!Endpoint.isValidHost(name) || name.contains(':') || id == null) {
            _step.value = BleStep.Tailscale(
                error = "Tailscale is connected but the device has no HTTPS name. Enable MagicDNS and " +
                    "HTTPS certificates for your tailnet, then try again.",
            )
            return
        }
        fqdn = name
        try {
            devices.completeProvisioning(id, Endpoint(name, tls = true), name.substringBefore('.'))
        } catch (e: IOException) {
            fail(e.message ?: "The device could not be stored securely on this phone.")
            return
        }
        _step.value = BleStep.Secrets()
    }

    /** Optional: seed API keys over the encrypted BLE channel. Blank values are skipped. */
    fun pushSecrets(values: Map<String, String>) {
        val session = client ?: return
        val secrets = values.mapValues { it.value.trim() }.filterValues { it.isNotEmpty() }
        if (secrets.isEmpty()) {
            verify()
            return
        }
        _step.value = BleStep.Secrets(saving = true)
        viewModelScope.launch {
            try {
                session.pushSecrets(secrets)
                verify()
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                _step.value = BleStep.Secrets(error = describe(e, "The keys were not saved"))
            }
        }
    }

    /**
     * HTTPS health check over the tailnet with the new token, and only after it succeeds
     * `finish_provisioning`.
     *
     * The first HTTPS request makes Tailscale issue the device's TLS certificate, which takes
     * 10-60 s and can fail transiently, so the check is retried with backoff for about 90 s.
     * On failure nothing is sent to the device: the BLE session stays open for another try.
     */
    fun verify() {
        val name = fqdn ?: return
        val bearer = token ?: return
        verifyJob?.cancel()
        _step.value = BleStep.Verifying(name)
        verifyJob = viewModelScope.launch {
            val api = LapisApiClient(Endpoint(name, tls = true), { bearer })
            val startedAt = System.nanoTime()
            fun elapsedMs() = (System.nanoTime() - startedAt) / 1_000_000
            var wait = VERIFY_FIRST_DELAY_MS
            var attempt = 0
            var last: IOException
            while (true) {
                attempt += 1
                _step.value = BleStep.Verifying(name, checking = true, attempt = attempt)
                try {
                    api.health()
                    finishAndComplete()
                    return@launch
                } catch (e: LapisApiException) {
                    last = e
                    if (e.isUnauthorized) break // retrying cannot fix a rejected token
                } catch (e: IOException) {
                    last = e
                }
                if (elapsedMs() + wait > VERIFY_BUDGET_MS) break
                delay(wait)
                wait = (wait * 3 / 2).coerceAtMost(VERIFY_MAX_DELAY_MS)
            }
            val detail = ChatRepository.describe(last)
            _step.value = BleStep.Verifying(
                name,
                checking = false,
                attempt = attempt,
                error = if ((last as? LapisApiException)?.isUnauthorized == true) {
                    "The device rejected the new token. Start pairing again."
                } else {
                    "Could not reach https://$name ($detail). The two usual causes: Tailscale VPN is " +
                        "off on this phone, or HTTPS certificates / MagicDNS are not enabled for " +
                        "your tailnet. The Bluetooth session is still open: fix it and retry."
                },
            )
        }
    }

    /**
     * Leaves the wizard after a failed check WITHOUT `finish_provisioning`: the device keeps its
     * pairing window until its own timeout, and the stored device works once HTTPS does.
     */
    fun exitWithoutFinishing() {
        verifyJob?.cancel()
        expectDisconnect = true
        viewModelScope.launch {
            closeLink()
            _step.value = BleStep.Done(fqdn ?: target?.name.orEmpty(), verified = false)
        }
    }

    /** Only ever called after a successful HTTPS health check. */
    private suspend fun finishAndComplete() {
        expectDisconnect = true
        try {
            client?.finish()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // The link already dropped; the device closes the window by itself after its timeout.
        }
        closeLink()
        _step.value = BleStep.Done(fqdn ?: target?.name.orEmpty(), verified = true)
    }

    private fun describe(e: Exception, fallback: String): String = when (e) {
        is BleCommandException -> "$fallback: ${e.message}"
        is BleTimeoutException -> "$fallback: the device did not answer in time"
        else -> fallback
    }

    private fun fail(message: String) {
        _step.value = BleStep.Failed(message)
        // "Start over" begins with a new handshake and a new token; forget the half-made entry.
        pendingId = null
        token = null
        cleanupScope.launch {
            closeLink()
            devices.purgeUnprovisioned()
        }
    }

    private suspend fun closeLink() {
        pumpJob?.cancel()
        pumpJob = null
        tailscaleJob?.cancel()
        tailscaleJob = null
        verifyJob = null
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
        // An entry that never got its Tailscale name is useless: drop it (and its token).
        devices.purgeUnprovisioned()
    }

    private companion object {
        const val VERIFY_BUDGET_MS = 90_000L
        const val VERIFY_FIRST_DELAY_MS = 2_000L
        const val VERIFY_MAX_DELAY_MS = 10_000L
    }
}
