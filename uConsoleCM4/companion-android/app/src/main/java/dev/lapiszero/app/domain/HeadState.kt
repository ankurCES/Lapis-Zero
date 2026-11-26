package dev.lapiszero.app.domain

import dev.lapiszero.app.data.remote.AgentState
import dev.lapiszero.app.data.remote.LapisEvent

/**
 * What the agent's head acts out. The same ten states, under the same rules, as the head on the
 * CardputerZero's own screen (`ui/src/gfx/mascot.rs`), so the phone and the device always agree.
 */
enum class HeadState(val label: String) {
    IDLE("Idle"),
    LISTENING("Listening"),
    THINKING("Thinking"),
    WORKING("Working"),
    FETCHING("Fetching"),
    COMMAND("Running a command"),
    APPROVAL("Waiting for approval"),
    PAUSED("Paused"),
    DONE("Done"),
    ERROR("Error"),
}

/**
 * Everything the head's state depends on, folded out of the device's frames by [on] and read
 * back with [shown]. Both are pure: time comes in as `nowMs` (any monotonic clock).
 *
 * The rules are the device's own (`ui/src/views/mod.rs`):
 * - `agent_state` gives the base state;
 * - while it is `ACTING`, `state_sync.action` names the step on the host. A step stays on the
 *   face while it runs and in any case for [STEP_HOLD_MS] from when it was first seen, because
 *   the daemon is back to thinking the moment a quick click returns;
 * - `task_finished` plays one [BEAT_MS] beat (done, or error) once the agent is idle.
 */
