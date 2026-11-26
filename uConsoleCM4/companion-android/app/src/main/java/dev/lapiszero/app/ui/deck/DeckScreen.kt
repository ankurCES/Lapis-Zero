package dev.lapiszero.app.ui.deck

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.data.remote.AuditEntry
import dev.lapiszero.app.data.remote.Rail
import dev.lapiszero.app.data.remote.SdrSpectrum
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.DeckAlert
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.domain.formatMhz
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.ConfirmDialog
import dev.lapiszero.app.ui.components.KeyValueRow
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.SectionTitle
import dev.lapiszero.app.ui.components.connectionColor
import dev.lapiszero.app.ui.components.connectionLabel
import dev.lapiszero.app.ui.components.formatAgo
import dev.lapiszero.app.ui.components.formatClock
import java.util.Locale

/** The Lapis-Deck cockpit: only reachable when the paired device is a Lapis-Deck. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun DeckScreen(session: LapisSession, snackbar: SnackbarHostState) {
    val vm: DeckViewModel = viewModel(
        factory = viewModelFactory { initializer { DeckViewModel(session.repo, session.deck, session.api) } },
    )
    val ui by vm.ui.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    LifecycleResumeEffect(vm) {
        vm.onVisible(true)
        onPauseOrDispose { vm.onVisible(false) }
    }
    var confirmRail by remember { mutableStateOf<Pair<Rail, Boolean>?>(null) }

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            LapisBadge("lapis-deck", color = lapis.amber, filled = true)
            LapisBadge(connectionLabel(ui.connection), color = connectionColor(ui.connection), filled = ui.connection == ConnectionState.ONLINE)
            ui.harnessUp?.let { LapisBadge(if (it) "harness up" else "harness down", color = if (it) lapis.blue else lapis.danger) }
            ui.batteryW?.let { LapisBadge("draw ${formatWatts(it)}", color = lapis.muted) }
            if (ui.status?.mock == true) LapisBadge("mock hw", color = lapis.amber)
            if (ui.loading) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
        }
        ui.error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }

        SectionTitle("Power")
        PowerCard(ui, onToggle = { rail, on ->
            if (!on && rail.cutsPhoneLink) confirmRail = rail to on else vm.setRail(rail.name, on)
        })

        SectionTitle("GPS")
        GpsCard(ui)

        SectionTitle("Mesh")
        MeshCard(ui, onSend = vm::sendMesh)

        SectionTitle("SDR")
        SdrCard(ui, onTune = vm::tune, onRun = vm::run)

        SectionTitle("NetOps")
        LapisCard(Modifier.fillMaxWidth()) {
            QuickActions.NETOPS.forEach { action -> QuickActionBlock(action, ui, onRun = vm::run) }
        }

        SectionTitle("Live feed")
        AlertFeed(ui.alerts, onClear = vm::clearAlerts)

        SectionTitle("Audit log")
        AuditCard(ui.audit, ui.auditLoading, onLoad = { vm.loadAudit() })
        Spacer(Modifier.height(24.dp))
    }

    confirmRail?.let { (rail, on) ->
        ConfirmDialog(
            title = "Switch ${Rail.label(rail.name)} off?",
            text = "This phone may be reaching the deck through ${Rail.label(rail.name)} (Tailscale over it). " +
                "Switching it off can cut this connection; you would need the deck's own keyboard to turn it on again.",
            confirmLabel = "Switch off",
            danger = true,
            onConfirm = { vm.setRail(rail.name, on) },
            onDismiss = { confirmRail = null },
        )
    }
}

@Composable
private fun PowerCard(ui: DeckViewModel.Ui, onToggle: (Rail, Boolean) -> Unit) {
    val battery = ui.status?.battery
    LapisCard(Modifier.fillMaxWidth()) {
        Text("Battery (AXP228)", style = MaterialTheme.typography.titleSmall)
        if (battery == null) {
            Text(
                ui.batteryW?.let { "Draw ${formatWatts(it)}" } ?: "No battery reading yet",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
        } else {
            LinearProgressIndicator(
                progress = { (battery.percent / 100f).coerceIn(0f, 1f) },
                color = if (battery.percent <= 15) lapis.danger else lapis.blue,
                trackColor = MaterialTheme.colorScheme.surfaceContainerHighest,
                modifier = Modifier.fillMaxWidth().height(8.dp).padding(vertical = 2.dp),
            )
            KeyValueRow("Charge", "${battery.percent}%")
            KeyValueRow("Voltage", String.format(Locale.US, "%.2f V", battery.voltageV))
            KeyValueRow("Current", battery.currentA?.let { String.format(Locale.US, "%.2f A", it) } ?: "n/a")
            KeyValueRow("Draw", formatWatts(battery.powerW))
            KeyValueRow("State", battery.status.ifBlank { if (battery.charging) "charging" else "on battery" })
        }
    }
    LapisCard(Modifier.fillMaxWidth()) {
        Text("Power rails", style = MaterialTheme.typography.titleSmall)
        if (ui.rails.isEmpty()) {
            Text("No rails reported yet", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
        }
        ui.rails.forEach { rail ->
            val busy = rail.name in ui.railBusy
            Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(Rail.label(rail.name), style = MaterialTheme.typography.bodyMedium)
                    if (rail.detail.isNotBlank()) {
                        Text(rail.detail, style = LapisType.MonoSmall, color = lapis.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
                if (busy) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                val on = rail.on
                when {
                    !rail.switchable -> LapisBadge(
                        when (on) { true -> "on"; false -> "off"; null -> "n/a" },
                        color = if (on == true) lapis.blue else lapis.muted,
                    )
                    // State not readable (bias tee, 4G): two plain commands.
                    on == null -> Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        TextButton(onClick = { onToggle(rail, true) }, enabled = !busy) { Text("On") }
                        TextButton(onClick = { onToggle(rail, false) }, enabled = !busy) { Text("Off") }
                    }
                    else -> Switch(
                        checked = on,
                        onCheckedChange = { onToggle(rail, it) },
                        enabled = !busy,
                        modifier = Modifier.semantics { contentDescription = "${Rail.label(rail.name)} rail" },
                    )
                }
            }
        }
    }
}

@Composable
private fun GpsCard(ui: DeckViewModel.Ui) {
    val gps = ui.gps
    val summary = ui.summary?.summary?.gps
    LapisCard(Modifier.fillMaxWidth()) {
        val fix = gps?.fix
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("GNSS", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            val label = fix?.fix ?: summary?.fix ?: "none"
            LapisBadge(if (label == "none") "no fix" else "$label fix", color = if (label == "none") lapis.amber else lapis.blue, filled = label != "none")
            Spacer(Modifier.width(4.dp))
            if (gps?.ppsLocked == true || summary?.ppsLocked == true) LapisBadge("PPS", color = lapis.blue)
        }
        KeyValueRow("Status", gps?.status ?: summary?.status ?: "unknown")
        KeyValueRow("Position", formatLatLon(fix?.lat, fix?.lon))
        KeyValueRow("Altitude", fix?.altM?.let { String.format(Locale.US, "%.1f m", it) } ?: "n/a")
        KeyValueRow("Speed", fix?.speedMps?.let { String.format(Locale.US, "%.1f m/s", it) } ?: "n/a")
        KeyValueRow("Satellites", "${gps?.used ?: summary?.used ?: 0} used / ${gps?.visible ?: 0} visible")
        KeyValueRow("HDOP", gps?.hdop?.let { String.format(Locale.US, "%.2f", it) } ?: "n/a")
        gps?.ppsOffsetNs?.let { KeyValueRow("PPS offset", "$it ns") }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MeshCard(ui: DeckViewModel.Ui, onSend: (Int, String) -> Boolean) {
    val mesh = ui.mesh
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Meshtastic", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            LapisBadge(mesh?.status ?: ui.summary?.summary?.mesh?.status ?: "unknown", color = lapis.muted)
        }
        mesh?.myNode?.let { KeyValueRow("This node", it) }
        val nodes = mesh?.nodes.orEmpty().sortedByDescending { it.lastHeard }
        Text("Nodes (${nodes.size})", style = LapisType.MonoLabel, color = lapis.muted, modifier = Modifier.padding(top = 6.dp))
        if (nodes.isEmpty()) Text("No nodes heard yet", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
        nodes.take(12).forEach { node ->
            Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(node.displayName, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(
                        listOfNotNull(
                            node.id,
                            node.snr?.let { String.format(Locale.US, "SNR %.1f", it) },
                            node.rssi?.let { "RSSI $it" },
                            node.hops?.let { "$it hop" + if (it == 1) "" else "s" },
                        ).joinToString(" · "),
                        style = LapisType.MonoSmall,
                        color = lapis.muted,
                    )
                }
                Text(formatAgo(node.lastHeard), style = LapisType.MonoSmall, color = lapis.muted)
            }
        }

        HorizontalDivider(Modifier.padding(vertical = 8.dp))
        Text("Channel chat", style = MaterialTheme.typography.titleSmall)
        val messages = ui.meshMessages.takeLast(30)
        if (messages.isEmpty()) Text("No messages yet", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
        messages.forEach { m ->
            Column(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
                Text(
                    "ch${m.channel} · " + (if (m.outgoing) "you" else m.from.ifBlank { "?" }) + " · " + formatClock(m.rxTime),
                    style = LapisType.MonoSmall,
                    color = if (m.outgoing) lapis.blue else lapis.muted,
                )
                Text(m.text, style = MaterialTheme.typography.bodyMedium)
            }
        }
        var channel by rememberSaveable { mutableIntStateOf(0) }
        var draft by rememberSaveable { mutableStateOf("") }
        val sending = ui.actions[QuickActions.MESH_SEND]
        val running = sending is ActionState.Running
        Spacer(Modifier.height(6.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            (0..MeshRules.MAX_CHANNEL).forEach { ch ->
                LapisBadge("ch$ch", color = if (ch == channel) lapis.blue else lapis.muted, filled = ch == channel, onClick = { channel = ch })
            }
        }
        OutlinedTextField(
            value = draft,
            onValueChange = { draft = it },
            label = { Text("Message on ch$channel") },
            supportingText = { Text("${MeshRules.bytes(draft)} / ${MeshRules.MAX_BYTES} bytes") },
            isError = MeshRules.bytes(draft) > MeshRules.MAX_BYTES,
            enabled = !running,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Button(
                onClick = { if (onSend(channel, draft)) draft = "" },
                enabled = !running && MeshRules.problem(channel, draft) == null,
            ) { Text("Send on LoRa") }
            Spacer(Modifier.width(8.dp))
            TierBadge(ui.tierOf(QuickActions.MESH_SEND))
        }
        when (sending) {
            is ActionState.Running -> Text(
                if (sending.tier >= 1) "Waiting for approval (transmits on LoRa)…" else "Sending…",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.amber,
            )
            is ActionState.Failed -> Text(sending.message, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
            else -> Unit
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SdrCard(ui: DeckViewModel.Ui, onTune: (Long) -> Unit, onRun: (QuickAction) -> Unit) {
    val sdr = ui.sdr
    val summary = ui.summary?.summary?.sdr
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Receiver", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            if (sdr?.source?.synthetic == true || summary?.synthetic == true) LapisBadge("synthetic", color = lapis.amber)
        }
        val center = sdr?.source?.centerHz?.takeIf { it > 0 } ?: summary?.centerHz ?: 0
        KeyValueRow("Status", sdr?.status ?: summary?.status ?: "unknown")
        KeyValueRow("Centre", if (center > 0) formatMhz(center) else "n/a")
        sdr?.source?.let { src ->
            if (src.kind.isNotBlank()) KeyValueRow("Source", src.kind)
            if (src.sampleRate > 0) KeyValueRow("Sample rate", String.format(Locale.US, "%.2f MS/s", src.sampleRate / 1e6))
            KeyValueRow("Gain", String.format(Locale.US, "%.1f dB", src.gainDb))
        }
        ui.spectrum?.let { Spectrum(it, Modifier.fillMaxWidth().height(72.dp).padding(vertical = 6.dp)) }
        val peaks = (ui.spectrum?.peaks ?: sdr?.peaks).orEmpty().sortedByDescending { it.snrDb }.take(4)
        peaks.forEach { p ->
            KeyValueRow(formatMhz(p.freqHz.toLong()), String.format(Locale.US, "%.0f dB SNR", p.snrDb))
        }
        Text("Tune", style = LapisType.MonoLabel, color = lapis.muted, modifier = Modifier.padding(top = 6.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            TUNE_PRESETS.forEach { preset ->
                OutlinedButton(onClick = { onTune(preset.hz) }, enabled = ui.tuning == null) {
                    Text(preset.label, style = LapisType.MonoSmall)
                }
            }
        }
        HorizontalDivider(Modifier.padding(vertical = 8.dp))
        QuickActions.SDR.forEach { action -> QuickActionBlock(action, ui, onRun) }
    }
}

/** One waterfall row as a line: level 0..255 over the visible span. */
@Composable
private fun Spectrum(row: SdrSpectrum, modifier: Modifier) {
    val line = lapis.blue
    val grid = MaterialTheme.colorScheme.outlineVariant
    Canvas(modifier.semantics { contentDescription = "Live spectrum around ${formatMhz(row.centerHz)}" }) {
        val bins = row.bins
        if (bins.isEmpty()) return@Canvas
        drawLine(grid, Offset(size.width / 2, 0f), Offset(size.width / 2, size.height))
        val path = Path()
        bins.forEachIndexed { i, v ->
            val x = i * size.width / (bins.size - 1).coerceAtLeast(1)
            val y = size.height - v / 255f * size.height
            if (i == 0) path.moveTo(x, y) else path.lineTo(x, y)
        }
        drawPath(path, line, style = Stroke(width = 1.5f))
    }
}

