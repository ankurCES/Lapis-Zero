package dev.lapiszero.app.ui.device

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.Pairing
import dev.lapiszero.app.data.remote.Telemetry
import dev.lapiszero.app.data.remote.UiPatch
import dev.lapiszero.app.data.remote.UiSettings
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.ConnectionState
import java.io.IOException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** The 320x170 LCD mirror. */
sealed interface MirrorState {
    data object Idle : MirrorState
    data object Loading : MirrorState
    class Frame(val png: ByteArray) : MirrorState

    /** `504 ui_unavailable`: lapis-ui is not running (always the case for a mock daemon). */
    data object Unavailable : MirrorState
    data class Failed(val message: String) : MirrorState
}

class DeviceViewModel(
    private val repo: ChatRepository,
    private val api: LapisApiClient,
) : ViewModel() {
    data class Ui(
        val state: DeviceState? = null,
        val telemetry: Telemetry? = null,
        val pairing: Pairing? = null,
        val connection: ConnectionState = ConnectionState.CONNECTING,
        val mirror: MirrorState = MirrorState.Idle,
        val liveMirror: Boolean = false,
        /** Optimistic copy of the LCD settings while a PATCH is in flight. */
        val uiOverride: UiSettings? = null,
        val busyAction: String? = null,
    ) {
        val lcd: UiSettings get() = uiOverride ?: state?.ui ?: UiSettings()
    }

    private val local = MutableStateFlow(Ui())
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()
    private var mirrorJob: Job? = null

    val ui: StateFlow<Ui> = combine(
        local, repo.state, repo.telemetry, repo.pairing, repo.connection,
    ) { l, state, telemetry, pairing, connection ->
        l.copy(state = state, telemetry = telemetry, pairing = pairing, connection = connection)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), Ui())

    /** Called while the cockpit is on screen: 5 s telemetry ticks instead of 30 s. */
    fun onVisible(visible: Boolean) {
        repo.setTelemetrySubscribed(visible)
        if (visible) {
            viewModelScope.launch { repo.refreshTelemetry() }
            if (local.value.mirror is MirrorState.Idle) refreshMirror()
        } else {
            setLiveMirror(false)
        }
    }

    fun refreshMirror() {
        viewModelScope.launch { fetchMirror(showLoading = true) }
    }

    private suspend fun fetchMirror(showLoading: Boolean): Boolean {
        if (showLoading && local.value.mirror !is MirrorState.Frame) {
            local.update { it.copy(mirror = MirrorState.Loading) }
        }
        val next = try {
            MirrorState.Frame(api.screenPng())
        } catch (e: LapisApiException) {
            if (e.isUiUnavailable) MirrorState.Unavailable else MirrorState.Failed(ChatRepository.describe(e))
        } catch (e: IOException) {
            MirrorState.Failed(ChatRepository.describe(e))
        }
        local.update { it.copy(mirror = next) }
        return next is MirrorState.Frame
    }

    /** About two frames per second while enabled; stops by itself when the mirror fails. */
    fun setLiveMirror(enabled: Boolean) {
        mirrorJob?.cancel()
        mirrorJob = null
        local.update { it.copy(liveMirror = enabled) }
        if (!enabled) return
        mirrorJob = viewModelScope.launch {
            while (isActive) {
                if (!fetchMirror(showLoading = false)) {
                    local.update { it.copy(liveMirror = false) }
                    break
                }
                delay(500)
            }
        }
    }

    fun setTheme(theme: String) = patch(UiPatch(theme = theme)) { it.copy(theme = theme) }

    fun setBrightness(value: Int) {
        val clamped = value.coerceIn(0, 100)
        patch(UiPatch(brightness = clamped)) { it.copy(brightness = clamped) }
    }

    fun setMute(mute: Boolean) = patch(UiPatch(muteAudio = mute)) { it.copy(muteAudio = mute) }

    fun wake() = patch(UiPatch(wake = true)) { it }

    private fun patch(patch: UiPatch, optimistic: (UiSettings) -> UiSettings) {
        local.update { it.copy(uiOverride = optimistic(it.uiOverride ?: ui.value.lcd)) }
        viewModelScope.launch {
            try {
                api.patchUi(patch)
                if (patch.wake == true) _messages.emit("Wake sent")
            } catch (e: IOException) {
                _messages.emit("Could not update the display: ${ChatRepository.describe(e)}")
            }
            // The daemon broadcasts the new settings in state_sync; stop overriding them.
            local.update { it.copy(uiOverride = null) }
        }
    }

    fun power(action: String, label: String) {
        if (local.value.busyAction != null) return
        local.update { it.copy(busyAction = action) }
        viewModelScope.launch {
            try {
                api.power(action)
                _messages.emit("$label requested")
            } catch (e: IOException) {
                _messages.emit("$label failed: ${ChatRepository.describe(e)}")
            }
            local.update { it.copy(busyAction = null) }
        }
    }

    override fun onCleared() {
        repo.setTelemetrySubscribed(false)
    }
}
