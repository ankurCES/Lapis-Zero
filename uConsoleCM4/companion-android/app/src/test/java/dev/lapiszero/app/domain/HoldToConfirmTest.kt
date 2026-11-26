package dev.lapiszero.app.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The Tier 2 rule: only a press that lasts the whole hold approves, and only once. */
class HoldToConfirmTest {
    @Test
    fun `a tap never confirms`() {
        val gate = HoldToConfirm(1500)
        gate.down(1_000)
        assertFalse(gate.tick(1_016))
        assertFalse(gate.up(1_120))
        assertFalse(gate.confirmed)
        assertEquals(0f, gate.progress(1_200))
    }

    @Test
    fun `holding for the full time confirms exactly once`() {
        val gate = HoldToConfirm(1500)
        gate.down(0)
        var fired = 0
        var t = 0L
        while (t <= 2_000) {
            if (gate.tick(t)) fired++
            t += 16
        }
        assertEquals(1, fired)
        assertTrue(gate.confirmed)
        assertFalse("release after confirming does not fire again", gate.up(2_100))
        assertEquals(1f, gate.progress(2_100))
    }

    @Test
    fun `the threshold is inclusive and one millisecond short is not enough`() {
        val short = HoldToConfirm(1500)
        short.down(100)
        assertFalse(short.tick(1_599))
        assertFalse(short.up(1_599))
        val exact = HoldToConfirm(1500)
        exact.down(100)
        assertTrue(exact.tick(1_600))
    }

    @Test
    fun `releasing early starts over from zero`() {
        val gate = HoldToConfirm(1500)
        gate.down(0)
        assertFalse(gate.tick(1_400))
        assertEquals(1400f / 1500f, gate.progress(1_400), 1e-6f)
        assertFalse(gate.up(1_400))
        assertFalse(gate.holding)
        // A second press needs the full hold again, not the 100 ms left over.
        gate.down(2_000)
        assertFalse(gate.tick(2_200))
        assertTrue(gate.tick(3_500))
    }

    @Test
    fun `a release that arrives before the frame still completes a full hold`() {
        val gate = HoldToConfirm(1500)
        gate.down(0)
        assertFalse(gate.tick(1_490))
        assertTrue(gate.up(1_510))
        assertTrue(gate.confirmed)
    }

    @Test
    fun `a cancelled press does not count`() {
        val gate = HoldToConfirm(1500)
        gate.down(0)
        gate.cancel()
        assertFalse(gate.tick(5_000))
        assertFalse(gate.confirmed)
        assertEquals(0f, gate.progress(5_000))
    }

    @Test
    fun `a second down while pressed keeps the first press time`() {
        val gate = HoldToConfirm(1500)
        gate.down(0)
        gate.down(1_000) // a second pointer, or a repeated event
        assertTrue(gate.tick(1_500))
    }

    @Test(expected = IllegalArgumentException::class)
    fun `a zero hold is refused`() {
        HoldToConfirm(0)
    }
}
