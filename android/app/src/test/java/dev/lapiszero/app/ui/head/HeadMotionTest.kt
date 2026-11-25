package dev.lapiszero.app.ui.head

import dev.lapiszero.app.domain.HeadState
import kotlin.math.abs
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * How the head moves, against the numbers of the head spec (sections 6 to 8): the poses, the
 * easing between states and the still pose that the "remove animations" setting gets.
 */
class HeadMotionTest {
    private fun ns(seconds: Double): Long = (seconds * 1e9).toLong()

    private fun pose(state: HeadState, t: Double, tau: Float = 0f, calm: Boolean = false) =
        HeadPose().also { target(state, t, tau, rms = 0.5f, calm, it) }

    private fun HeadPose.values() = listOf(yaw, pitch, roll, lift, lookX, lookY, open, wide, glow)

    /** Number by number: a list would tell 0.0 from -0.0, which the head does not. */
    private fun assertSame(expected: List<Float>, actual: List<Float>, what: String = "") {
        expected.zip(actual).forEachIndexed { i, (e, a) -> assertEquals("$what value $i", e, a, 0f) }
    }

    // A stretch of idle with neither a blink (4.08 s, 10.21 s) nor a glance (5.58 to 7.78 s) in it.
    private val quietTime = 9.0

    @Test
    fun `the hash and what hangs on it are the device's`() {
        // Pinned from the device's `hash01`: a phone next to it blinks and glances in step.
        assertEquals(0.78607357f, hash01(0), 1e-7f)
        assertEquals(0.9031986f, hash01(1), 1e-7f)
        assertEquals(0.073026f, hash01(2), 1e-7f)
        assertEquals(0.43176204f, hash01(0x5BD1), 1e-7f)
        // One 240 ms blink per 5.6 s slot: shut by 72 ms, shut for 48, open again by 240.
        val blinkAt = 4.081609573364258
        assertEquals(1f, blink(blinkAt - 0.01), 0f)
        assertEquals(0.5f, blink(blinkAt + 0.036), 0.01f)
        assertEquals(0f, blink(blinkAt + 0.1), 0f)
        assertEquals(0.5f, blink(blinkAt + 0.18), 0.01f)
        assertEquals(1f, blink(blinkAt + 0.25), 0f)
        // One 2.2 s glance per 13 s slot: 0.35 s out, held, 0.35 s back.
        assertEquals(0f, glance(5.5), 0f)
        assertEquals(1f, glance(5.5836 + 1.0), 0f) // slot 0 looks right
        assertEquals(-1f, glance(22.8782 + 1.0), 0f) // slot 1 looks left
        assertEquals(0f, glance(quietTime), 0f)
        assertEquals(0.5f, glance(5.5836 + 0.175), 0.02f)
    }

    @Test
    fun `accents and eye shapes belong to the states the spec gives them`() {
        val none = setOf(HeadState.IDLE, HeadState.LISTENING, HeadState.PAUSED)
        val amber = setOf(HeadState.APPROVAL, HeadState.ERROR)
        for (state in HeadState.entries) {
            val expected = when (state) {
                in none -> Accent.NONE
                in amber -> Accent.AMBER
                else -> Accent.BLUE
            }
            assertEquals(state.name, expected, state.accent)
            assertEquals(state.name, if (expected == Accent.NONE) 0f else 1f, pose(state, quietTime).glow, 0f)
        }
        assertEquals(Eyes.HAPPY, HeadState.DONE.eyes)
        assertEquals(Eyes.CROSS, HeadState.ERROR.eyes)
        assertEquals(Eyes.ASLEEP, HeadState.PAUSED.eyes)
        assertEquals(Eyes.PROMPT, HeadState.COMMAND.eyes)
        assertEquals(setOf(Eyes.OPEN), (HeadState.entries - setOf(HeadState.DONE, HeadState.ERROR, HeadState.PAUSED, HeadState.COMMAND)).map { it.eyes }.toSet())
    }

