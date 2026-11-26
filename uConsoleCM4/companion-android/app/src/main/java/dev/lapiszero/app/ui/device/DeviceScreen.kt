package dev.lapiszero.app.ui.device

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Slider
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.data.remote.Pairing
import dev.lapiszero.app.data.remote.PowerAction
import dev.lapiszero.app.data.remote.Telemetry
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.HeadState
import dev.lapiszero.app.domain.LapisSession
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
import dev.lapiszero.app.ui.components.formatUptime
import dev.lapiszero.app.ui.head.LapisHead
import java.util.Locale
import kotlin.math.roundToInt

private data class PowerChoice(val action: String, val label: String, val text: String, val danger: Boolean)

/** The power menu; the wording names the hardware (CardputerZero or Lapis-Deck). */
private fun powerChoices(isDeck: Boolean): List<PowerChoice> {
    val noun = if (isDeck) "Lapis-Deck" else "CardputerZero"
    return listOf(
    PowerChoice(
        PowerAction.RESTART_DAEMON, "Restart agent",
        "Restarts lapis-daemon. A running task is stopped and this app reconnects in a few seconds.", false,
    ),
    PowerChoice(
        PowerAction.START_BLE_PAIRING, "Pair another phone (BLE)",
        "Opens a 5-minute Bluetooth pairing window and shows a PIN on the device screen.", false,
    ),
    PowerChoice(
        PowerAction.START_QR_PAIRING, "Pair another phone (QR)",
        "Shows a one-time QR code on the device screen for 3 minutes.", false,
    ),
    PowerChoice(
        PowerAction.REBOOT, "Reboot",
        "Reboots the $noun. Running tasks are stopped.", true,
    ),
    PowerChoice(
        PowerAction.SHUTDOWN, "Shutdown",
        "Powers the $noun off. You need physical access to turn it on again.", true,
    ),
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun DeviceScreen(session: LapisSession, snackbar: SnackbarHostState, isDeck: Boolean = false) {
    val vm: DeviceViewModel = viewModel(
        factory = viewModelFactory { initializer { DeviceViewModel(session.repo, session.api) } },
    )
    val ui by vm.ui.collectAsStateWithLifecycle()
    val head by session.repo.head.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    // Fast telemetry only while this tab is actually on screen.
    LifecycleResumeEffect(vm) {
        vm.onVisible(true)
        onPauseOrDispose { vm.onVisible(false) }
    }
    var confirm by remember { mutableStateOf<PowerChoice?>(null) }
    val state = ui.state
    val telemetry = ui.telemetry

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        HeadHero(head, state?.activeTask?.title)

        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            LapisBadge(connectionLabel(ui.connection), color = connectionColor(ui.connection), filled = ui.connection == ConnectionState.ONLINE)
            state?.tailscaleIp?.takeIf { it.isNotEmpty() }?.let { LapisBadge("tailscale $it", color = lapis.blue) }
            telemetry?.let { LapisBadge("up ${formatUptime(it.uptimeSec)}", color = lapis.muted) }
            state?.device?.let { device ->
                if (device.version.isNotEmpty()) LapisBadge("v${device.version}", color = lapis.muted)
                if (device.mock) LapisBadge("mock", color = lapis.amber)
            }
        }

        MirrorCard(ui.mirror, ui.liveMirror, isDeck, onRefresh = vm::refreshMirror, onLive = vm::setLiveMirror)

        ui.pairing?.let { PairingCard(it, onStop = { vm.power(PowerAction.STOP_PAIRING, "Stop pairing") }) }

        SectionTitle("Telemetry")
        if (telemetry == null) {
            Text("Waiting for telemetry…", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
        } else {
            TelemetryCards(telemetry, isDeck)
        }

        SectionTitle("Display & sound")
        LapisCard(Modifier.fillMaxWidth()) {
            Text("LCD theme", style = MaterialTheme.typography.titleSmall)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 4.dp)) {
                ThemeOption("Dark Ink", selected = ui.lcd.theme == "dark") { vm.setTheme("dark") }
                ThemeOption("Paper White", selected = ui.lcd.theme == "paper") { vm.setTheme("paper") }
            }
            Spacer(Modifier.height(10.dp))
            var slider by remember(ui.lcd.brightness) { mutableFloatStateOf(ui.lcd.brightness.toFloat()) }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Backlight", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                Text("${slider.roundToInt()}%", style = LapisType.Mono)
            }
            Slider(
                value = slider,
                onValueChange = { slider = it },
                onValueChangeFinished = { vm.setBrightness(slider.roundToInt()) },
                valueRange = 0f..100f,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Mute audio chimes", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                Switch(checked = ui.lcd.muteAudio, onCheckedChange = vm::setMute)
            }
            OutlinedButton(onClick = vm::wake, modifier = Modifier.fillMaxWidth()) { Text("Wake screen") }
            KeyValueRow("Clock timezone", clockLabel(ui.lcd.timezone, state?.utcOffsetMin))
        }

        SectionTitle("Power & pairing")
        LapisCard(Modifier.fillMaxWidth()) {
            powerChoices(isDeck).forEach { choice ->
                val busy = ui.busyAction == choice.action
                if (choice.danger) {
                    OutlinedButton(
                        onClick = { confirm = choice },
                        enabled = ui.busyAction == null,
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text(if (busy) "${choice.label}…" else choice.label, color = MaterialTheme.colorScheme.error) }
                } else {
                    OutlinedButton(
                        onClick = { confirm = choice },
                        enabled = ui.busyAction == null,
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text(if (busy) "${choice.label}…" else choice.label) }
                }
            }
        }
        Spacer(Modifier.height(24.dp))
    }

    confirm?.let { choice ->
        ConfirmDialog(
            title = "${choice.label}?",
            text = choice.text,
            confirmLabel = choice.label,
            danger = choice.danger,
            onConfirm = { vm.power(choice.action, choice.label) },
            onDismiss = { confirm = null },
        )
    }
}

