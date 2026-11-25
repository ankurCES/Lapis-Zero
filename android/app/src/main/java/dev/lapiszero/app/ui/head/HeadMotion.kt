package dev.lapiszero.app.ui.head

import androidx.compose.animation.core.FloatSpringSpec
import dev.lapiszero.app.domain.HeadState
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.sin

/*
 * How the head moves: every number here is the device's (`ui/src/gfx/mascot.rs`, written down
 * in the head spec, sections 6 to 8), so the phone and the CardputerZero act out a state the
 * same way. Angles are radians, lengths are head radii, times are seconds.
 */

internal enum class Eyes { OPEN, ASLEEP, HAPPY, CROSS, PROMPT }

internal enum class Accent { NONE, BLUE, AMBER }

/** Blue: the agent is at work. Amber: it needs the owner, or has failed. */
internal val HeadState.accent: Accent
    get() = when (this) {
        HeadState.IDLE, HeadState.LISTENING, HeadState.PAUSED -> Accent.NONE
        HeadState.APPROVAL, HeadState.ERROR -> Accent.AMBER
        else -> Accent.BLUE
    }

internal val HeadState.eyes: Eyes
    get() = when (this) {
        HeadState.DONE -> Eyes.HAPPY
        HeadState.ERROR -> Eyes.CROSS
        HeadState.PAUSED -> Eyes.ASLEEP
        HeadState.COMMAND -> Eyes.PROMPT
        else -> Eyes.OPEN
    }

/** The nine numbers that ease between states. */
internal class HeadPose {
    var yaw = 0f
    var pitch = 0f
    var roll = 0f
    var lift = 0f
    var lookX = 0f
    var lookY = 0f
    var open = 1f
    var wide = 1f
    var glow = 0f

    fun rest() {
        yaw = 0f; pitch = 0f; roll = 0f; lift = 0f; lookX = 0f; lookY = 0f
        open = 1f; wide = 1f; glow = 0f
    }

    fun set(o: HeadPose) {
        yaw = o.yaw; pitch = o.pitch; roll = o.roll; lift = o.lift; lookX = o.lookX; lookY = o.lookY
        open = o.open; wide = o.wide; glow = o.glow
    }

    /** `a + (b - a) * w`; `w` may overshoot a little, which is the head settling. */
    fun mix(a: HeadPose, b: HeadPose, w: Float) {
        yaw = a.yaw + (b.yaw - a.yaw) * w
        pitch = a.pitch + (b.pitch - a.pitch) * w
        roll = a.roll + (b.roll - a.roll) * w
        lift = a.lift + (b.lift - a.lift) * w
        lookX = a.lookX + (b.lookX - a.lookX) * w
        lookY = a.lookY + (b.lookY - a.lookY) * w
        open = a.open + (b.open - a.open) * w
        wide = a.wide + (b.wide - a.wide) * w
        glow = a.glow + (b.glow - a.glow) * w
    }
}

private const val TAU = (2 * PI).toFloat()

/** Position in a cycle of `period` seconds. The clock stays a double until it is reduced. */
internal fun phase(t: Double, period: Double): Float {
    val x = t / period
    return (x - floor(x)).toFloat()
}

internal fun wave(t: Double, period: Double): Float = sin(TAU * phase(t, period))

internal fun smooth(x: Float): Float {
    val c = x.coerceIn(0f, 1f)
    return c * c * (3f - 2f * c)
}

/** Height of a hop that starts at 0 and lands at 1. */
internal fun hop(x: Float): Float = if (x >= 0f && x < 1f) sin(PI.toFloat() * x) else 0f

/** The device's own hash, so a phone next to it blinks and glances in step with it. */
internal fun hash01(n: Int): Float {
    var x = (n * 0x9E3779B9.toInt()) xor 0x85EBCA6B.toInt()
    x = x xor (x ushr 15)
    x *= 0x2C1B3C6D
    x = x xor (x ushr 12)
    return (x ushr 8) / 16_777_216f
}

private const val BLINK_SLOT = 5.6
private const val BLINK_SECS = 0.24
private const val GLANCE_SLOT = 13.0
private const val GLANCE_SECS = 2.2
private const val GLANCE_TURN = 0.35

