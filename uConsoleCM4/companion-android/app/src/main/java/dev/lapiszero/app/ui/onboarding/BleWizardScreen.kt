package dev.lapiszero.app.ui.onboarding

import android.Manifest
import android.bluetooth.BluetoothAdapter
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.AppContainer
import dev.lapiszero.app.data.ble.BlePermissions
import dev.lapiszero.app.data.ble.WifiNetwork
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.ui.components.ErrorRetry
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.LoadingRow

private val STEP_NAMES = listOf("Find", "PIN", "Wi-Fi", "Tailscale", "Keys", "Verify")

private fun stepIndex(step: BleStep): Int = when (step) {
    BleStep.Intro, is BleStep.Scanning, is BleStep.Connecting -> 0
    is BleStep.EnterPin -> 1
    is BleStep.Wifi -> 2
    is BleStep.Tailscale -> 3
    is BleStep.Secrets -> 4
    is BleStep.Verifying, is BleStep.Done -> 5
    is BleStep.Failed -> 0
}

/** Opens the Tailscale login page in a Custom Tab (the user is usually signed in there already). */
private fun openCustomTab(context: Context, url: String) {
    if (!url.startsWith("https://")) return
    val uri = url.toUri()
    try {
        CustomTabsIntent.Builder().setShowTitle(true).build().launchUrl(context, uri)
    } catch (e: Exception) {
        runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, uri).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
    }
}

/** Path A: zero-cable pairing and provisioning over Bluetooth LE. */
@Composable
fun BleWizardScreen(container: AppContainer, onBack: () -> Unit, onFinished: () -> Unit) {
    val context = LocalContext.current
    val vm: BleWizardViewModel = viewModel(
        key = "ble-wizard",
        factory = viewModelFactory {
            initializer { BleWizardViewModel(container.ble, container.devices, phoneInfo(), container.appScope) }
        },
    )
    val step by vm.step.collectAsStateWithLifecycle()
    val leave = {
        vm.reset()
        onBack()
    }
    val finish = {
        vm.reset()
        onFinished()
    }
    // After the device is stored there is nothing to go "back" to: back means done.
    BackHandler { if (step is BleStep.Done) finish() else leave() }
    LaunchedEffect(vm) {
        vm.events.collect { event ->
            when (event) {
                is BleWizardEvent.OpenUrl -> openCustomTab(context, event.url)
            }
        }
    }

    Column(Modifier.fillMaxSize()) {
        WizardHeader("Pair over Bluetooth", leave.takeIf { step !is BleStep.Done })
        LinearProgressIndicator(
            progress = { (stepIndex(step) + 1) / STEP_NAMES.size.toFloat() },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        )
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp), horizontalArrangement = Arrangement.SpaceBetween) {
            STEP_NAMES.forEachIndexed { index, name ->
                Text(
                    name,
                    style = LapisType.MonoSmall,
                    color = if (index == stepIndex(step)) MaterialTheme.colorScheme.onSurface else lapis.muted,
                )
            }
        }
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            when (val s = step) {
                BleStep.Intro -> IntroStep(container, onReady = vm::startScan)
                is BleStep.Scanning -> ScanStep(s, onSelect = vm::select, onRescan = vm::startScan)
                is BleStep.Connecting -> {
                    LoadingRow("Connecting to ${s.deviceName}…")
                    s.notice?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = lapis.amber) }
                }

                is BleStep.EnterPin -> PinStep(s, onSubmit = vm::submitPin)
                is BleStep.Wifi -> WifiStep(s, onScan = vm::scanWifi, onConnect = vm::connectWifi, onSkip = vm::skipWifi)
                is BleStep.Tailscale -> TailscaleStep(
                    s,
                    onLogin = { vm.tailscaleUp(null) },
                    onAuthKey = { vm.tailscaleUp(it) },
                    onOpen = { openCustomTab(context, it) },
                )

                is BleStep.Secrets -> SecretsStep(s, onPush = vm::pushSecrets)
                is BleStep.Verifying -> VerifyStep(s, onRetry = vm::verify, onExit = vm::exitWithoutFinishing)
                is BleStep.Done -> DoneStep(s, finish)
                is BleStep.Failed -> {
                    ErrorRetry(s.message)
                    Button(onClick = vm::startScan, modifier = Modifier.fillMaxWidth()) { Text("Start over") }
                }
            }
        }
    }
}

