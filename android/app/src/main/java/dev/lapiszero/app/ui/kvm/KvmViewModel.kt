package dev.lapiszero.app.ui.kvm

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.remote.Activity
import dev.lapiszero.app.data.remote.AgentState
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.data.remote.NudgeRequest
import dev.lapiszero.app.domain.ChatRepository
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/** A recent click, in pixels of the screenshot the agent was looking at. */
data class ClickMarker(val x: Int, val y: Int, val label: String, val pending: Boolean = false)

private val CLICK_RE = Regex("""\(\s*(-?\d+)\s*,\s*(-?\d+)""")

/** `click(840, 412) left` -> (840, 412). The daemon writes coordinates into the summary. */
fun parseClick(summary: String): Pair<Int, Int>? =
    CLICK_RE.find(summary)?.destructured?.let { (x, y) -> x.toInt() to y.toInt() }

fun buildMarkers(activity: List<Activity>, approvals: List<Approval>, limit: Int = 5): List<ClickMarker> {
    val clicks = activity.filter { it.kind == "vnc_click" || it.kind == "nudge" }
        .mapNotNull { a -> parseClick(a.summary)?.let { (x, y) -> ClickMarker(x, y, "#${a.step}") } }
        .takeLast(limit)
    val waiting = approvals.mapNotNull { a ->
        val x = a.clickX
        val y = a.clickY
        if (x != null && y != null) ClickMarker(x, y, "?", pending = true) else null
    }
    return clicks + waiting
}

class KvmViewModel(private val repo: ChatRepository) : ViewModel() {
    data class Ui(
        val screenshotId: Long? = null,
        val markers: List<ClickMarker> = emptyList(),
        /** Newest first. */
        val ticker: List<Activity> = emptyList(),
        val agentState: String = AgentState.IDLE,
        val activeHost: String = "",
        val taskTitle: String? = null,
        val step: Int = 0,
        val maxSteps: Int = 0,
        /** Manual assist is only possible while the agent is PAUSED or WAITING_APPROVAL. */
        val canNudge: Boolean = false,
        val running: Boolean = false,
    )

    private val _busy = MutableStateFlow(false)
    val busy: StateFlow<Boolean> = _busy.asStateFlow()
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    val ui: StateFlow<Ui> = combine(repo.activity, repo.state, repo.approvals) { activity, state, approvals ->
        val latest = activity.lastOrNull { it.screenshotId != null }?.screenshotId
            ?: approvals.lastOrNull { it.screenshotId != null }?.screenshotId
        Ui(
            screenshotId = latest,
            markers = buildMarkers(activity, approvals),
            ticker = activity.asReversed(),
            agentState = state?.agentState ?: AgentState.IDLE,
            activeHost = state?.activeHost.orEmpty(),
            taskTitle = state?.activeTask?.title,
            step = state?.activeTask?.step ?: 0,
            maxSteps = state?.activeTask?.maxSteps ?: 0,
            canNudge = state?.canNudge == true,
            running = state?.isRunning == true,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), Ui())

    /** "Take control temporarily". */
    fun pause() = act("Could not pause the agent") { repo.pause() }

    /** "Hand back to agent". */
    fun resume() = act("Could not resume the agent") { repo.resume() }

    fun killAll() {
        viewModelScope.launch { if (repo.killAll()) _messages.emit("Kill switch sent") }
    }

    fun nudgeClick(x: Int, y: Int) = nudge(NudgeRequest(action = "click", x = x, y = y))

    fun nudgeType(text: String) {
        if (text.isNotEmpty()) nudge(NudgeRequest(action = "type", text = text))
    }

    fun nudgeKey(combo: String) {
        if (combo.isNotBlank()) nudge(NudgeRequest(action = "key", combo = combo.trim()))
    }

    private fun nudge(request: NudgeRequest) {
        val state = ui.value
        if (!state.canNudge) {
            _messages.tryEmit("Manual assist is only available while the agent is paused")
            return
        }
        act("Manual assist failed") { repo.nudge(state.activeHost, request) }
    }

    private fun act(fallback: String, block: suspend () -> Result<*>) {
        if (_busy.value) return
        _busy.value = true
        viewModelScope.launch {
            block().onFailure { _messages.emit(it.message ?: fallback) }
            _busy.value = false
        }
    }
}
