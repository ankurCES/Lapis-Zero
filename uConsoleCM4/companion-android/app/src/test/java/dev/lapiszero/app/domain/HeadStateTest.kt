package dev.lapiszero.app.domain

import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.data.remote.LapisEvent
import dev.lapiszero.app.domain.HeadState.APPROVAL
import dev.lapiszero.app.domain.HeadState.COMMAND
import dev.lapiszero.app.domain.HeadState.DONE
import dev.lapiszero.app.domain.HeadState.ERROR
import dev.lapiszero.app.domain.HeadState.FETCHING
import dev.lapiszero.app.domain.HeadState.IDLE
import dev.lapiszero.app.domain.HeadState.LISTENING
import dev.lapiszero.app.domain.HeadState.PAUSED
import dev.lapiszero.app.domain.HeadState.THINKING
import dev.lapiszero.app.domain.HeadState.WORKING
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The head's state from the device's frames (head spec, section 9): the same table of cases
 * the device's own `ui/src/views/mod.rs` is tested against, so the two cannot drift apart
 * unnoticed. Plain JVM: the mapping takes its time as a number.
 */
class HeadStateTest {
    private val chat = "c_9e838c02"

    /** A track with frames applied at given times (ms). */
    private class Run(var track: HeadTrack = HeadTrack()) {
        fun at(ms: Long, frame: String): Run {
            track = track.on(LapisEvent.parse(frame) ?: error("unparseable test frame: $frame"), ms)
            return this
        }

        fun shown(ms: Long): HeadState = track.shown(ms)
    }

    /** `state_sync` of software that reports the step it is executing (`action`, "" for none). */
    private fun sync(agentState: String, action: String = "", task: Boolean = agentState != "IDLE") =
        Fixtures.stateSync(agentState, activeChatId = chat.takeIf { task }, action = action)

    /** `state_sync` of older software: no `action` field at all. */
    private fun oldSync(agentState: String, task: Boolean = agentState != "IDLE") =
        Fixtures.stateSync(agentState, activeChatId = chat.takeIf { task })

    // ------------------------------------------------------------------ base

    @Test
    fun `agent_state gives the base state`() {
        val expected = mapOf(
            "IDLE" to IDLE, "LISTENING" to LISTENING, "THINKING" to THINKING, "ACTING" to WORKING,
            "WAITING_APPROVAL" to APPROVAL, "PAUSED" to PAUSED,
        )
        for ((agentState, state) in expected) {
            assertEquals(agentState, state, Run().at(0, sync(agentState)).shown(0))
            assertEquals("$agentState, older software", state, Run().at(0, oldSync(agentState)).shown(0))
        }
        assertEquals(IDLE, HeadTrack().shown(0)) // before the first frame
    }

    @Test
    fun `an agent_state this app does not know leaves the base as it is`() {
        val run = Run().at(0, sync("THINKING")).at(10, sync("DREAMING"))
        assertEquals(THINKING, run.shown(10))
        // The rest of the frame is still read: it reports `action`, and it has a task.
        assertEquals(true, run.track.reportsAction)
        assertEquals(true, run.track.hasTask)
    }

    // ------------------------------------------------------------------ the step

    @Test
    fun `while acting the action names what the head does`() {
        val expected = mapOf(
            "screenshot" to FETCHING, "wait" to FETCHING,
            "ssh_exec" to COMMAND, "agent_run" to COMMAND,
            "vnc_click" to WORKING, "vnc_move" to WORKING, "vnc_drag" to WORKING, "vnc_scroll" to WORKING,
            "type_text" to WORKING, "vnc_key" to WORKING,
            "" to WORKING, "a_kind_added_later" to WORKING,
        )
        for ((action, state) in expected) {
            assertEquals("action '$action'", state, Run().at(0, sync("ACTING", action)).shown(0))
            assertEquals("action '$action', long after", state, Run().at(0, sync("ACTING", action)).shown(60_000))
        }
    }

    @Test
    fun `action in a frame that is not acting is ignored`() {
        for (agentState in listOf("IDLE", "THINKING", "WAITING_APPROVAL", "PAUSED", "LISTENING")) {
            val run = Run().at(0, sync(agentState, action = "ssh_exec"))
            assertEquals(agentState, HeadTrack.baseFor(agentState), run.shown(0))
            assertNull(agentState, run.track.step)
        }
    }