    @Test
    fun `each state holds the pose the spec gives it`() {
        // Idle: a breath of 4 s and a slow sway; never more than these.
        pose(HeadState.IDLE, quietTime).let {
            assertEquals(0.03f * wave(quietTime, 4.0), it.lift, 1e-6f)
            assertEquals(0.05f + 0.02f * wave(quietTime, 4.0), it.pitch, 1e-6f)
            assertEquals(0.07f * wave(quietTime, 9.0), it.yaw, 1e-6f)
            assertEquals(1f, it.open, 0f)
        }
        // ... and a glance of 18 degrees with the eyes leading.
        pose(HeadState.IDLE, 6.6).let {
            assertEquals(0.32f + 0.07f * wave(6.6, 9.0), it.yaw, 1e-6f)
            assertEquals(0.09f, it.lookX, 1e-6f)
        }
        pose(HeadState.LISTENING, quietTime).let {
            assertEquals(listOf(-0.10f, -0.10f), listOf(it.yaw, it.pitch))
            assertEquals(0.03f + 0.06f * 0.5f, it.lift, 1e-6f)
            assertEquals(1.06f + 0.24f * 0.5f, it.wide, 1e-6f)
        }
        pose(HeadState.THINKING, quietTime).let {
            assertEquals(-0.22f, it.pitch, 0f)
            assertEquals(-0.11f, it.lookY, 0f)
            assertEquals(0.07f * wave(quietTime - 1.2, 4.8), it.roll, 1e-6f) // 1.2 s behind the yaw
        }
        // Working: the nod lands on the beat and eases off; the eyes are narrowed.
        pose(HeadState.WORKING, 9.0).let {
            assertEquals(0.34f, it.pitch, 1e-5f)
            assertEquals(-0.09f, it.lift, 1e-5f)
            assertEquals(0.66f, it.open, 1e-6f)
        }
        assertEquals(0.20f + 0.14f * 0.25f, pose(HeadState.WORKING, 9.25).pitch, 1e-5f)
        // Fetching: turned 30 degrees to look out, the eyes widening as each packet arrives.
        pose(HeadState.FETCHING, 9.0).let {
            assertEquals(0.52f, it.yaw, 0.04f + 1e-6f)
            assertEquals(1.18f, it.wide, 1e-5f) // a packet has just gone in
            assertEquals(0.10f, it.lookX, 0f)
        }
        assertEquals(1f, pose(HeadState.FETCHING, 9.3).wide, 1e-5f)
        // Command never blinks; approval hops twice every 1.6 s; paused sleeps.
        assertEquals(1f, pose(HeadState.COMMAND, 4.0816 + 0.1).open, 0f)
        assertEquals(0.02f + 0.14f, pose(HeadState.APPROVAL, 9.6 + 0.14).lift, 1e-4f)
        assertEquals(0.02f + 0.09f, pose(HeadState.APPROVAL, 9.6 + 0.42).lift, 1e-4f)
        assertEquals(0.02f, pose(HeadState.APPROVAL, 9.6 + 1.0).lift, 1e-6f)
        assertEquals(1.18f, pose(HeadState.APPROVAL, quietTime).wide, 0f)
        pose(HeadState.PAUSED, 4.0816 + 0.1).let {
            assertEquals(1f, it.open, 0f) // asleep is a shape of its own, not a blink
            assertEquals(listOf(-0.14f, -0.10f), listOf(it.yaw, it.roll))
        }
        // The beats run on the time since they began.
        pose(HeadState.DONE, quietTime, tau = 0.21f).let {
            assertEquals(0.30f, it.lift, 1e-5f)
            assertEquals(-0.14f, it.pitch, 1e-5f)
        }
        assertEquals(0.3f * 0.30f, pose(HeadState.DONE, quietTime, tau = 0.54f).lift, 1e-4f) // the small second hop
        assertEquals(0f, pose(HeadState.DONE, quietTime, tau = 1.0f).lift, 0f)
        assertEquals(0.46f * (1f - 0.075f / 0.9f).let { it * it }, pose(HeadState.ERROR, quietTime, tau = 0.075f).yaw, 1e-4f)
        assertTrue(pose(HeadState.ERROR, quietTime, tau = 0.225f).yaw < -0.2f) // the swing back
        assertEquals(0f, pose(HeadState.ERROR, quietTime, tau = 1.0f).yaw, 0f)
    }

    @Test
    fun `the first frame is simply there and a change of state eases on the device's spring`() {
        val m = HeadMotion()
        m.advance(HeadState.IDLE, ns(20.0))
        assertTrue(m.live)
        assertEquals(listOf(1f, 0f), listOf(m.strength, m.leaving))
        assertSame(pose(HeadState.IDLE, 20.0).values(), m.pose.values())

        m.advance(HeadState.IDLE, ns(20.5))
        val from = HeadPose().also { it.set(m.pose) }
        // The frame that notices the change still draws the head where it was.
        m.advance(HeadState.FETCHING, ns(20.5))
        assertSame(from.values(), m.pose.values())
        assertEquals(listOf(HeadState.FETCHING, HeadState.IDLE), listOf(m.state, m.prev))
        assertEquals(listOf(0f, 0f), listOf(m.strength, m.tau))

        // The spec's table for the device's blend: 54.3 % after 100 ms, 78.4 after 150, 92.8
        // after 200. The device, stepping, runs half a step (8 ms) ahead of the continuous
        // spring that matches it, so the same values come 8 ms later here.
        for ((ms, percent) in listOf(108 to 54.3f, 158 to 78.4f, 208 to 92.8f)) {
            m.advance(HeadState.FETCHING, ns(20.5) + ms * 1_000_000L)
            assertEquals("after $ms ms", percent / 100f, m.strength, 0.008f)
        }
        // It overshoots by two points around a third of a second: the head settling.
        m.advance(HeadState.FETCHING, ns(20.5 + 0.333))
        val to = pose(HeadState.FETCHING, 20.5 + 0.333)
        assertEquals(1f, m.strength, 0f) // a prop never grows past its size
        assertEquals(from.yaw + (to.yaw - from.yaw) * 1.021f, m.pose.yaw, 0.002f)
        assertTrue(m.pose.yaw > to.yaw)
        // And is exactly there, for good, from 0.93 s.
        m.advance(HeadState.FETCHING, ns(20.5 + 0.93))
        assertSame(pose(HeadState.FETCHING, 20.5 + 0.93).values(), m.pose.values())
        assertEquals(listOf(1f, 0f), listOf(m.strength, m.leaving))
    }