@Composable
private fun IntroStep(container: AppContainer, onReady: () -> Unit) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(BlePermissions.granted(context)) }
    var denied by remember { mutableStateOf(false) }
    var enabled by remember { mutableStateOf(container.ble.isEnabled) }
    val permissions = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        granted = result.values.all { it }
        denied = !granted
    }
    val enable = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        enabled = container.ble.isEnabled
    }

    Text(
        "On a CardputerZero choose Menu -> Pair Android App; on a uConsole open the pairing screen in [7] SYSTEM " +
            "(a freshly flashed device starts in this mode). It shows a 6-digit PIN and stays discoverable for five minutes.",
        style = MaterialTheme.typography.bodyMedium,
    )
    when {
        !container.ble.isSupported -> ErrorRetry("This phone has no Bluetooth LE. Use QR pairing instead.")
        !granted -> {
            Text(
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    "Lapis Zero needs the Nearby devices permission to find and connect to the device."
                } else {
                    "On this Android version, Bluetooth scanning requires the Location permission. Lapis Zero does not use your location."
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            if (denied) {
                Text(
                    "Permission was denied. If no dialog appears, allow it in the app's system settings.",
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.amber,
                )
            }
            Button(onClick = { permissions.launch(BlePermissions.required()) }, modifier = Modifier.fillMaxWidth()) {
                Text("Allow Bluetooth access")
            }
        }

        !enabled -> {
            Text("Bluetooth is turned off.", style = MaterialTheme.typography.bodyMedium)
            Button(
                onClick = {
                    // ACTION_REQUEST_ENABLE needs BLUETOOTH_CONNECT on Android 12+, granted above.
                    val allowed = Build.VERSION.SDK_INT < Build.VERSION_CODES.S ||
                        ContextCompat.checkSelfPermission(context, Manifest.permission.BLUETOOTH_CONNECT) ==
                        PackageManager.PERMISSION_GRANTED
                    if (allowed) {
                        runCatching { enable.launch(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE)) }
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Turn on Bluetooth") }
        }

        else -> Button(onClick = onReady, modifier = Modifier.fillMaxWidth()) { Text("Search for devices") }
    }
}

@Composable
private fun ScanStep(step: BleStep.Scanning, onSelect: (dev.lapiszero.app.data.ble.FoundDevice) -> Unit, onRescan: () -> Unit) {
    if (step.error != null) {
        ErrorRetry(step.error, onRetry = onRescan)
        return
    }
    LoadingRow("Looking for LapisZero-XXXX or LapisDeck-XXXX nearby…")
    if (step.devices.isEmpty()) {
        Text(
            "Nothing found yet. Make sure the pairing screen with the PIN is showing on the device." +
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
                    " On this Android version, Location must also be switched on for Bluetooth scanning."
                } else {
                    ""
                },
            style = MaterialTheme.typography.bodySmall,
            color = lapis.muted,
        )
    }
    step.devices.forEach { device ->
        LapisCard(Modifier.fillMaxWidth(), borderColor = lapis.blue, onClick = { onSelect(device) }) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(device.name, style = MaterialTheme.typography.titleMedium)
                    Text(device.address, style = LapisType.MonoSmall, color = lapis.muted)
                    DeviceKind.label(device.kind).takeIf { it.isNotEmpty() }?.let {
                        LapisBadge(it, color = if (DeviceKind.isDeck(device.kind)) lapis.amber else lapis.blue)
                    }
                }
                Text("${device.rssi} dBm", style = LapisType.Mono)
            }
        }
    }
}