/** Eye opening from the blink schedule: one 240 ms blink in every 5.6 s slot. */
internal fun blink(t: Double): Float {
    val slot = floor(t / BLINK_SLOT)
    val start = (slot + 0.1 + 0.8 * hash01(slot.toInt())) * BLINK_SLOT
    val x = ((t - start) / BLINK_SECS).toFloat()
    return when {
        x < 0f || x >= 1f -> 1f
        x < 0.3f -> 1f - x / 0.3f
        x < 0.5f -> 0f
        else -> (x - 0.5f) / 0.5f
    }
}

/** The idle glance, -1..1: eased out to one side, held, eased back; one in every 13 s slot. */
internal fun glance(t: Double): Float {
    val slot = floor(t / GLANCE_SLOT)
    val n = slot.toInt()
    val start = slot * GLANCE_SLOT + 2.0 + hash01(n xor 0x5BD1) * (GLANCE_SLOT - GLANCE_SECS - 2.5)
    val side = if (hash01(n xor 0x1234) < 0.5f) -1f else 1f
    val x = t - start
    return side * smooth((x / GLANCE_TURN).toFloat()) * smooth(((GLANCE_SECS - x) / GLANCE_TURN).toFloat())
}

/**
 * Where `state` wants the head at clock time `t`, `tau` seconds into the state. `calm` leaves
 * the blinks and glances out: a still pose must not catch the head with its eyes shut.
 */
internal fun target(state: HeadState, t: Double, tau: Float, rms: Float, calm: Boolean, out: HeadPose) {
    out.rest()
    out.open = if (calm) 1f else blink(t)
    out.glow = if (state.accent == Accent.NONE) 0f else 1f
    when (state) {
        HeadState.IDLE -> {
            // Breathing, and a slow sway in depth so the face is never held dead-on for long.
            val breath = wave(t, 4.0)
            val g = if (calm) 0f else glance(t)
            out.yaw = 0.32f * g + 0.07f * wave(t, 9.0)
            out.pitch = 0.05f + 0.02f * breath
            out.roll = 0.025f * wave(t, 7.0)
            out.lift = 0.03f * breath
            out.lookX = 0.09f * g
        }

        HeadState.LISTENING -> {
            out.yaw = -0.10f
            out.pitch = -0.10f
            out.roll = 0.15f + 0.03f * wave(t, 3.2)
            out.lift = 0.03f + 0.06f * rms
            out.wide = 1.06f + 0.24f * rms
        }

        HeadState.THINKING -> {
            out.yaw = 0.24f * wave(t, 4.8)
            out.pitch = -0.22f
            out.roll = 0.07f * wave(t - 1.2, 4.8)
            out.lift = 0.03f * wave(t, 3.0)
            out.lookX = 0.10f * wave(t, 2.4)
            out.lookY = -0.11f
        }

        HeadState.WORKING -> {
            // One key every half second: a nod that lands on the beat, a turn toward the hand.
            val b = phase(t, BEAT_STEP)
            val nod = (1f - b) * (1f - b)
            out.yaw = -0.10f * wave(t, 2 * BEAT_STEP)
            out.pitch = 0.20f + 0.14f * nod
            out.lift = -0.02f - 0.07f * nod
            out.lookX = -0.05f * wave(t, 2 * BEAT_STEP)
            out.lookY = 0.10f
            out.open *= 0.66f
        }

        HeadState.FETCHING -> {
            // A gulp for each packet: the eyes widen as it arrives and ease back.
            val q = phase(t, PACKET_SECS)
            val gulp = if (q > 0.7f) smooth((q - 0.7f) / 0.3f) else max(1f - q / 0.35f, 0f)
            out.yaw = 0.52f + 0.04f * wave(t, 1.8)
            out.pitch = -0.06f
            out.roll = 0.03f
            out.lift = 0.02f * wave(t, 2.2)
            out.lookX = 0.10f
            out.lookY = -0.02f
            out.wide = 1f + 0.18f * gulp
        }

        HeadState.COMMAND -> {
            // Reading the output: a slow scan toward it and back, a nod on every third line.
            val nod = max(1f - phase(t, 3 * LINE_SECS) / 0.3f, 0f)
            out.yaw = 0.16f + 0.10f * wave(t, 3.6)
            out.pitch = 0.05f + 0.04f * nod * nod
            out.lift = 0.015f * wave(t, 2.4) - 0.02f * nod * nod
            out.open = 1f
        }

        HeadState.APPROVAL -> {
            // Two hops, then a wait, every 1.6 s.
            val q = phase(t, 1.6) * 1.6f
            out.pitch = -0.08f
            out.roll = 0.12f
            out.lift = 0.02f + 0.14f * hop(q / 0.28f) + 0.09f * hop((q - 0.28f) / 0.28f)
            out.wide = 1.18f
        }

        HeadState.PAUSED -> {
            // Asleep: sunk and tilted, rising and falling on a slow breath.
            val breath = wave(t, 6.0)
            out.yaw = -0.14f
            out.pitch = 0.26f + 0.03f * breath
            out.roll = -0.10f
            out.lift = -0.06f + 0.02f * breath
            out.open = 1f
        }

        HeadState.DONE -> {
            val h = hop(tau / 0.42f) + 0.3f * hop((tau - 0.42f) / 0.24f)
            out.pitch = -0.14f * h
            out.roll = 0.06f
            out.lift = 0.30f * h
            out.wide = 1.1f
            out.open = 1f
        }

        HeadState.ERROR -> {
            // A "no" that dies away over 0.9 s.
            val fade = max(1f - tau / 0.9f, 0f)
            out.yaw = 0.46f * fade * fade * sin(TAU * tau / 0.30f)
            out.pitch = 0.12f
            out.lift = -0.04f
            out.wide = 1.05f
            out.open = 1f
        }
    }
}