    @Test
    fun `a quick step is still seen for its hold after the daemon is back to thinking`() {
        // A click that has come and gone between two frames.
        val run = Run().at(1_000, sync("ACTING", "screenshot")).at(1_050, sync("THINKING"))
        assertEquals(FETCHING, run.shown(1_050))
        assertEquals(FETCHING, run.shown(2_199))
        assertEquals(THINKING, run.shown(2_200)) // 1.2 s from when it was first seen
        assertEquals(1_150L, run.track.nextChange(1_050))
        assertNull(run.track.nextChange(2_200))
    }

    @Test
    fun `a long step is on the face while it runs and hands back at once`() {
        val run = Run().at(0, sync("ACTING", "ssh_exec"))
        assertEquals(COMMAND, run.shown(30_000))
        assertNull("nothing changes by the clock while it runs", run.track.nextChange(30_000))
        run.at(30_000, sync("THINKING"))
        assertEquals(THINKING, run.shown(30_000))
    }

    @Test
    fun `repeats of the frame do not restart the hold of a running step`() {
        // The daemon repeats state_sync whenever anything in its status changes.
        val run = Run().at(0, sync("ACTING", "screenshot")).at(500, sync("ACTING", "screenshot")).at(1_000, sync("ACTING", "screenshot"))
        assertEquals(0L, run.track.step?.since)
        run.at(1_100, sync("THINKING"))
        assertEquals(FETCHING, run.shown(1_100))
        assertEquals(THINKING, run.shown(1_200))
    }

    @Test
    fun `a step that returned while still acting keeps what is left of its hold`() {
        val run = Run().at(0, sync("ACTING", "screenshot")).at(300, sync("ACTING", ""))
        assertEquals(HeadTrack.Step(FETCHING, since = 0, running = false), run.track.step)
        assertEquals(FETCHING, run.shown(300))
        assertEquals(900L, run.track.nextChange(300))
        assertEquals(WORKING, run.shown(1_200))
        // Still no step named: the same one stays returned, its clock untouched.
        run.at(600, sync("ACTING", ""))
        assertEquals(0L, run.track.step?.since)
    }

    @Test
    fun `another kind of step or the same kind again after a return is a new step`() {
        val run = Run().at(0, sync("ACTING", "screenshot")).at(400, sync("ACTING", "vnc_click"))
        assertEquals(WORKING, run.shown(400))
        assertEquals(HeadTrack.Step(WORKING, since = 400, running = true), run.track.step)
        // screenshot, returned, screenshot again: first seen now.
        run.at(500, sync("ACTING", "screenshot")).at(600, sync("ACTING", "")).at(700, sync("ACTING", "screenshot"))
        assertEquals(HeadTrack.Step(FETCHING, since = 700, running = true), run.track.step)
    }

    @Test
    fun `entering acting always starts a step`() {
        // The same kind as the step kept from before the thinking: still a new one.
        val run = Run().at(0, sync("ACTING", "ssh_exec")).at(100, sync("THINKING")).at(5_000, sync("ACTING", "ssh_exec"))
        assertEquals(HeadTrack.Step(COMMAND, since = 5_000, running = true), run.track.step)
        // Acting with no step named is plain working, and not "running".
        val plain = Run().at(0, sync("THINKING")).at(100, sync("ACTING", ""))
        assertEquals(WORKING, plain.shown(100))
        assertEquals(HeadTrack.Step(WORKING, since = 100, running = false), plain.track.step)
    }

    @Test
    fun `approval and pause show over a step and the step runs out its hold behind them`() {
        val run = Run().at(0, sync("ACTING", "screenshot")).at(100, sync("PAUSED", action = "screenshot"))
        assertEquals(PAUSED, run.shown(100))
        assertEquals(HeadTrack.Step(FETCHING, since = 0, running = true), run.track.step)
        assertNull(run.track.nextChange(100))
        run.at(200, sync("WAITING_APPROVAL"))
        assertEquals(APPROVAL, run.shown(200))
        // Back to thinking within the hold: the step is on show again for what is left of it.
        run.at(300, sync("THINKING"))
        assertEquals(FETCHING, run.shown(300))
        assertEquals(THINKING, run.shown(1_200))
    }