/** The agent itself, as it is on the device's own screen, with its state named under it. */
@Composable
private fun HeadHero(head: HeadState, task: String?) {
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
        LapisHead(head, size = 160.dp)
        Text(head.label, style = MaterialTheme.typography.titleMedium)
        // What it is busy with, while it is.
        if (!task.isNullOrBlank()) {
            Text(task, style = LapisType.MonoSmall, color = lapis.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** "America/Los_Angeles (UTC-08:00)", or just one of the two when the daemon reports only that. */
internal fun clockLabel(timezone: String, utcOffsetMin: Int?): String {
    val offset = utcOffsetMin?.let {
        val sign = if (it < 0) "-" else "+"
        val abs = kotlin.math.abs(it)
        String.format(Locale.US, "UTC%s%02d:%02d", sign, abs / 60, abs % 60)
    }
    return when {
        timezone.isNotEmpty() && offset != null -> "$timezone ($offset)"
        timezone.isNotEmpty() -> timezone
        offset != null -> offset
        else -> "not set"
    }
}

@Composable
private fun ThemeOption(label: String, selected: Boolean, onClick: () -> Unit) {
    if (selected) Button(onClick = onClick) { Text(label) } else OutlinedButton(onClick = onClick) { Text(label) }
}

/** Pixel-sharp mirror of the 320x170 LCD. */
@Composable
private fun MirrorCard(mirror: MirrorState, live: Boolean, isDeck: Boolean, onRefresh: () -> Unit, onLive: (Boolean) -> Unit) {
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Device screen", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Text("live", style = LapisType.MonoLabel, color = lapis.muted)
            Spacer(Modifier.width(6.dp))
            Switch(checked = live, onCheckedChange = onLive, enabled = mirror !is MirrorState.Unavailable)
            TextButton(onClick = onRefresh) { Text("Refresh") }
        }
        Box(
            Modifier
                .fillMaxWidth()
                // The CardputerZero LCD is 320x170; the uConsole panel 1280x720.
                .aspectRatio(if (isDeck) 1280f / 720f else 320f / 170f)
                .border(1.dp, MaterialTheme.colorScheme.outline)
                .clickable(onClickLabel = "Refresh screen mirror", onClick = onRefresh),
            contentAlignment = Alignment.Center,
        ) {
            when (mirror) {
                is MirrorState.Frame -> {
                    val bitmap = remember(mirror) {
                        BitmapFactory.decodeByteArray(mirror.png, 0, mirror.png.size)?.asImageBitmap()
                    }
                    if (bitmap != null) {
                        Image(
                            bitmap = bitmap,
                            contentDescription = if (isDeck) "Lapis-Deck screen" else "CardputerZero screen",
                            filterQuality = FilterQuality.None,
                            contentScale = ContentScale.FillBounds,
                            modifier = Modifier.fillMaxSize(),
                        )
                    } else {
                        Text("The device sent an unreadable image", style = LapisType.Mono, color = lapis.muted)
                    }
                }

                MirrorState.Unavailable -> Text(
                    "Screen mirror unavailable: lapis-ui is not running on the device (this is always the " +
                        "case for a mock daemon without the simulator).",
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                    modifier = Modifier.padding(16.dp),
                )

                is MirrorState.Failed -> Text(
                    mirror.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(16.dp),
                )

                MirrorState.Loading -> CircularProgressIndicator()
                MirrorState.Idle -> Text("Tap to load", style = LapisType.Mono, color = lapis.muted)
            }
        }
    }
}

@Composable
private fun PairingCard(pairing: Pairing, onStop: () -> Unit) {
    LapisCard(Modifier.fillMaxWidth(), borderColor = lapis.amber) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Pairing window open", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            LapisBadge(if (pairing.mode == "qr") "QR" else "BLE PIN", color = lapis.amber, filled = true)
        }
        Text(
            if (pairing.mode == "qr") {
                "A one-time QR code is on the device screen. Scan it with the other phone."
            } else {
                "The device (${pairing.deviceName ?: "CardputerZero"}) is advertising over Bluetooth" +
                    (pairing.status?.let { " · $it" } ?: "") + ". The PIN is shown on its screen."
            },
            style = MaterialTheme.typography.bodySmall,
            color = lapis.muted,
        )
        pairing.expiresIn?.let { KeyValueRow("Window", "${it}s") }
        OutlinedButton(onClick = onStop, modifier = Modifier.fillMaxWidth()) { Text("Stop pairing") }
    }
}