data class HeadTrack(
    val base: HeadState = HeadState.IDLE,
    val step: Step? = null,
    val beat: HeadState? = null,
    /** When the beat came on show (a beat is set and the base is idle); null while it waits. */
    val beatSince: Long? = null,
    val hasTask: Boolean = false,
    /** The latest `state_sync` carried `action`. Decided frame by frame, as on the device. */
    val reportsAction: Boolean = false,
    /** What the pending approval is for: on older software the step that follows a grant. */
    val approved: HeadState? = null,
    val approvalId: String? = null,
) {
    data class Step(val state: HeadState, val since: Long, val running: Boolean)

    /** The state to show at `nowMs`. */
    fun shown(nowMs: Long): HeadState = when (base) {
        HeadState.WORKING, HeadState.THINKING -> step?.takeIf { it.onShow(nowMs) }?.state ?: base
        HeadState.IDLE -> beat?.takeIf { beatSince != null && nowMs - beatSince < BEAT_MS } ?: base
        else -> base
    }

    /** Milliseconds until [shown] changes by the passing of time alone; null when it will not. */
    fun nextChange(nowMs: Long): Long? = when (base) {
        HeadState.WORKING, HeadState.THINKING ->
            step?.takeIf { it.onShow(nowMs) && !(it.running && base == HeadState.WORKING) }
                ?.let { it.since + STEP_HOLD_MS - nowMs }

        HeadState.IDLE -> beatSince?.takeIf { beat != null && nowMs - it < BEAT_MS }?.let { it + BEAT_MS - nowMs }
        else -> null
    }

    private fun Step.onShow(nowMs: Long): Boolean =
        (running && base == HeadState.WORKING) || nowMs - since < STEP_HOLD_MS

    /** Folds one frame from the device in. Frames the head has no use for change nothing. */
    fun on(event: LapisEvent, nowMs: Long): HeadTrack = when (event) {
        is LapisEvent.StateSync -> {
            val state = event.state
            val synced = copy(reportsAction = state.action != null, hasTask = state.activeTask != null)
            // An `agent_state` this app does not know leaves the base as it is.
            val next = baseFor(state.agentState)?.let { synced.follow(it, state.action, nowMs) } ?: synced
            // A new task ends a beat early.
            if (!hasTask && state.activeTask != null) next.copy(beat = null) else next
        }

        // A daemon that sends `action` named this step while it ran. From an older one this
        // report, sent as the step returns, is the first word of it.
        is LapisEvent.ActivityEvent -> {
            val kind = event.activity.kind
            val reported = event.activity.status != "error" && event.activity.status != "denied"
            if (reportsAction || !reported || kind == "thought" || kind == "prompt") {
                this
            } else {
                copy(step = Step(stepFor(kind), nowMs, running = true))
            }
        }

        // The sheet is up before the WAITING_APPROVAL sync arrives.
        is LapisEvent.ApprovalRequest -> copy(
            base = HeadState.APPROVAL,
            approved = event.approval.action.takeUnless { it == "thought" || it == "prompt" }?.let { stepFor(it) },
            approvalId = event.approval.requestId,
        )

        // Decided, here or elsewhere: idle until the next sync says what the agent does now.
        is LapisEvent.ApprovalResolvedEvent ->
            if (base == HeadState.APPROVAL && (approvalId == null || approvalId == event.resolved.requestId)) {
                copy(base = HeadState.IDLE, approvalId = null)
            } else {
                this
            }

        // It follows the idle sync; one that comes first waits for it (see `beatSince`).
        is LapisEvent.TaskFinishedEvent -> when (event.finished.status) {
            "" -> this
            "done" -> copy(beat = HeadState.DONE, beatSince = null)
            "failed", "stopped" -> copy(beat = HeadState.ERROR, beatSince = null)
            else -> copy(beat = null) // cancelled, and anything newer: straight to idle
        }

        else -> this
    }.let { next ->
        // The beat's 1.6 s run from the frame that puts it on show.
        val showing = next.beat != null && next.base == HeadState.IDLE
        next.copy(beatSince = if (showing) next.beatSince ?: nowMs else null)
    }

    /** A new `agent_state`, and with it `state_sync.action` (null from older software). */
    private fun follow(agent: HeadState, action: String?, nowMs: Long): HeadTrack {
        var step = step
        var approved = approved
        if (agent == HeadState.WORKING) {
            val entering = base != HeadState.WORKING
            if (action != null) {
                val state = stepFor(action)
                val named = action.isNotEmpty()
                step = when {
                    // No step named: the last one has returned and keeps what is left of its hold.
                    step != null && !entering && !named -> step.copy(running = false)
                    // The same step, still going: the daemon repeats the frame on any status change.
                    step != null && !entering && step.running && step.state == state -> step
                    else -> Step(state, nowMs, running = named)
                }
            } else if (entering) {
                // Older software: until its event comes back, only a step that needed approval
                // is known for what it is.
                step = approved?.let { Step(it, nowMs, running = true) }
                approved = null
            }
        } else if (agent != HeadState.APPROVAL) {
            approved = null
        }
        return copy(
            base = agent,
            step = step,
            approved = approved,
            beat = if (agent == HeadState.IDLE) beat else null,
            approvalId = if (agent == HeadState.APPROVAL) approvalId else null,
        )
    }

    companion object {
        /** A step is on the face for at least this long from when it was first seen. */
        const val STEP_HOLD_MS = 1_200L

        /** One beat of Done or Error. */
        const val BEAT_MS = 1_600L

        fun baseFor(agentState: String): HeadState? = when (agentState) {
            AgentState.IDLE -> HeadState.IDLE
            AgentState.LISTENING -> HeadState.LISTENING
            AgentState.THINKING -> HeadState.THINKING
            AgentState.ACTING -> HeadState.WORKING
            AgentState.WAITING_APPROVAL -> HeadState.APPROVAL
            AgentState.PAUSED -> HeadState.PAUSED
            else -> null
        }

        /** What the head acts out for the activity kind of a step on the host. */
        fun stepFor(kind: String): HeadState = when (kind) {
            "screenshot", "wait" -> HeadState.FETCHING
            "ssh_exec", "agent_run" -> HeadState.COMMAND
            // vnc_click, vnc_move, vnc_drag, vnc_scroll, type_text, vnc_key, "" (no step named)
            // and any kind added later.
            else -> HeadState.WORKING
        }
    }
}
