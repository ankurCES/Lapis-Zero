package dev.lapiszero.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.domain.ScreenshotStore
import dev.lapiszero.app.notifications.ApprovalNotifier
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis

/**
 * One pending high-risk action: what, where, the 1-bit preview the device shows, the
 * full-colour frame with a crosshair on the click point, and the three decisions.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun ApprovalCard(
    approval: Approval,
    shots: ScreenshotStore,
    busy: Boolean,
    onDecide: (decision: String, reason: String?) -> Unit,
    modifier: Modifier = Modifier,
) {
    var note by rememberSaveable(approval.requestId) { mutableStateOf("") }
    var viewer by rememberSaveable(approval.requestId) { mutableStateOf(false) }
    val click = approval.clickX?.let { x -> approval.clickY?.let { y -> x to y } }

    LapisCard(modifier.fillMaxWidth(), borderColor = lapis.amber) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            LapisBadge("approval", color = lapis.amber, filled = true)
            LapisBadge(ApprovalNotifier.actionLabel(approval.action))
            LapisBadge(approval.host, color = lapis.blue)
            LapisBadge("risk ${approval.risk}", color = if (approval.risk == "high") lapis.danger else lapis.muted)
            LapisBadge("step ${approval.step}", color = lapis.muted)
        }
        Spacer(Modifier.height(8.dp))
        Text(approval.summary, style = LapisType.Mono)
        if (click != null) {
            Text("target (${click.first}, ${click.second})", style = LapisType.MonoSmall, color = lapis.muted)
        }

        if (approval.preview1BitB64 != null || approval.screenshotId != null) {
            Spacer(Modifier.height(10.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.Top) {
                if (approval.preview1BitB64 != null) {
                    Column(Modifier.weight(1f)) {
                        Preview1BitImage(approval.preview1BitB64, Modifier.fillMaxWidth())
                        Text("device preview 120x68", style = LapisType.MonoSmall, color = lapis.muted)
                    }
                }
                approval.screenshotId?.let { id ->
                    Column(Modifier.weight(1f)) {
                        when (val shot = rememberShot(shots, id)) {
                            is ShotUi.Ready -> ScreenshotCanvas(
                                shot = shot.shot,
                                crosshair = click,
                                modifier = Modifier.clickable(onClickLabel = "Enlarge screenshot") { viewer = true },
                            )

                            is ShotUi.Failed -> Text(shot.message, style = LapisType.MonoSmall, color = lapis.muted)
                            else -> CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        }
                        Text("target screen · tap to enlarge", style = LapisType.MonoSmall, color = lapis.muted)
                    }
                }
            }
        }

        Spacer(Modifier.height(10.dp))
        OutlinedTextField(
            value = note,
            onValueChange = { note = it.take(200) },
            label = { Text("Note for the agent if you deny (optional)") },
            singleLine = true,
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(10.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Button(onClick = { onDecide(Decision.APPROVE_ONCE, null) }, enabled = !busy) {
                Text("Approve once")
            }
            OutlinedButton(onClick = { onDecide(Decision.APPROVE_SESSION, null) }, enabled = !busy) {
                Text("Approve session")
            }
            OutlinedButton(
                onClick = { onDecide(Decision.DENY, note.trim().ifEmpty { null }) },
                enabled = !busy,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            ) { Text("Deny") }
            if (busy) {
                Spacer(Modifier.width(4.dp))
                CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            }
        }
    }

    if (viewer) {
        approval.screenshotId?.let { id ->
            ScreenshotDialog(shots, id, onDismiss = { viewer = false }, crosshair = click)
        }
    }
}