@Composable
private fun TelemetryCards(t: Telemetry, isDeck: Boolean) {
    val battery = t.battery
    LapisCard(Modifier.fillMaxWidth()) {
        Text(if (isDeck) "Battery (AXP228)" else "Battery (BQ27220)", style = MaterialTheme.typography.titleSmall)
        if (battery == null) {
            Text("No fuel gauge reading", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
        } else {
            Gauge(battery.percent / 100f, if (battery.percent <= 15) lapis.danger else lapis.blue)
            KeyValueRow("Charge", "${battery.percent}%")
            KeyValueRow("Voltage", String.format(Locale.US, "%.2f V", battery.voltageV))
            battery.powerW?.let { KeyValueRow("Draw", String.format(Locale.US, "%.2f W", it)) }
            battery.currentA?.let { KeyValueRow("Current", String.format(Locale.US, "%.2f A", it)) }
            KeyValueRow("State", battery.status?.takeIf { it.isNotBlank() } ?: if (battery.charging) "charging" else "on battery")
        }
    }
    val m = t.memory
    // The 220 MB budget is the 512 MB CM0's; a CM4 deck has 4 or 8 GB.
    val over = m.overBudget && !isDeck
    LapisCard(Modifier.fillMaxWidth(), borderColor = if (over) lapis.amber else MaterialTheme.colorScheme.outlineVariant) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Memory", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            if (over) LapisBadge("over 220 MB", color = lapis.amber, filled = true)
        }
        Gauge(if (m.totalMb > 0) (m.usedMb / m.totalMb).toFloat() else 0f, if (over) lapis.amber else lapis.blue)
        KeyValueRow(
            "In use",
            "${m.usedMb.roundToInt()} / ${m.totalMb.roundToInt()} MB",
            valueColor = if (over) lapis.amber else Color.Unspecified,
        )
        if (over) {
            Text(
                "Active memory is above the 220 MB budget of the 512 MB CM0.",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.amber,
            )
        }
        KeyValueRow("zram", "${m.zramUsedMb.roundToInt()} / ${m.zramTotalMb.roundToInt()} MB")
        KeyValueRow("lapis-daemon RSS", "${m.daemonRssMb.roundToInt()} MB")
        KeyValueRow("lapis-ui RSS", "${m.uiRssMb.roundToInt()} MB")
    }
    LapisCard(Modifier.fillMaxWidth()) {
        Text("CPU", style = MaterialTheme.typography.titleSmall)
        KeyValueRow(
            "Temperature",
            t.cpu.tempC?.let { String.format(Locale.US, "%.1f °C", it) } ?: "n/a",
            valueColor = if ((t.cpu.tempC ?: 0.0) >= 75.0) lapis.amber else Color.Unspecified,
        )
        KeyValueRow("Load (1 min)", t.cpu.load1?.let { String.format(Locale.US, "%.2f", it) } ?: "n/a")
    }
    LapisCard(Modifier.fillMaxWidth()) {
        Text("Wi-Fi", style = MaterialTheme.typography.titleSmall)
        KeyValueRow("Network", t.wifi.ssid.ifEmpty { "not connected" })
        KeyValueRow("Address", t.wifi.ip.ifEmpty { "n/a" })
        KeyValueRow("Signal", t.wifi.rssiDbm?.let { "$it dBm" } ?: "n/a")
    }
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Tailscale", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            LapisBadge(t.tailscale.state.ifEmpty { "unknown" }, color = if (t.tailscale.state == "connected") lapis.blue else lapis.amber)
        }
        KeyValueRow("Name", t.tailscale.fqdn.ifEmpty { "n/a" })
        KeyValueRow("Address", t.tailscale.ip.ifEmpty { "n/a" })
        KeyValueRow("Peers", t.tailscale.peers.toString())
    }
    if (t.services.isNotEmpty()) {
        LapisCard(Modifier.fillMaxWidth()) {
            Text("Services", style = MaterialTheme.typography.titleSmall)
            t.services.forEach { (name, status) ->
                KeyValueRow(name, status, valueColor = if (status == "active") lapis.blue else lapis.danger)
            }
        }
    }
}

@Composable
private fun Gauge(fraction: Float, color: Color) {
    LinearProgressIndicator(
        progress = { fraction.coerceIn(0f, 1f) },
        color = color,
        trackColor = MaterialTheme.colorScheme.surfaceContainerHighest,
        modifier = Modifier.fillMaxWidth().height(8.dp).padding(vertical = 2.dp),
    )
}
