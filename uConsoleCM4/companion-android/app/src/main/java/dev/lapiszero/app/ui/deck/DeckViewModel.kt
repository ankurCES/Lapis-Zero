package dev.lapiszero.app.ui.deck

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.remote.AuditEntry
import dev.lapiszero.app.data.remote.DeckStatus
import dev.lapiszero.app.data.remote.DeckSummary
import dev.lapiszero.app.data.remote.DeckTool
import dev.lapiszero.app.data.remote.GpsSnapshot
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.MeshMessage
import dev.lapiszero.app.data.remote.MeshSnapshot
import dev.lapiszero.app.data.remote.Rail
import dev.lapiszero.app.data.remote.SdrSnapshot
import dev.lapiszero.app.data.remote.SdrSpectrum
import dev.lapiszero.app.data.remote.ToolInvokeResult
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.DeckAlert
import dev.lapiszero.app.domain.DeckRepository
import dev.lapiszero.app.domain.formatMhz
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
import kotlinx.serialization.json.JsonObject

/** Where one tool call (a quick action, a mesh send) stands. */
sealed interface ActionState {
    /** Running; a Tier 1/2 call is waiting for the owner's decision meanwhile. */
    data class Running(val tier: Int) : ActionState
    data class Done(val tool: String, val result: ToolInvokeResult) : ActionState {
        val rows: List<ResultRow> get() = ToolResults.rows(tool, result.result)
        val synthetic: Boolean get() = ToolResults.synthetic(result.result)
    }
    data class Failed(val message: String) : ActionState
}

/**
 * The Deck screen (Lapis-Deck only): power rails and battery draw, GPS, mesh nodes and channel
 * chat, SDR status with tune presets and quick decoders, quick NetOps, the live alert feed and the
 * audit log. Every tool call goes through the daemon's Policy Gate (`/v1/deck/tools/invoke`).
 */