/** A hand strikes every this many seconds (working). */
internal const val BEAT_STEP = 0.5

/** A packet leaves the edge every this many seconds (fetching). */
internal const val PACKET_SECS = 0.6

/** A line of output arrives every this many seconds (command). */
internal const val LINE_SECS = 0.36

/** An outgoing prop no longer animates on `tau`: it is drawn as at the end of a beat. */
internal const val BEAT_SECS = 1.6f

/**
 * The head between frames: the state it is in, the easing from the one before, and everything
 * the painter reads. One instance per head on screen; nothing here allocates once it exists.
 *
 * [advance] is the animated path (one call per frame, from the frame clock). [still] is the
 * other one: a fixed, telling pose per state, for the system's "remove animations" setting and
 * for a head that has not been given a frame yet.
 */
internal class HeadMotion {
    /** The pose to draw. */
    val pose = HeadPose()

    var state = HeadState.IDLE
        private set

    /** The state being eased away from; its prop is still fading while [leaving] is above 0. */
    var prev = HeadState.IDLE
        private set

    /** Strength of [state]'s prop, 0..1: it grows in with the easing. */
    var strength = 1f
        private set

    /** Strength of [prev]'s prop, shrinking out. */
    var leaving = 0f
        private set

    /** The eye shape on show: it swaps half way through an easing, behind what looks like a blink. */
    var eyes = Eyes.OPEN
        private set

    /** The prompt's cursor (command): a block while true, an underline otherwise. */
    var cursor = true
        private set

    /** A state without an accent borrows the one just left while its glow fades out. */
    var accent = Accent.NONE
        private set

    /** Clock seconds: what the loops of the props run on. */
    var t = 0.0
        private set

    /** Seconds since [state] was entered: the clock of the Done and Error beats. */
    var tau = 0f
        private set

    /** Microphone level 0..1 (listening). The phone has no live level: a synthetic swell. */
    var rms = 0f
        private set

    /** True once [advance] has run: from then on a change of state eases. */
    var live = false
        private set

    /**
     * True while nothing on the head moves fast: asleep, or idle between its blinks and glances
     * (a slow sway is all there is). The frame clock then runs at [QUIET_FRAME_MS] instead of
     * the display's rate, as the device drops to a few frames a second for the same stretches.
     */
    var quiet = false
        private set

