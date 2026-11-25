package dev.lapiszero.app.ui.head

import androidx.compose.animation.core.withInfiniteAnimationFrameNanos
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.MotionDurationScale
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.lapiszero.app.domain.HeadState
import dev.lapiszero.app.theme.lapis
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest

/** What a screen reader says for the head, and what the tests look for. */
fun headDescription(state: HeadState): String = "Lapis agent: ${state.label.lowercase()}"

/**
 * The agent's head: the character on the CardputerZero's screen, acting out `state` the way it
 * does there (see [HeadMotion] for the motion, [HeadPainter] for the drawing).
 *
 * `size` is the height of the figure. It is 1.4 times as wide, with the head in the middle:
 * that is the room its props need (a speech bubble, packets flying in, a keyboard under it).
 * Made for 28 dp, beside a line of text, up to 160 dp as a hero.
 *
 * One frame clock moves everything. It stops when the head is scrolled out of the window or
 * leaves the composition, and it never starts when the system's "remove animations" setting is
 * on: the head then holds one telling pose per state.
 */
@Composable
fun LapisHead(state: HeadState, modifier: Modifier = Modifier, size: Dp = 48.dp) =
    LapisHead(state, modifier, size, remember { HeadMotion() })

/** The same, on a [HeadMotion] the caller can look into: that is what its tests do. */
@Composable
internal fun LapisHead(state: HeadState, modifier: Modifier = Modifier, size: Dp = 48.dp, motion: HeadMotion) {
    val colors = headColors()
    val painter = remember(colors) { HeadPainter(colors) }
    val target by rememberUpdatedState(state)
    var frame by remember { mutableLongStateOf(NO_FRAME) }
    var onScreen by remember { mutableStateOf(true) }

    LaunchedEffect(onScreen) {
        frame = NO_FRAME
        if (!onScreen) return@LaunchedEffect
        // 0 while the system setting "remove animations" is on, and it can change under us.
        val scale = coroutineContext[MotionDurationScale]
        snapshotFlow { (scale?.scaleFactor ?: 1f) > 0f }.collectLatest { animate ->
            frame = NO_FRAME
            while (animate) {
                withInfiniteAnimationFrameNanos { nanos ->
                    motion.advance(target, nanos)
                    frame = nanos
                }
                // An idle head only sways: no need to redraw it at the display's rate.
                if (motion.quiet) delay(HeadMotion.QUIET_FRAME_MS)
            }
        }
    }

    Canvas(
        modifier = modifier
            .size(width = size * (HeadPainter.ZONE_WIDTH / HeadPainter.ZONE_HEIGHT), height = size)
            // Scrolled out of the window (a clipped parent leaves nothing of it): stop the clock.
            .onGloballyPositioned { onScreen = !it.boundsInWindow().isEmpty },
        contentDescription = headDescription(state),
    ) {
        // Reading `frame` is what redraws the head on every tick of the clock, and nothing else.
        if (frame == NO_FRAME) motion.still(state)
        drawIntoCanvas { painter.draw(it, this.size.width, this.size.height, motion) }
    }
}

/**
 * The head itself is paper and ink in both themes, as on the device; the room around it
 * (contour, shadow, props) follows the theme. The accents are the two identity colours.
 */
@Composable
internal fun headColors(): HeadColors {
    val scheme = MaterialTheme.colorScheme
    val extras = lapis
    return HeadColors(
        hi = extras.paper,
        lo = extras.ink,
        fg = scheme.onBackground,
        bg = scheme.background,
        blue = scheme.primary,
        amber = extras.amber,
    )
}

private const val NO_FRAME = Long.MIN_VALUE