    @Test
    fun `a change of eye shape happens behind a blink`() {
        val m = HeadMotion()
        m.advance(HeadState.THINKING, ns(9.0))
        m.advance(HeadState.DONE, ns(9.0))
        m.advance(HeadState.DONE, ns(9.05))
        assertEquals(Eyes.OPEN, m.eyes) // still the old shape, closing
        assertTrue(m.pose.open < 0.7f)
        m.advance(HeadState.DONE, ns(9.1))
        assertTrue("shut at half way, was ${m.pose.open}", m.pose.open < 0.05f)
        m.advance(HeadState.DONE, ns(9.15))
        assertEquals(Eyes.HAPPY, m.eyes) // the new one, opening out of the line
        m.advance(HeadState.DONE, ns(9.3))
        assertEquals(1f, m.pose.open, 0.05f)
        // Between two states with the same eyes nothing shuts.
        val same = HeadMotion()
        same.advance(HeadState.IDLE, ns(quietTime))
        same.advance(HeadState.THINKING, ns(quietTime))
        same.advance(HeadState.THINKING, ns(quietTime + 0.1))
        assertEquals(1f, same.pose.open, 0f)
    }

    @Test
    fun `props grow in and an interrupted one fades from where it was`() {
        val m = HeadMotion()
        m.advance(HeadState.THINKING, ns(9.0))
        m.advance(HeadState.WORKING, ns(9.0))
        m.advance(HeadState.WORKING, ns(9.1))
        val grown = m.strength
        assertEquals(0.496f, grown, 0.01f)
        assertEquals(1f - grown, m.leaving, 1e-6f) // the moon, shrinking out as the keyboard grows
        // Changed again mid-blend: the keyboard was half grown and shrinks from half, not from full.
        m.advance(HeadState.FETCHING, ns(9.1))
        assertEquals(listOf(HeadState.FETCHING, HeadState.WORKING), listOf(m.state, m.prev))
        assertEquals(grown, m.leaving, 1e-6f)
        m.advance(HeadState.FETCHING, ns(9.2))
        assertEquals(grown * (1f - m.strength), m.leaving, 1e-6f)
        m.advance(HeadState.FETCHING, ns(10.2))
        assertEquals(0f, m.leaving, 0f)
    }

    @Test
    fun `a state without an accent borrows the one just left while its glow fades`() {
        val m = HeadMotion()
        m.advance(HeadState.THINKING, ns(quietTime))
        assertEquals(Accent.BLUE, m.accent)
        m.advance(HeadState.IDLE, ns(quietTime))
        m.advance(HeadState.IDLE, ns(quietTime + 0.1))
        assertEquals(Accent.BLUE, m.accent)
        assertEquals(0.5f, m.pose.glow, 0.02f)
        // A state with an accent shows its own from the first frame: blue to amber at once.
        m.advance(HeadState.APPROVAL, ns(quietTime + 0.1))
        assertEquals(Accent.AMBER, m.accent)
        // After two changes through states without one, the leftover glow is simply gone.
        m.advance(HeadState.IDLE, ns(quietTime + 0.2))
        m.advance(HeadState.PAUSED, ns(quietTime + 0.25))
        assertEquals(Accent.NONE, m.accent)
    }