    // ------------------------------------------------------------------ the beat

    @Test
    fun `task_finished plays one beat and then idle`() {
        for ((status, beat) in listOf("done" to DONE, "failed" to ERROR, "stopped" to ERROR)) {
            val run = Run().at(0, sync("ACTING", "vnc_click")).at(5_000, sync("IDLE")).at(5_010, Fixtures.taskFinished(status))
            assertEquals(status, beat, run.shown(5_010))
            assertEquals(status, beat, run.shown(6_609))
            assertEquals(status, IDLE, run.shown(6_610)) // 1.6 s from when it came on show
            assertEquals(status, 1_600L, run.track.nextChange(5_010))
            assertNull(status, run.track.nextChange(6_610))
            // Repeats of the idle sync do not replay it.
            run.at(7_000, sync("IDLE"))
            assertEquals(status, IDLE, run.shown(7_000))
        }
    }

    @Test
    fun `a cancelled task goes straight to idle`() {
        for (status in listOf("cancelled", "a_status_added_later")) {
            val run = Run().at(0, sync("THINKING")).at(100, sync("IDLE")).at(110, Fixtures.taskFinished(status))
            assertEquals(status, IDLE, run.shown(110))
            assertNull(status, run.track.beat)
        }
        // It also takes back a beat that was about to play.
        val run = Run().at(0, sync("IDLE")).at(10, Fixtures.taskFinished("done")).at(20, Fixtures.taskFinished("cancelled"))
        assertEquals(IDLE, run.shown(20))
        // A frame without a status says nothing.
        val silent = Run().at(0, sync("IDLE")).at(10, Fixtures.taskFinished("done")).at(20, Fixtures.taskFinished(""))
        assertEquals(DONE, silent.shown(20))
    }

    @Test
    fun `a beat that comes before the idle sync waits for it`() {
        val run = Run().at(0, sync("ACTING", "ssh_exec")).at(4_000, sync("THINKING", task = false))
            .at(4_010, Fixtures.taskFinished("done"))
        assertEquals(THINKING, run.shown(4_010))
        assertNull(run.track.beatSince)
        // The sync that follows is not a new task: the beat survives it and starts its clock.
        run.at(6_000, sync("IDLE"))
        assertEquals(DONE, run.shown(6_000))
        assertEquals(DONE, run.shown(7_599))
        assertEquals(IDLE, run.shown(7_600))
    }

    @Test
    fun `a new task ends a beat early`() {
        // Any sync that is not idle.
        val thinking = Run().at(0, sync("IDLE")).at(10, Fixtures.taskFinished("done")).at(500, sync("THINKING"))
        assertEquals(THINKING, thinking.shown(500))
        assertNull(thinking.track.beat)
        // A task appearing, even while the daemon still says idle.
        val queued = Run().at(0, sync("IDLE")).at(10, Fixtures.taskFinished("failed")).at(500, sync("IDLE", task = true))
        assertEquals(IDLE, queued.shown(500))
        assertNull(queued.track.beat)
        // A beat that follows another starts its own 1.6 s.
        val twice = Run().at(0, sync("IDLE")).at(10, Fixtures.taskFinished("done")).at(1_000, Fixtures.taskFinished("failed"))
        assertEquals(ERROR, twice.shown(2_599))
        assertEquals(IDLE, twice.shown(2_600))
    }

    // ------------------------------------------------------------------ approvals

    @Test
    fun `an approval request shows at once and a decision is idle until the next sync`() {
        val run = Run().at(0, sync("ACTING", "vnc_click")).at(100, Fixtures.approval("req_1"))
        assertEquals(APPROVAL, run.shown(100)) // before the WAITING_APPROVAL sync
        run.at(120, sync("WAITING_APPROVAL"))
        assertEquals(APPROVAL, run.shown(120))
        // Somebody else's request being resolved is not this one.
        run.at(130, Fixtures.approvalResolved("req_other"))
        assertEquals(APPROVAL, run.shown(130))
        run.at(140, Fixtures.approvalResolved("req_1"))
        assertEquals(IDLE, run.shown(140))
        run.at(160, sync("ACTING", "vnc_click"))
        assertEquals(WORKING, run.shown(160))
        // An approval this phone only knows from a sync is cleared by any decision.
        val joined = Run().at(0, sync("WAITING_APPROVAL")).at(10, Fixtures.approvalResolved("req_9", "deny", "device"))
        assertEquals(IDLE, joined.shown(10))
        // A decision when nothing is waiting changes nothing.
        assertEquals(THINKING, Run().at(0, sync("THINKING")).at(10, Fixtures.approvalResolved()).shown(10))
    }