    private val from = HeadPose()
    private val to = HeadPose()
    private var prevStrength = 1f
    private var since = 0L
    private var blending = false

    /** Moves to frame time `nanos`, easing toward `target` if that is not the state already. */
    fun advance(target: HeadState, nanos: Long) {
        if (!live) {
            // The first frame: the head is simply there, in its state.
            live = true
            state = target
            prev = target
            since = nanos
            blending = false
        } else if (target != state) {
            // From wherever the head is now, and from however far the old prop had grown.
            from.set(pose)
            prevStrength = strength
            prev = state
            state = target
            since = nanos
            blending = true
        }
        val elapsed = nanos - since
        var w = 1f
        if (blending) {
            if (elapsed >= SETTLE_NANOS) blending = false else w = SPRING.getValueFromNanos(elapsed, 0f, 1f, 0f)
        }
        t = nanos / 1e9
        tau = elapsed / 1e9f
        // ponytail: no microphone level reaches the phone (and no daemon publishes LISTENING
        // yet), so listening always runs on the swell the device uses until its first level.
        val swell = 0.5f + 0.5f * sin(3.0 * t).toFloat()
        rms = swell * swell
        target(state, t, tau, rms, calm = false, to)
        settle(w)
        val ahead = t + 2 * QUIET_FRAME_MS / 1000.0
        quiet = !blending && when (state) {
            HeadState.PAUSED -> true
            // Looking a little ahead, so the full rate is back before a blink or a turn begins.
            HeadState.IDLE -> blink(t) == 1f && blink(ahead) == 1f && glance(t) == glance(ahead)
            else -> false
        }
    }

    /** The still pose of `state`: no clock, no easing, the prop at full size. */
    fun still(state: HeadState) {
        live = false
        this.state = state
        prev = state
        blending = false
        t = stillTime(state)
        tau = stillTau(state)
        rms = 0.55f
        target(state, t, tau, rms, calm = true, to)
        settle(1f)
    }

    private fun settle(w: Float) {
        val weight = w.coerceIn(0f, 1f)
        if (blending) pose.mix(from, to, w.coerceIn(-0.25f, 1.25f)) else pose.set(to)
        // A change of eye shape happens behind a blink: shut by half way, open again after it.
        val differ = blending && prev.eyes != state.eyes
        if (differ) pose.open *= abs(2f * weight - 1f)
        pose.open = pose.open.coerceIn(0f, 1.3f)
        pose.wide = pose.wide.coerceIn(0.5f, 1.5f)
        pose.glow = pose.glow.coerceIn(0f, 1f)
        eyes = if (differ && weight < 0.5f) prev.eyes else state.eyes
        cursor = phase(t, 1.0) < 0.55f
        accent = if (state.accent == Accent.NONE) prev.accent else state.accent
        strength = weight
        leaving = if (blending) prevStrength * (1f - weight) else 0f
    }

    companion object {
        /** The continuous spring that matches the device's stepped one (head spec, section 8). */
        private val SPRING = FloatSpringSpec(dampingRatio = 0.775f, stiffness = 217.5f, visibilityThreshold = 0.001f)

        /** The device sets its blend to exactly 1 at 0.93 s; so does this. */
        private const val SETTLE_NANOS = 930_000_000L

        /** About 15 frames a second while the head is [quiet]. */
        const val QUIET_FRAME_MS = 66L

        /**
         * One telling moment per state, the ones the device's own theme sheet shows: a hand on
         * the keyboard, the moon in front of the brow, a packet in flight, the pause mark solid.
         */
        internal fun stillTime(state: HeadState): Double = when (state) {
            HeadState.THINKING -> 7.5
            HeadState.WORKING -> 6.05
            HeadState.FETCHING -> 6.3
            HeadState.COMMAND -> 6.14
            HeadState.APPROVAL -> 7.0
            HeadState.PAUSED -> 6.6
            HeadState.LISTENING -> 6.18
            else -> 6.0
        }

        /** Done at the top of its hop with the rays out; Error facing front before the shake. */
        internal fun stillTau(state: HeadState): Float = if (state == HeadState.DONE) 0.21f else 0f
    }
}