@Composable
private fun QuickActionBlock(action: QuickAction, ui: DeckViewModel.Ui, onRun: (QuickAction) -> Unit) {
    val state = ui.actions[action.key]
    Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
        OutlinedButton(onClick = { onRun(action) }, enabled = state !is ActionState.Running, modifier = Modifier.weight(1f)) {
            Text(action.label, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.width(8.dp))
        TierBadge(ui.tierOf(action.tool))
    }
    when (state) {
        is ActionState.Running -> Row(verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(8.dp))
            Text(
                if (state.tier >= 1) "Waiting for approval…" else "Running…",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
        }
        is ActionState.Failed -> Text(state.message, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        is ActionState.Done -> {
            val rows = state.rows
            Text(
                ToolResults.headline(action.tool, rows.size) + if (state.synthetic) " · synthetic/mock" else "",
                style = LapisType.MonoLabel,
                color = lapis.muted,
            )
            rows.take(12).forEach { r ->
                Column(Modifier.fillMaxWidth().padding(vertical = 1.dp)) {
                    Text(r.title, style = MaterialTheme.typography.bodyMedium, color = if (r.dim) lapis.muted else Color.Unspecified)
                    if (r.detail.isNotBlank()) {
                        Text(r.detail, style = LapisType.MonoSmall, color = lapis.muted, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
        }
        null -> Unit
    }
}

@Composable
private fun TierBadge(tier: Int?) {
    if (tier == null) return
    LapisBadge(
        "tier $tier",
        color = when {
            tier >= 2 -> lapis.danger
            tier == 1 -> lapis.amber
            else -> lapis.muted
        },
    )
}

@Composable
private fun AlertFeed(alerts: List<DeckAlert>, onClear: () -> Unit) {
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Mesh messages and SDR decodes", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            if (alerts.isNotEmpty()) TextButton(onClick = onClear) { Text("Clear") }
        }
        if (alerts.isEmpty()) {
            Text(
                "Nothing yet. Incoming mesh messages and decoder hits appear here while the app is connected.",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
        }
        alerts.take(40).forEach { a ->
            Column(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    LapisBadge(if (a.kind == DeckAlert.Kind.MESH) "mesh" else "sdr", color = if (a.kind == DeckAlert.Kind.MESH) lapis.blue else lapis.amber)
                    Spacer(Modifier.width(6.dp))
                    Text(a.title, style = LapisType.MonoSmall, color = lapis.muted, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (a.synthetic) Text("synthetic", style = LapisType.MonoSmall, color = lapis.amber)
                }
                if (a.text.isNotBlank()) Text(a.text, style = MaterialTheme.typography.bodyMedium, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

@Composable
private fun AuditCard(entries: List<AuditEntry>?, loading: Boolean, onLoad: () -> Unit) {
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Policy and power decisions", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            if (loading) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
            TextButton(onClick = onLoad, enabled = !loading) { Text(if (entries == null) "Load" else "Refresh") }
        }
        if (entries != null && entries.isEmpty()) {
            Text("The audit log is empty", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
        }
        entries.orEmpty().take(50).forEach { e ->
            Column(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
                Text("${formatClock(e.ts)}  ${e.event}", style = LapisType.MonoSmall)
                val data = DeckAlert.describePacket(e.data)
                if (data.isNotBlank()) {
                    Text(data, style = LapisType.MonoSmall, color = lapis.muted, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}
