package dev.lapiszero.app.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.lapiszero.app.domain.HoldToConfirm
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import java.util.Locale

/**
 * An approve control that only fires after being held for [holdMs] (Tier 2). A tap does nothing
 * but say how long to hold; letting go early empties it again. Timing is [HoldToConfirm]'s.
 */
@Composable
fun HoldToConfirmButton(
    label: String,
    holdMs: Long,
    enabled: Boolean,
    onConfirmed: () -> Unit,
    modifier: Modifier = Modifier,
    color: Color = lapis.danger,
) {
    val gate = remember(holdMs) { HoldToConfirm(holdMs) }
    var pressed by remember { mutableStateOf(false) }
    var progress by remember { mutableFloatStateOf(0f) }
    var hint by remember { mutableStateOf(false) }
    var approved by remember { mutableStateOf(false) }
    val onDone by rememberUpdatedState(onConfirmed)
    val confirm = {
        approved = true
        onDone()
    }
    val seconds = String.format(Locale.US, "%.1f", holdMs / 1000.0)

    // Frame clock, not wall clock: the fill and the decision follow the same frames.
    LaunchedEffect(pressed, gate) {
        if (!pressed) {
            if (gate.up(withFrameMillis { it })) confirm()
            progress = gate.progress(0)
            return@LaunchedEffect
        }
        gate.down(withFrameMillis { it })
        while (true) {
            val now = withFrameMillis { it }
            progress = gate.progress(now)
            if (gate.tick(now)) {
                confirm()
                break
            }
        }
    }

    val shape = MaterialTheme.shapes.small
    Box(
        modifier
            .heightIn(min = 44.dp)
            .clip(shape)
            .border(1.dp, if (enabled) color else MaterialTheme.colorScheme.outlineVariant, shape)
            .semantics {
                role = Role.Button
                contentDescription = "$label: hold for $seconds seconds"
                if (!enabled) disabled()
            }
            .pointerInput(enabled, gate) {
                if (!enabled) return@pointerInput
                detectTapGestures(
                    onPress = {
                        hint = false
                        pressed = true
                        val released = tryAwaitRelease()
                        if (!released) gate.cancel()
                        pressed = false
                    },
                    onTap = { if (!approved) hint = true },
                )
            },
        contentAlignment = Alignment.CenterStart,
    ) {
        Box(
            Modifier
                .fillMaxHeight()
                .fillMaxWidth(progress)
                .background(color.copy(alpha = 0.35f)),
        )
        Text(
            when {
                approved -> "Approved"
                hint -> "Hold for $seconds s to approve"
                pressed -> "Keep holding…"
                else -> label
            },
            style = LapisType.MonoLabel,
            color = if (enabled) color else lapis.muted,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
        )
    }
}