@Composable
private fun PinStep(step: BleStep.EnterPin, onSubmit: (String) -> Unit) {
    var pin by remember(step.deviceName, step.attemptsLeft) { mutableStateOf("") }
    Text("Enter the PIN shown on ${step.deviceName}", style = MaterialTheme.typography.titleLarge)
    Text(
        "The PIN proves you are holding the device. It never leaves this phone: both sides only prove they know it.",
        style = MaterialTheme.typography.bodyMedium,
        color = lapis.muted,
    )
    OutlinedTextField(
        value = pin,
        onValueChange = { value -> pin = value.filter { it in '0'..'9' }.take(6) },
        label = { Text("6-digit PIN") },
        singleLine = true,
        textStyle = LapisType.MonoLarge,
        enabled = !step.verifying,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
        modifier = Modifier.fillMaxWidth(),
    )
    step.message?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = lapis.amber) }
    if (step.attemptsLeft < 3) {
        Text(
            "${step.attemptsLeft} attempt" + (if (step.attemptsLeft == 1) "" else "s") +
                " left before the device locks and shows a new PIN.",
            style = MaterialTheme.typography.bodySmall,
            color = lapis.muted,
        )
    }
    Button(
        onClick = { onSubmit(pin) },
        enabled = pin.length == 6 && !step.verifying,
        modifier = Modifier.fillMaxWidth(),
    ) { Text(if (step.verifying) "Checking…" else "Verify") }
    if (step.verifying) LoadingRow("Waiting for the device to confirm…")
}