    // ------------------------------------------------------------------ older device software

    @Test
    fun `older software without action shows plain working until a step reports back`() {
        val run = Run().at(0, oldSync("THINKING")).at(100, oldSync("ACTING"))
        assertEquals(false, run.track.reportsAction)
        assertEquals(WORKING, run.shown(100))
        assertNull(run.track.step)
        // Its activity event comes when the step returns; the kind of the latest one is shown.
        run.at(900, Fixtures.activity(kind = "screenshot"))
        assertEquals(FETCHING, run.shown(900))
        run.at(950, Fixtures.activity(kind = "ssh_exec"))
        assertEquals(COMMAND, run.shown(950))
        run.at(960, Fixtures.activity(kind = "agent_run", status = "running")) // a progress line
        assertEquals(COMMAND, run.shown(960))
        assertEquals(960L, run.track.step?.since)
        // ... and is held for 1.2 s into thinking.
        run.at(1_000, oldSync("THINKING"))
        assertEquals(COMMAND, run.shown(2_159))
        assertEquals(THINKING, run.shown(2_160))
        // Entering acting again forgets it.
        run.at(3_000, oldSync("ACTING"))
        assertNull(run.track.step)
        assertEquals(WORKING, run.shown(3_000))
    }

    @Test
    fun `older software activity that is no step on the host does not touch the head`() {
        val run = Run().at(0, oldSync("ACTING")).at(10, Fixtures.activity(kind = "vnc_click"))
        val before = run.track
        for (frame in listOf(
            Fixtures.activity(kind = "thought"), Fixtures.activity(kind = "prompt"),
            Fixtures.activity(kind = "ssh_exec", status = "error"), Fixtures.activity(kind = "ssh_exec", status = "denied"),
        )) {
            assertEquals(frame, before, run.at(500, frame).track)
        }
    }

    @Test
    fun `older software knows a step that needed approval for what it is`() {
        val run = Run().at(0, oldSync("ACTING")).at(100, Fixtures.approval("req_1", action = "ssh_exec"))
            .at(110, oldSync("WAITING_APPROVAL"))
        assertEquals(APPROVAL, run.shown(110))
        run.at(5_000, Fixtures.approvalResolved("req_1")).at(5_020, oldSync("ACTING"))
        assertEquals(COMMAND, run.shown(5_020))
        assertEquals(HeadTrack.Step(COMMAND, since = 5_020, running = true), run.track.step)
        // Denied instead: the agent goes back to thinking and the approval is forgotten.
        val denied = Run().at(0, oldSync("ACTING")).at(100, Fixtures.approval("req_1", action = "ssh_exec"))
            .at(200, Fixtures.approvalResolved("req_1", "deny")).at(210, oldSync("THINKING")).at(900, oldSync("ACTING"))
        assertEquals(WORKING, denied.shown(900))
    }

    @Test
    fun `software that sends action is not second-guessed by its activity events`() {
        val run = Run().at(0, sync("ACTING", "screenshot"))
        val before = run.track
        assertEquals(before, run.at(100, Fixtures.activity(kind = "ssh_exec")).track)
        assertEquals(FETCHING, run.shown(100))
        // Decided frame by frame: a sync without the field is older software again.
        run.at(200, oldSync("ACTING")).at(300, Fixtures.activity(kind = "ssh_exec"))
        assertEquals(COMMAND, run.shown(300))
    }

    @Test
    fun `frames the head has no use for change nothing`() {
        val run = Run().at(0, sync("ACTING", "ssh_exec"))
        val before = run.track
        for (frame in listOf(
            """{"type": "toast", "level": "error", "text": "VNC disconnected"}""",
            """{"type": "pong", "ts": 1}""",
            Fixtures.messageCreated(chat, Fixtures.assistantMessage()),
            """{"type": "from_the_future", "x": 1}""",
        )) {
            assertEquals(frame, before, run.at(50, frame).track)
        }
    }
}
