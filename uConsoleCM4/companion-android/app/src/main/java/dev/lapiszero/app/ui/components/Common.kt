package dev.lapiszero.app.ui.components

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.lapiszero.app.data.remote.Activity
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.flow.Flow

/** Small monospace tag: tool kind, host, policy. */
@Composable
fun LapisBadge(
    text: String,
    modifier: Modifier = Modifier,
    color: Color = MaterialTheme.colorScheme.onSurface,
    filled: Boolean = false,
    onClick: (() -> Unit)? = null,
) {
    val shape = MaterialTheme.shapes.extraSmall
    val content = if (filled) contentOn(color) else color
    // A plain Surface (not the clickable overload): tags must not grow to the 48 dp touch minimum.
    Surface(
        modifier = modifier.then(
            if (onClick != null) Modifier.clip(shape).clickable(onClick = onClick) else Modifier,
        ),
        shape = shape,
        color = if (filled) color else Color.Transparent,
        contentColor = content,
        border = BorderStroke(1.dp, color),
    ) {
        Text(
            text = text.uppercase(Locale.ROOT),
            style = LapisType.MonoLabel,
            color = content,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            // Tappable tags get a little more body so they are comfortable to hit.
            modifier = Modifier.padding(
                horizontal = if (onClick != null) 10.dp else 6.dp,
                vertical = if (onClick != null) 7.dp else 2.dp,
            ),
        )
    }
}

@Composable
private fun contentOn(color: Color): Color {
    val c = lapis
    return when (color) {
        c.amber -> c.ink
        MaterialTheme.colorScheme.onSurface -> MaterialTheme.colorScheme.surface
        else -> c.onAccent
    }
}

@Composable
fun StatusDot(color: Color, modifier: Modifier = Modifier, pulsing: Boolean = false, size: Dp = 10.dp) {
    val alpha = if (pulsing) {
        val transition = rememberInfiniteTransition(label = "pulse")
        val value by transition.animateFloat(
            initialValue = 1f,
            targetValue = 0.25f,
            animationSpec = infiniteRepeatable(tween(700), RepeatMode.Reverse),
            label = "pulseAlpha",
        )
        value
    } else {
        1f
    }
    Box(modifier.size(size).alpha(alpha).clip(CircleShape).background(color))
}

/** Outlined, flat card: the paper/ink look has borders instead of shadows. */
@Composable
fun LapisCard(
    modifier: Modifier = Modifier,
    borderColor: Color = MaterialTheme.colorScheme.outlineVariant,
    onClick: (() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val shape = MaterialTheme.shapes.medium
    if (onClick != null) {
        Surface(
            onClick = onClick,
            modifier = modifier,
            shape = shape,
            color = MaterialTheme.colorScheme.surfaceContainerLow,
            border = BorderStroke(1.dp, borderColor),
        ) { Column(Modifier.padding(12.dp), content = content) }
    } else {
        Surface(
            modifier = modifier,
            shape = shape,
            color = MaterialTheme.colorScheme.surfaceContainerLow,
            border = BorderStroke(1.dp, borderColor),
        ) { Column(Modifier.padding(12.dp), content = content) }
    }
}

@Composable
fun SectionTitle(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text.uppercase(Locale.ROOT),
        style = LapisType.MonoLabel,
        color = lapis.muted,
        modifier = modifier.padding(top = 8.dp, bottom = 4.dp),
    )
}