@Composable
private fun WifiStep(
    step: BleStep.Wifi,
    onScan: () -> Unit,
    onConnect: (String, String?) -> Unit,
    onSkip: () -> Unit,
) {
    var selected by remember { mutableStateOf<WifiNetwork?>(null) }
    var psk by remember { mutableStateOf("") } // never saved: a Wi-Fi password is a secret
    Text("Connect the device to Wi-Fi", style = MaterialTheme.typography.titleLarge)
    step.error?.let { ErrorRetry(it) }
    when {
        step.connectingTo != null -> LoadingRow("Joining ${step.connectingTo}… this can take half a minute.")
        step.scanning -> LoadingRow("The device is scanning for networks…")
        else -> {
            if (step.networks.isEmpty()) {
                Text("No networks found.", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
            }
            step.networks.forEach { network ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable {
                            selected = network
                            psk = ""
                        }
                        .padding(vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(network.ssid, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                    if (network.secured) LapisBadge("secured", color = lapis.muted)
                    Spacer(Modifier.width(8.dp))
                    Text("${network.rssi} dBm", style = LapisType.MonoSmall, color = lapis.muted)
                }
                if (selected?.ssid == network.ssid) {
                    if (network.secured) {
                        OutlinedTextField(
                            value = psk,
                            onValueChange = { psk = it },
                            label = { Text("Password for ${network.ssid}") },
                            singleLine = true,
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                    Button(
                        onClick = { onConnect(network.ssid, psk.takeIf { network.secured }) },
                        enabled = !network.secured || psk.length >= 8,
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text("Connect") }
                }
                HorizontalDivider()
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onScan) { Text("Scan again") }
                TextButton(onClick = onSkip) { Text("Already online, skip") }
            }
        }
    }
}

@Composable
private fun TailscaleStep(
    step: BleStep.Tailscale,
    onLogin: () -> Unit,
    onAuthKey: (String) -> Unit,
    onOpen: (String) -> Unit,
) {
    var authKey by remember { mutableStateOf("") } // not saved: an auth key is a credential
    Text("Join your tailnet", style = MaterialTheme.typography.titleLarge)
    Text(
        "The device is only ever reachable through Tailscale. Sign it in with your browser, or use a pre-auth key.",
        style = MaterialTheme.typography.bodyMedium,
        color = lapis.muted,
    )
    step.error?.let { ErrorRetry(it) }
    if (step.waiting) {
        LoadingRow(if (step.authUrl == null) "Starting Tailscale on the device…" else "Waiting for you to finish signing in…")
        step.authUrl?.let { url ->
            LapisCard(Modifier.fillMaxWidth(), borderColor = lapis.blue) {
                Text("Login page", style = MaterialTheme.typography.titleSmall)
                Text(url, style = LapisType.MonoSmall, color = lapis.muted)
                Button(onClick = { onOpen(url) }, modifier = Modifier.fillMaxWidth()) { Text("Open login page again") }
            }
        }
        return
    }
    Button(onClick = onLogin, modifier = Modifier.fillMaxWidth()) { Text("Sign in with browser") }
    HorizontalDivider()
    OutlinedTextField(
        value = authKey,
        onValueChange = { authKey = it.trim() },
        label = { Text("Pre-auth key (tskey-auth-…)") },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedButton(
        onClick = { onAuthKey(authKey) },
        enabled = authKey.startsWith("tskey-"),
        modifier = Modifier.fillMaxWidth(),
    ) { Text("Use auth key") }
}

@Composable
private fun SecretsStep(step: BleStep.Secrets, onPush: (Map<String, String>) -> Unit) {
    var anthropic by remember { mutableStateOf("") }
    var openai by remember { mutableStateOf("") }
    Text("API keys (optional)", style = MaterialTheme.typography.titleLarge)
    Text(
        "Sent over the encrypted Bluetooth channel straight into the device's secrets file. You can also " +
            "do this later, and add a key for any other provider (OpenRouter, MiniMax, ...), under " +
            "Config -> Agent & LLM.",
        style = MaterialTheme.typography.bodyMedium,
        color = lapis.muted,
    )
    step.error?.let { ErrorRetry(it) }
    listOf(
        Triple("ANTHROPIC_API_KEY", anthropic) { v: String -> anthropic = v },
        Triple("OPENAI_API_KEY", openai) { v: String -> openai = v },
    ).forEach { (name, value, set) ->
        OutlinedTextField(
            value = value,
            onValueChange = { set(it.trim()) },
            label = { Text(name) },
            singleLine = true,
            enabled = !step.saving,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
            modifier = Modifier.fillMaxWidth(),
        )
    }
    Button(
        onClick = { onPush(mapOf("ANTHROPIC_API_KEY" to anthropic, "OPENAI_API_KEY" to openai)) },
        enabled = !step.saving,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            when {
                step.saving -> "Saving…"
                anthropic.isBlank() && openai.isBlank() -> "Skip"
                else -> "Save and continue"
            },
        )
    }
}

@Composable
private fun VerifyStep(step: BleStep.Verifying, onRetry: () -> Unit, onExit: () -> Unit) {
    Text("Securing connection…", style = MaterialTheme.typography.titleLarge)
    Text("https://${step.fqdn}", style = LapisType.Mono)
    if (step.checking) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator()
            Spacer(Modifier.width(16.dp))
            Column {
                Text("Checking the device over Tailscale (attempt ${step.attempt})", style = MaterialTheme.typography.bodyMedium)
                Text(
                    "Tailscale issues the TLS certificate on first use; this can take up to a minute.",
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
            }
        }
        return
    }
    step.error?.let { ErrorRetry(it) }
    Button(onClick = onRetry, modifier = Modifier.fillMaxWidth()) { Text("Retry") }
    OutlinedButton(onClick = onExit, modifier = Modifier.fillMaxWidth()) { Text("Exit setup for now") }
    Text(
        "Exiting keeps the pairing on this phone but does not close the device's Bluetooth window; it " +
            "closes by itself after a few minutes.",
        style = MaterialTheme.typography.bodySmall,
        color = lapis.muted,
    )
}

@Composable
private fun DoneStep(step: BleStep.Done, onFinished: () -> Unit) {
    LapisBadge(if (step.verified) "ready" else "paired, not verified", color = if (step.verified) lapis.blue else lapis.amber, filled = true)
    Text(step.deviceName, style = MaterialTheme.typography.titleLarge)
    Text(
        if (step.verified) {
            "The device is on your tailnet, answered over HTTPS and has switched its Bluetooth pairing service off."
        } else {
            "The device is paired, but it could not be reached over HTTPS yet. The app keeps trying once you continue."
        },
        style = MaterialTheme.typography.bodyMedium,
    )
    Spacer(Modifier.height(4.dp))
    Button(onClick = onFinished, modifier = Modifier.fillMaxWidth()) { Text("Open workspace") }
}
