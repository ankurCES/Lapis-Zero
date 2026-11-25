package dev.lapiszero.app.ui.onboarding

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.AppContainer
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.ErrorRetry
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.StatusDot

private enum class Path { WELCOME, BLE, QR, DEV }

private const val TAILSCALE_PACKAGE = "com.tailscale.ipn"

fun phoneInfo(): PhoneInfo {
    val model = listOf(Build.MANUFACTURER, Build.MODEL).filter { !it.isNullOrBlank() }.joinToString(" ")
    return PhoneInfo(name = Build.MODEL?.takeIf { it.isNotBlank() } ?: "Android", model = model)
}

/** Welcome screen plus the three ways in: BLE zero-cable, QR, and manual dev connect. */
@Composable
fun OnboardingFlow(container: AppContainer, canCancel: Boolean, onFinished: () -> Unit) {
    var path by rememberSaveable { mutableStateOf(Path.WELCOME) }
    // Each pairing screen handles Back itself (it has state to reset); here only "cancel".
    BackHandler(enabled = path == Path.WELCOME && canCancel) { onFinished() }
    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        when (path) {
            Path.WELCOME -> Welcome(canCancel, onPick = { path = it }, onCancel = onFinished)
            Path.BLE -> BleWizardScreen(container, onBack = { path = Path.WELCOME }, onFinished = onFinished)
            Path.QR -> {
                val vm: PairingViewModel = viewModel(
                    key = "pair-qr",
                    factory = viewModelFactory { initializer { PairingViewModel(container.devices, phoneInfo()) } },
                )
                QrPairScreen(vm, onBack = { vm.reset(); path = Path.WELCOME }, onFinished = { vm.reset(); onFinished() })
            }

            Path.DEV -> {
                val vm: PairingViewModel = viewModel(
                    key = "pair-dev",
                    factory = viewModelFactory { initializer { PairingViewModel(container.devices, phoneInfo()) } },
                )
                DevConnectScreen(vm, onBack = { vm.reset(); path = Path.WELCOME }, onFinished = { vm.reset(); onFinished() })
            }
        }
    }
}

@Composable
internal fun WizardHeader(title: String, onBack: (() -> Unit)?) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        if (onBack != null) TextButton(onClick = onBack) { Text("‹ Back") } else Spacer(Modifier.width(12.dp))
        Text(title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(start = 4.dp))
    }
}

private fun isVpnActive(context: Context): Boolean {
    val cm = context.getSystemService(ConnectivityManager::class.java) ?: return false
    val caps = cm.getNetworkCapabilities(cm.activeNetwork ?: return false) ?: return false
    return caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
}

@Composable
private fun Welcome(canCancel: Boolean, onPick: (Path) -> Unit, onCancel: () -> Unit) {
    val context = LocalContext.current
    var vpn by remember { mutableStateOf(isVpnActive(context)) }
    LifecycleResumeEffect(Unit) {
        vpn = isVpnActive(context)
        onPauseOrDispose { }
    }
    val tailscale = remember { context.packageManager.getLaunchIntentForPackage(TAILSCALE_PACKAGE) }

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("LAPIS-ZERO", style = LapisType.MonoLarge)
        Text(
            "Pair this phone with a CardputerZero to chat with its agent, watch the target screen and approve risky actions.",
            style = MaterialTheme.typography.bodyLarge,
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            StatusDot(if (vpn) lapis.blue else lapis.amber)
            Spacer(Modifier.width(8.dp))
            Text(
                if (vpn) "A VPN is active on this phone (Tailscale looks connected)." else
                    "No VPN is active. Connect Tailscale on this phone to reach the device.",
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            if (!vpn && tailscale != null) {
                TextButton(onClick = { runCatching { context.startActivity(tailscale) } }) { Text("Open Tailscale") }
            }
        }
        Spacer(Modifier.height(4.dp))
        EntryCard(
            badge = "bluetooth",
            title = "Pair a new CardputerZero",
            text = "No cable, no SD-card editing: enter the PIN on its screen, pick Wi-Fi and join Tailscale.",
            onClick = { onPick(Path.BLE) },
        )
        EntryCard(
            badge = "qr code",
            title = "Pair an existing device",
            text = "Already on Tailscale? On the device choose Menu -> Pair Android App (QR) and scan the code.",
            onClick = { onPick(Path.QR) },
        )
        EntryCard(
            badge = "dev",
            title = "Dev connect",
            text = "Connect to a mock daemon on your computer with host:port and its pairing token.",
            onClick = { onPick(Path.DEV) },
        )
        if (canCancel) {
            OutlinedButton(onClick = onCancel, modifier = Modifier.fillMaxWidth()) { Text("Cancel") }
        }
    }
}