@Composable
fun KeyValueRow(label: String, value: String, modifier: Modifier = Modifier, valueColor: Color = Color.Unspecified) {
    Row(modifier.fillMaxWidth().padding(vertical = 2.dp), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label, style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
        Spacer(Modifier.width(12.dp))
        Text(value, style = LapisType.Mono, color = valueColor, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
fun LoadingRow(text: String, modifier: Modifier = Modifier) {
    Row(modifier.padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Spacer(Modifier.width(12.dp))
        Text(text, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
fun ErrorRetry(text: String, modifier: Modifier = Modifier, onRetry: (() -> Unit)? = null) {
    Row(
        modifier
            .fillMaxWidth()
            .border(1.dp, MaterialTheme.colorScheme.error, MaterialTheme.shapes.small)
            .padding(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.weight(1f),
        )
        if (onRetry != null) TextButton(onClick = onRetry) { Text("Retry") }
    }
}

@Composable
fun ConfirmDialog(
    title: String,
    text: String,
    confirmLabel: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    danger: Boolean = false,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = {
            Button(
                onClick = {
                    onConfirm()
                    onDismiss()
                },
                colors = if (danger) {
                    ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError,
                    )
                } else {
                    ButtonDefaults.buttonColors()
                },
            ) { Text(confirmLabel) }
        },
        dismissButton = { OutlinedButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** A badge that opens a menu of choices (host switcher, policy picker). */
@Composable
fun <T> ChoiceBadge(
    label: String,
    options: List<T>,
    optionLabel: (T) -> String,
    onPick: (T) -> Unit,
    modifier: Modifier = Modifier,
    color: Color = MaterialTheme.colorScheme.onSurface,
    enabled: Boolean = true,
) {
    var open by remember { mutableStateOf(false) }
    Box(modifier) {
        LapisBadge("$label ▾", color = color, onClick = if (enabled) ({ open = true }) else null)
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(optionLabel(option), style = LapisType.Mono) },
                    onClick = {
                        open = false
                        onPick(option)
                    },
                )
            }
        }
    }
}

/** Shows every string emitted by a ViewModel's `messages` flow as a snackbar. */
@Composable
fun CollectMessages(messages: Flow<String>, snackbar: SnackbarHostState) {
    LaunchedEffect(messages, snackbar) {
        messages.collect { snackbar.showSnackbar(it) }
    }
}

@Composable
fun connectionColor(state: ConnectionState): Color = when (state) {
    ConnectionState.ONLINE -> lapis.blue
    ConnectionState.CONNECTING -> lapis.amber
    ConnectionState.OFFLINE -> lapis.muted
    ConnectionState.UNAUTHORIZED -> lapis.danger
}

fun connectionLabel(state: ConnectionState): String = when (state) {
    ConnectionState.ONLINE -> "online"
    ConnectionState.CONNECTING -> "connecting"
    ConnectionState.OFFLINE -> "offline"
    ConnectionState.UNAUTHORIZED -> "unpaired"
}

/** `[VNC]`, `[SSH]`... for a tool-call kind (spec 5.4 Activity.kind). */
fun kindLabel(kind: String?): String = when (kind) {
    "ssh_exec" -> "SSH"
    "vnc_click", "vnc_move", "vnc_drag", "vnc_scroll" -> "VNC"
    "vnc_key" -> "KEY"
    "vnc_type", "type_text" -> "TYPE"
    "screenshot" -> "SHOT"
    "wait" -> "WAIT"
    "thought" -> "THINK"
    "prompt" -> "PROMPT"
    "nudge" -> "NUDGE"
    Activity.KIND_AGENT_RUN -> "AGENT" // a tool on the host (Claude Code, opencode, ...) at work
    null, "" -> "TOOL"
    else -> kind.take(8)
}

fun policyLabel(policy: String?): String = when (policy) {
    "strict" -> "Strict"
    "balanced" -> "Balanced"
    "autonomous" -> "Auto"
    null, "" -> "Host default"
    else -> policy
}

fun formatClock(unixSeconds: Long): String =
    if (unixSeconds <= 0) "" else SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(Date(unixSeconds * 1000))

fun formatDateTime(unixSeconds: Long?): String =
    if (unixSeconds == null || unixSeconds <= 0) {
        "never"
    } else {
        SimpleDateFormat("d MMM HH:mm", Locale.getDefault()).format(Date(unixSeconds * 1000))
    }

fun formatAgo(unixSeconds: Long, nowSeconds: Long = System.currentTimeMillis() / 1000): String {
    if (unixSeconds <= 0) return ""
    val delta = (nowSeconds - unixSeconds).coerceAtLeast(0)
    return when {
        delta < 60 -> "now"
        delta < 3600 -> "${delta / 60}m"
        delta < 86_400 -> "${delta / 3600}h"
        else -> "${delta / 86_400}d"
    }
}

fun formatUptime(seconds: Long): String {
    val d = seconds / 86_400
    val h = seconds % 86_400 / 3600
    val m = seconds % 3600 / 60
    return when {
        d > 0 -> "${d}d ${h}h"
        h > 0 -> "${h}h ${m}m"
        else -> "${m}m"
    }
}
