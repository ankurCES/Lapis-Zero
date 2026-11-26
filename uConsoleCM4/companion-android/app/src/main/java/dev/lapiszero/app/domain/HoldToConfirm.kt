package dev.lapiszero.app.domain

/**
 * Timing of the hold-to-confirm gesture that a Tier 2 approval needs (SPEC 5.4: destructive
 * commands, RF transmit, GPIO rails, ...). Pure, so the rule is unit-tested; the approve control
 * feeds it pointer and frame times.
 *
 * The confirmation fires once, when the press has lasted [holdMs]. Letting go earlier starts
 * over from zero; a confirmed gate stays confirmed (the decision is on its way).
 */
class HoldToConfirm(val holdMs: Long) {
    init {
        require(holdMs > 0) { "holdMs must be positive" }
    }

    private var downAtMs = NOT_PRESSED

    var confirmed: Boolean = false
        private set

    val holding: Boolean get() = downAtMs != NOT_PRESSED && !confirmed

    fun down(nowMs: Long) {
        if (!confirmed && downAtMs == NOT_PRESSED) downAtMs = nowMs
    }

    /**
     * Called on every frame while pressed. Returns true exactly once: on the first call at which
     * the press has lasted [holdMs].
     */
    fun tick(nowMs: Long): Boolean {
        if (confirmed || downAtMs == NOT_PRESSED) return false
        if (nowMs - downAtMs < holdMs) return false
        confirmed = true
        return true
    }

    /** The finger left. Returns true when that release completes the hold (no frame came in time). */
    fun up(nowMs: Long): Boolean {
        val done = tick(nowMs)
        if (!confirmed) downAtMs = NOT_PRESSED
        return done
    }

    /** The press was taken by something else (a scroll, the sheet being dragged): start over. */
    fun cancel() {
        if (!confirmed) downAtMs = NOT_PRESSED
    }

    /** 0..1 for the fill of the control. */
    fun progress(nowMs: Long): Float = when {
        confirmed -> 1f
        downAtMs == NOT_PRESSED -> 0f
        else -> ((nowMs - downAtMs).toFloat() / holdMs).coerceIn(0f, 1f)
    }

    private companion object {
        const val NOT_PRESSED = Long.MIN_VALUE
    }
}