class DeckViewModel(
    private val repo: ChatRepository,
    private val deck: DeckRepository,
    private val api: LapisApiClient,
    private val pollMs: Long = 5_000,
) : ViewModel() {
    data class Ui(
        val connection: ConnectionState = ConnectionState.CONNECTING,
        val summary: DeckSummary? = null,
        val status: DeckStatus? = null,
        val gps: GpsSnapshot? = null,
        val mesh: MeshSnapshot? = null,
        val liveMesh: List<MeshMessage> = emptyList(),
        val sdr: SdrSnapshot? = null,
        val spectrum: SdrSpectrum? = null,
        val tools: Map<String, DeckTool> = emptyMap(),
        val alerts: List<DeckAlert> = emptyList(),
        val audit: List<AuditEntry>? = null,
        val auditLoading: Boolean = false,
        val loading: Boolean = false,
        val error: String? = null,
        val railBusy: Set<String> = emptySet(),
        val tuning: Long? = null,
        /** By quick-action key, plus [QuickActions.MESH_SEND] for the mesh composer. */
        val actions: Map<String, ActionState> = emptyMap(),
    ) {
        /** The REST status when there is one (fresher after a switch), else the last `state_sync`. */
        val rails: List<Rail>
            get() = (status?.rails?.takeIf { it.isNotEmpty() } ?: summary?.rails.orEmpty())
                .sortedBy { Rail.ORDER.indexOf(it.name).let { i -> if (i < 0) Int.MAX_VALUE else i } }

        val batteryW: Double? get() = status?.battery?.powerW ?: summary?.batteryW

        val harnessUp: Boolean? get() = status?.harness ?: summary?.harnessUp

        val meshMessages: List<MeshMessage> get() = MeshRules.merge(mesh?.messages.orEmpty(), liveMesh)

        fun tierOf(tool: String): Int? = tools[tool]?.tier ?: QuickActions.DEFAULT_TIERS[tool]
    }

    private val local = MutableStateFlow(Ui())
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()
    private var pollJob: Job? = null

    val ui: StateFlow<Ui> = combine(
        combine(local, repo.connection, deck.summary) { l, connection, summary ->
            l.copy(connection = connection, summary = summary)
        },
        deck.alerts, deck.liveMesh, deck.spectrum,
    ) { l, alerts, live, spectrum ->
        l.copy(alerts = alerts, liveMesh = live, spectrum = spectrum)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), Ui())

    /** While the screen is on: alerts count as seen, the waterfall streams, status is polled. */
    fun onVisible(visible: Boolean) {
        deck.setWatching(visible)
        deck.setSpectrumSubscribed(visible)
        pollJob?.cancel()
        pollJob = null
        if (!visible) return
        pollJob = viewModelScope.launch {
            refreshAll()
            while (isActive) {
                delay(pollMs)
                poll()
            }
        }
    }

    fun refresh() {
        viewModelScope.launch { refreshAll() }
    }

    private suspend fun refreshAll() {
        local.update { it.copy(loading = true) }
        if (local.value.tools.isEmpty()) {
            try {
                val tools = api.deckTools().associateBy { it.name }
                local.update { it.copy(tools = tools) }
            } catch (e: IOException) {
                // The default tiers stand in until the next refresh.
            }
        }
        val mesh = runCatching { api.mesh() }.getOrNull()
        if (mesh != null) local.update { it.copy(mesh = mesh) }
        poll()
        local.update { it.copy(loading = false) }
    }

    private suspend fun poll() {
        val error = try {
            val status = api.deckStatus()
            local.update { it.copy(status = status) }
            null
        } catch (e: IOException) {
            ChatRepository.describe(e)
        }
        runCatching { api.gps() }.getOrNull()?.let { gps -> local.update { it.copy(gps = gps) } }
        runCatching { api.sdr() }.getOrNull()?.let { sdr -> local.update { it.copy(sdr = sdr) } }
        local.update { it.copy(error = error) }
    }

    // ------------------------------------------------------------------ power

    /** The owner's own switch (audited on the deck). Wi-Fi/Bluetooth are confirmed by the screen first. */
    fun setRail(name: String, on: Boolean) {
        if (name in local.value.railBusy) return
        local.update { it.copy(railBusy = it.railBusy + name) }
        viewModelScope.launch {
            try {
                api.setRail(name, on)
                _messages.emit("${Rail.label(name)} switched ${if (on) "on" else "off"}")
                runCatching { api.deckStatus() }.getOrNull()?.let { s -> local.update { it.copy(status = s) } }
            } catch (e: LapisApiException) {
                _messages.emit(
                    if (e.isRailFailed) "${Rail.label(name)} did not switch: ${e.message}"
                    else "Could not switch ${Rail.label(name)}: ${ChatRepository.describe(e)}",
                )
            } catch (e: IOException) {
                _messages.emit("Could not switch ${Rail.label(name)}: ${ChatRepository.describe(e)}")
            }
            local.update { it.copy(railBusy = it.railBusy - name) }
        }
    }

    // ------------------------------------------------------------------ SDR

    fun tune(hz: Long) {
        if (local.value.tuning != null) return
        local.update { it.copy(tuning = hz) }
        viewModelScope.launch {
            try {
                val r = api.tune(hz)
                if (r.ok) {
                    _messages.emit("Tuned to ${formatMhz(hz)}")
                    runCatching { api.sdr() }.getOrNull()?.let { sdr -> local.update { it.copy(sdr = sdr) } }
                } else {
                    _messages.emit("Not tuned: ${r.error ?: "the SDR refused"}")
                }
            } catch (e: IOException) {
                _messages.emit("Not tuned: ${ChatRepository.describe(e)}")
            }
            local.update { it.copy(tuning = null) }
        }
    }

    // ------------------------------------------------------------------ tools

    fun run(action: QuickAction) = invoke(action.key, action.tool, action.args)

    /** Sends on a Meshtastic channel through `mesh.send_message` (Tier 1: the owner approves first). */
    fun sendMesh(channel: Int, text: String): Boolean {
        if (local.value.actions[QuickActions.MESH_SEND] is ActionState.Running) return false
        MeshRules.problem(channel, text)?.let {
            _messages.tryEmit(it)
            return false
        }
        invoke(QuickActions.MESH_SEND, QuickActions.MESH_SEND, MeshRules.args(channel, text)) { result ->
            if (result.ok) {
                runCatching { api.mesh() }.getOrNull()?.let { m -> local.update { it.copy(mesh = m) } }
            }
        }
        return true
    }

    private fun invoke(key: String, tool: String, args: JsonObject, after: suspend (ToolInvokeResult) -> Unit = {}) {
        if (local.value.actions[key] is ActionState.Running) return
        val tier = local.value.tierOf(tool) ?: 0
        local.update { it.copy(actions = it.actions + (key to ActionState.Running(tier))) }
        viewModelScope.launch {
            val next = try {
                val result = api.invokeTool(tool, args)
                after(result)
                if (result.ok) {
                    ActionState.Done(tool, result)
                } else {
                    val why = describeDecision(result.decision, result.by)
                    ActionState.Failed(result.error?.takeIf { it.isNotBlank() }?.let { "$why: $it" } ?: why)
                }
            } catch (e: IOException) {
                ActionState.Failed(ChatRepository.describe(e))
            }
            if (key == QuickActions.MESH_SEND) {
                _messages.emit(if (next is ActionState.Done) "Sent on the mesh" else "Not sent: ${(next as ActionState.Failed).message}")
            }
            local.update { it.copy(actions = it.actions + (key to next)) }
        }
    }

    // ------------------------------------------------------------------ feed / audit

    fun clearAlerts() = deck.clearAlerts()

    fun loadAudit(limit: Int = 50) {
        if (local.value.auditLoading) return
        local.update { it.copy(auditLoading = true) }
        viewModelScope.launch {
            try {
                val entries = api.audit(limit)
                local.update { it.copy(audit = entries) } // the daemon sends newest first
            } catch (e: IOException) {
                _messages.emit("Audit log unavailable: ${ChatRepository.describe(e)}")
            }
            local.update { it.copy(auditLoading = false) }
        }
    }

    override fun onCleared() {
        deck.setWatching(false)
        deck.setSpectrumSubscribed(false)
    }
}
