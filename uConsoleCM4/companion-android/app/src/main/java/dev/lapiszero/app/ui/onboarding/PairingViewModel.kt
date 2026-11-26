package dev.lapiszero.app.ui.onboarding

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.Health
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.PairExchangeResponse
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.DeviceRepository
import dev.lapiszero.app.domain.PairingPayload
import java.io.IOException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** How this phone introduces itself to the device (`device_name` / `device_model`). */
data class PhoneInfo(val name: String, val model: String)

sealed interface PairUi {
    data object Idle : PairUi

    /** A valid code was scanned; the user compares the PIN with the LCD before pairing. */
    data class Confirm(val payload: PairingPayload) : PairUi
    data class Working(val label: String) : PairUi
    data class Failed(val message: String) : PairUi
    data class Done(val deviceName: String) : PairUi
}

/** QR pairing (`POST /v1/pair/exchange`) and the manual "Dev connect" path. */
class PairingViewModel(
    private val devices: DeviceRepository,
    private val phone: PhoneInfo,
    private val exchange: suspend (Endpoint, String, PhoneInfo) -> PairExchangeResponse =
        { endpoint, nonce, info -> LapisApiClient.pairExchange(endpoint, nonce, info.name, info.model) },
    private val health: suspend (Endpoint, String) -> Health =
        { endpoint, token -> LapisApiClient(endpoint, { token }).health() },
) : ViewModel() {
    private val _ui = MutableStateFlow<PairUi>(PairUi.Idle)
    val ui: StateFlow<PairUi> = _ui.asStateFlow()

    fun reset() {
        _ui.value = PairUi.Idle
    }

    /** Called for every decoded QR code (and for a pasted payload). Returns true if accepted. */
    fun onScanned(raw: String): Boolean {
        if (_ui.value !is PairUi.Idle && _ui.value !is PairUi.Failed) return false
        return when (val parsed = PairingPayload.parse(raw)) {
            is PairingPayload.ParseResult.Ok -> {
                _ui.value = PairUi.Confirm(parsed.payload)
                true
            }

            is PairingPayload.ParseResult.Invalid -> {
                _ui.value = PairUi.Failed(parsed.reason)
                false
            }
        }
    }

    fun confirm() {
        val payload = (_ui.value as? PairUi.Confirm)?.payload ?: return
        _ui.value = PairUi.Working("Exchanging the pairing code…")
        viewModelScope.launch {
            _ui.value = try {
                val response = exchange(payload.endpoint, payload.nonce, phone)
                val name = response.hostname.ifBlank { payload.host }
                // The token goes straight into the Keystore-backed vault and nowhere else.
                devices.add(name, payload.endpoint, response.apiToken, response.deviceId)
                PairUi.Done(name)
            } catch (e: LapisApiException) {
                PairUi.Failed(
                    when (e.code) {
                        "bad_nonce" -> "This code has expired or was already used. Show a new QR code on the device."
                        "locked_out" -> "Too many failed attempts. Wait a minute and try again."
                        else -> ChatRepository.describe(e)
                    },
                )
            } catch (e: IOException) {
                PairUi.Failed(ChatRepository.describe(e))
            }
        }
    }

    /**
     * Manual connection to a development daemon: `host:port` plus the `--dev-pair-token`.
     * Plain HTTP is used for (and only for) 127.0.0.1, localhost and 10.0.2.2.
     */
    fun devConnect(hostInput: String, tokenInput: String) {
        val host = hostInput.trim().removePrefix("http://").removePrefix("https://").trimEnd('/')
        val token = tokenInput.trim()
        if (!Endpoint.isValidHost(host)) {
            _ui.value = PairUi.Failed("Enter the daemon address as host:port, for example 127.0.0.1:8421")
            return
        }
        if (token.isEmpty()) {
            _ui.value = PairUi.Failed("Enter the pairing token")
            return
        }
        val endpoint = Endpoint(host, tls = !Endpoint.isCleartextAllowed(host))
        _ui.value = PairUi.Working("Contacting ${endpoint.baseUrl}…")
        viewModelScope.launch {
            _ui.value = try {
                val info = health(endpoint, token)
                val name = info.hostname.ifBlank { host } + if (info.mock) " (mock)" else ""
                devices.add(name, endpoint, token, info.deviceId, kind = info.kind.orEmpty())
                PairUi.Done(name)
            } catch (e: LapisApiException) {
                PairUi.Failed(if (e.isUnauthorized) "The daemon rejected that token" else ChatRepository.describe(e))
            } catch (e: IOException) {
                PairUi.Failed(ChatRepository.describe(e))
            }
        }
    }
}