    @Test
    fun `with animations removed every state is one still and telling pose`() {
        val seen = HashSet<List<Any>>()
        for (state in HeadState.entries) {
            val m = HeadMotion()
            m.still(state)
            val first = m.pose.values()
            assertFalse(state.name, m.live)
            assertEquals(state.name, state, m.state)
            // The prop at full size, nothing fading, the eyes of the state itself.
            assertEquals(state.name, listOf(1f, 0f), listOf(m.strength, m.leaving))
            assertEquals(state.name, state.eyes, m.eyes)
            // Never caught mid-blink or mid-glance: open, unless the state narrows its eyes.
            assertEquals(state.name, if (state == HeadState.WORKING) 0.66f else 1f, m.pose.open, 1e-6f)
            assertTrue(state.name, abs(m.pose.yaw) < 0.6f)
            // The same picture every time it is asked for, on any instance.
            m.still(state)
            assertSame(first, m.pose.values(), state.name)
            assertSame(first, HeadMotion().also { it.still(state) }.pose.values(), state.name)
            assertTrue("${state.name} looks like another state", seen.add(first + m.eyes + m.accent))
        }
        // The moments chosen: a hand on the keyboard, the cursor on, the pause mark solid,
        // done at the top of its hop with the rays still out, error facing front.
        assertTrue(phase(HeadMotion.stillTime(HeadState.WORKING), BEAT_STEP) < 0.2f)
        assertTrue(HeadMotion().also { it.still(HeadState.COMMAND) }.cursor)
        assertTrue(phase(HeadMotion.stillTime(HeadState.PAUSED), 1.6) < 0.7f)
        HeadMotion().also { it.still(HeadState.DONE) }.let {
            assertEquals(0.30f, it.pose.lift, 1e-4f)
            assertTrue(it.tau < 0.7f)
        }
        assertEquals(0f, HeadMotion().also { it.still(HeadState.ERROR) }.pose.yaw, 0f)
    }

    @Test
    fun `a still head snaps between states and eases again once frames come back`() {
        val m = HeadMotion()
        m.advance(HeadState.WORKING, ns(9.0))
        m.advance(HeadState.FETCHING, ns(9.1))
        m.advance(HeadState.FETCHING, ns(9.15)) // mid-easing when animations are turned off
        m.still(HeadState.ERROR)
        assertFalse(m.live)
        assertSame(HeadMotion().also { it.still(HeadState.ERROR) }.pose.values(), m.pose.values())
        assertEquals(listOf(1f, 0f), listOf(m.strength, m.leaving))
        m.still(HeadState.IDLE)
        assertEquals(listOf(1f, 0f), listOf(m.strength, m.leaving))
        assertEquals(Accent.NONE, m.accent) // nothing borrowed: there was no easing to borrow it for
        // Turned back on: the first frame is simply there, the next change eases.
        m.advance(HeadState.THINKING, ns(30.0))
        assertEquals(listOf(1f, 0f), listOf(m.strength, m.leaving))
        m.advance(HeadState.WORKING, ns(30.1))
        assertEquals(0f, m.strength, 0f)
    }

    @Test
    fun `only a head that barely moves asks for the slow frame rate`() {
        val m = HeadMotion()
        m.advance(HeadState.IDLE, ns(quietTime))
        assertTrue(m.quiet)
        // The full rate is back before a blink (4.08 s) or a glance turn (5.58 s) begins.
        m.advance(HeadState.IDLE, ns(4.0816 - 0.1))
        assertFalse(m.quiet)
        m.advance(HeadState.IDLE, ns(4.0816 + 0.1))
        assertFalse(m.quiet)
        m.advance(HeadState.IDLE, ns(5.5836 + 0.2))
        assertFalse(m.quiet)
        m.advance(HeadState.IDLE, ns(5.5836 + 1.0)) // holding the glance
        assertTrue(m.quiet)
        // Not while easing, and not in any state that acts something out.
        m.advance(HeadState.PAUSED, ns(quietTime))
        m.advance(HeadState.PAUSED, ns(quietTime + 0.1))
        assertFalse(m.quiet)
        m.advance(HeadState.PAUSED, ns(quietTime + 2.0))
        assertTrue(m.quiet)
        for (state in HeadState.entries - setOf(HeadState.IDLE, HeadState.PAUSED)) {
            assertFalse(state.name, HeadMotion().also { it.advance(state, ns(quietTime)) }.quiet)
        }
    }

    @Test
    fun `the clock keeps its resolution after weeks of uptime`() {
        // Frame times are nanoseconds since boot: a float of seconds would stop telling
        // 60 FPS frames apart after three days.
        val boot = 40L * 24 * 3600 * 1_000_000_000L
        val a = HeadMotion().also { it.advance(HeadState.WORKING, boot) }
        val b = HeadMotion().also { it.advance(HeadState.WORKING, boot + 16_666_667L) }
        assertNotEquals(a.pose.pitch, b.pose.pitch)
        assertEquals(0.14f * (1f - (1f - 0.0333333f).let { it * it }), abs(a.pose.pitch - b.pose.pitch), 1e-3f)
    }
}