@Composable
private fun EntryCard(badge: String, title: String, text: String, onClick: () -> Unit) {
    LapisCard(Modifier.fillMaxWidth(), borderColor = MaterialTheme.colorScheme.outline, onClick = onClick) {
        LapisBadge(badge, color = lapis.blue)
        Spacer(Modifier.height(6.dp))
        Text(title, style = MaterialTheme.typography.titleMedium)
        Text(text, style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
    }
}

/** True while the pairing result (progress or success) replaces the input form. */
internal fun PairUi.isTerminal(): Boolean = this is PairUi.Working || this is PairUi.Done

/** Shared outcome area for the QR and dev paths: progress, success, or the error to retry. */
@Composable
internal fun PairOutcome(ui: PairUi, onRetry: () -> Unit, onFinished: () -> Unit) {
    when (ui) {
        is PairUi.Working -> Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator()
            Spacer(Modifier.width(16.dp))
            Text(ui.label, style = MaterialTheme.typography.bodyLarge)
        }

        is PairUi.Done -> Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            LapisBadge("paired", color = lapis.blue, filled = true)
            Text("Connected to ${ui.deviceName}", style = MaterialTheme.typography.titleLarge)
            Text(
                "Its access token is stored encrypted with a key in the Android Keystore.",
                style = MaterialTheme.typography.bodyMedium,
                color = lapis.muted,
            )
            Button(onClick = onFinished, modifier = Modifier.fillMaxWidth()) { Text("Continue") }
        }

        is PairUi.Failed -> Column(Modifier.padding(16.dp)) { ErrorRetry(ui.message, onRetry = onRetry) }
        else -> Unit
    }
}

@Composable
private fun DevConnectScreen(vm: PairingViewModel, onBack: () -> Unit, onFinished: () -> Unit) {
    val ui by vm.ui.collectAsStateWithLifecycle()
    var host by rememberSaveable { mutableStateOf("127.0.0.1:8421") }
    // Not saveable on purpose: the token should not be written into saved instance state.
    var token by remember { mutableStateOf("") }

    BackHandler(onBack = if (ui is PairUi.Done) onFinished else onBack)
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        WizardHeader("Dev connect", onBack.takeIf { ui !is PairUi.Done })
        PairOutcome(ui, onRetry = vm::reset, onFinished = onFinished)
        if (ui.isTerminal()) return@Column
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                "For a development daemon started with --mock --dev-pair-token. Plain HTTP is only " +
                    "allowed to 127.0.0.1, localhost and 10.0.2.2.",
                style = MaterialTheme.typography.bodyMedium,
            )
            OutlinedTextField(
                value = host,
                onValueChange = { host = it.trim() },
                label = { Text("Daemon address (host:port)") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = token,
                onValueChange = { token = it },
                label = { Text("Pairing token") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                onClick = { vm.devConnect(host, token) },
                enabled = host.isNotBlank() && token.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Connect") }
            LapisCard(Modifier.fillMaxWidth()) {
                Text("Physical phone over USB", style = MaterialTheme.typography.titleSmall)
                Text("adb reverse tcp:8421 tcp:8421", style = LapisType.Mono)
                Text("then connect to 127.0.0.1:8421", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
                Spacer(Modifier.height(6.dp))
                Text("Android emulator", style = MaterialTheme.typography.titleSmall)
                Text("10.0.2.2:8421", style = LapisType.Mono)
            }
        }
    }
}
