package dev.lapiszero.app.ui.kvm

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.data.remote.Activity
import dev.lapiszero.app.data.remote.AgentState
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.ScreenshotCanvas
import dev.lapiszero.app.ui.components.ScreenshotDialog
import dev.lapiszero.app.ui.components.SectionTitle
import dev.lapiszero.app.ui.components.ShotUi
import dev.lapiszero.app.ui.components.StatusDot
import dev.lapiszero.app.ui.components.formatClock
import dev.lapiszero.app.ui.components.kindLabel
import dev.lapiszero.app.ui.components.rememberShot

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun KvmScreen(session: LapisSession, snackbar: SnackbarHostState) {
    val vm: KvmViewModel = viewModel(factory = viewModelFactory { initializer { KvmViewModel(session.repo) } })
    val ui by vm.ui.collectAsStateWithLifecycle()
    val busy by vm.busy.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)

    var picked by remember { mutableStateOf<Pair<Int, Int>?>(null) }
    var viewer by rememberSaveable { mutableStateOf<Long?>(null) }
    var typeText by rememberSaveable { mutableStateOf("") }
    var keyCombo by rememberSaveable { mutableStateOf("") }
    if (!ui.canNudge && picked != null) picked = null

    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item(key = "status") {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                LapisBadge(
                    ui.agentState.replace('_', ' '),
                    color = when (ui.agentState) {
                        AgentState.IDLE -> lapis.muted
                        AgentState.WAITING_APPROVAL, AgentState.PAUSED -> lapis.amber
                        else -> lapis.blue
                    },
                    filled = ui.running,
                )
                if (ui.activeHost.isNotEmpty()) LapisBadge(ui.activeHost, color = lapis.blue)
                if (ui.running) LapisBadge("step ${ui.step}/${ui.maxSteps}", color = lapis.muted)
            }
            ui.taskTitle?.takeIf { it.isNotBlank() }?.let {
                Text(it, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }

        item(key = "viewport") {
            val shot = rememberShot(session.shots, ui.screenshotId)
            Surface(
                border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outline),
                shape = MaterialTheme.shapes.small,
            ) {
                when (shot) {
                    is ShotUi.Ready -> ScreenshotCanvas(
                        shot = shot.shot,
                        markers = ui.markers,
                        crosshair = picked,
                        zoomable = true,
                        onTap = if (ui.canNudge) ({ x, y -> picked = x to y }) else null,
                    )

                    else -> Box(
                        Modifier.fillMaxWidth().aspectRatio(16f / 9f),
                        contentAlignment = Alignment.Center,
                    ) {
                        when (shot) {
                            is ShotUi.Failed -> Text(shot.message, style = LapisType.Mono, color = lapis.muted)
                            ShotUi.Loading -> CircularProgressIndicator()
                            else -> Text(
                                "No frame yet. The target screen appears here once the agent takes a screenshot.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = lapis.muted,
                                modifier = Modifier.padding(24.dp),
                            )
                        }
                    }
                }
            }
            Text(
                ui.screenshotId?.let { "frame #$it · pinch to zoom · markers show recent clicks" } ?: "",
                style = LapisType.MonoSmall,
                color = lapis.muted,
            )
        }

        item(key = "assist") {
            LapisCard(Modifier.fillMaxWidth(), borderColor = if (ui.canNudge) lapis.amber else MaterialTheme.colorScheme.outlineVariant) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Manual assist", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                    when {
                        ui.agentState == AgentState.PAUSED ->
                            Button(onClick = vm::resume, enabled = !busy) { Text("Hand back to agent") }

                        ui.running && ui.agentState != AgentState.WAITING_APPROVAL ->
                            OutlinedButton(onClick = vm::pause, enabled = !busy) { Text("Take control") }
                    }
                }
                Text(
                    when {
                        ui.canNudge && ui.agentState == AgentState.WAITING_APPROVAL ->
                            "The agent is waiting for an approval. You can click, type or press keys on ${ui.activeHost} meanwhile."

                        ui.canNudge -> "The agent is paused. Tap the frame to pick a point, or type below."
                        ui.running -> "Available only while the agent is paused or waiting for approval. Take control first."
                        else -> "Available only while a task is paused or waiting for approval."
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
                Spacer(Modifier.height(8.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        picked?.let { "(${it.first}, ${it.second})" } ?: "tap the frame",
                        style = LapisType.Mono,
                        modifier = Modifier.weight(1f),
                    )
                    Button(
                        onClick = {
                            picked?.let { vm.nudgeClick(it.first, it.second) }
                            picked = null
                        },
                        enabled = ui.canNudge && picked != null && !busy,
                    ) { Text("Click here") }
                }
                Spacer(Modifier.height(6.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(
                        value = typeText,
                        onValueChange = { typeText = it },
                        label = { Text("Text to type") },
                        singleLine = true,
                        enabled = ui.canNudge,
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    Button(
                        onClick = {
                            vm.nudgeType(typeText)
                            typeText = ""
                        },
                        enabled = ui.canNudge && typeText.isNotEmpty() && !busy,
                    ) { Text("Type") }
                }
                Spacer(Modifier.height(6.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(
                        value = keyCombo,
                        onValueChange = { keyCombo = it },
                        label = { Text("Key combo, e.g. ctrl+c") },
                        singleLine = true,
                        enabled = ui.canNudge,
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    Button(
                        onClick = {
                            vm.nudgeKey(keyCombo)
                            keyCombo = ""
                        },
                        enabled = ui.canNudge && keyCombo.isNotBlank() && !busy,
                    ) { Text("Send key") }
                }
                Spacer(Modifier.height(6.dp))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    listOf("Return", "Escape", "Tab", "BackSpace").forEach { key ->
                        LapisBadge(key, onClick = if (ui.canNudge && !busy) ({ vm.nudgeKey(key) }) else null,
                            color = if (ui.canNudge) MaterialTheme.colorScheme.onSurface else lapis.muted)
                    }
                }
            }
        }

        item(key = "ticker-title") { SectionTitle("Step ticker") }
        if (ui.ticker.isEmpty()) {
            item(key = "ticker-empty") {
                Text("No steps yet.", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
            }
        }
        itemsIndexed(ui.ticker) { _, activity ->
            TickerRow(activity, onOpenShot = { viewer = it })
        }
    }

    viewer?.let { id -> ScreenshotDialog(session.shots, id, onDismiss = { viewer = null }) }
}

@Composable
private fun TickerRow(activity: Activity, onOpenShot: (Long) -> Unit) {
    val shot = activity.screenshotId
    Row(
        Modifier
            .fillMaxWidth()
            .then(if (shot != null) Modifier.clickable(onClickLabel = "Open frame") { onOpenShot(shot) } else Modifier)
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(
            when (activity.status) {
                "ok" -> lapis.blue
                "denied" -> lapis.amber
                else -> lapis.danger
            },
            size = 8.dp,
        )
        Spacer(Modifier.width(8.dp))
        LapisBadge(kindLabel(activity.kind))
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Text(activity.summary, style = LapisType.Mono, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (activity.detail.isNotBlank()) {
                Text(
                    activity.detail,
                    style = LapisType.MonoSmall,
                    color = lapis.muted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Column(horizontalAlignment = Alignment.End) {
            Text("#${activity.step}", style = LapisType.MonoSmall, color = lapis.muted)
            Text(formatClock(activity.ts), style = LapisType.MonoSmall, color = lapis.muted)
        }
        if (shot != null) {
            Spacer(Modifier.width(6.dp))
            Text("▣", color = lapis.blue)
        }
    }
}
