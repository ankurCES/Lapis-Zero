package dev.lapiszero.app.ui.device

import android.Manifest
import android.bluetooth.BluetoothAdapter
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
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
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.AppContainer
import dev.lapiszero.app.data.ble.BleAccess
import dev.lapiszero.app.data.ble.BlePermissions
import dev.lapiszero.app.data.ble.FoundDevice
import dev.lapiszero.app.data.ble.WifiNetwork
import dev.lapiszero.app.data.remote.WifiRules
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.ConfirmDialog
import dev.lapiszero.app.ui.components.ErrorRetry
import dev.lapiszero.app.ui.components.KeyValueRow
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.LoadingRow
import dev.lapiszero.app.ui.components.SectionTitle

/** "Change Wi-Fi" for the active paired device: over the API, or over Bluetooth when it is offline. */
@Composable
fun WifiScreen(container: AppContainer, session: LapisSession) {
    val device = session.device
    val vm: WifiViewModel = viewModel(
        factory = viewModelFactory {
            initializer { WifiViewModel(device, session.api, container.ble, container.devices, container.appScope) }
        },
    )
    val step by vm.step.collectAsStateWithLifecycle()

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        when (val s = step) {
            WifiStep.Loading -> LoadingRow("Asking ${device.name} for its Wi-Fi…")
            is WifiStep.Networks -> NetworksView(device.name, s, vm)
            is WifiStep.Offline -> {
                Text("${device.name} does not answer over the network", style = MaterialTheme.typography.titleMedium)
                Text(s.reason, style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
                Text("Its Wi-Fi can be changed over Bluetooth, without pairing again.", style = MaterialTheme.typography.bodyMedium)
                ListeningNote()
                Button(onClick = vm::useBluetooth, modifier = Modifier.fillMaxWidth()) { Text("Change it over Bluetooth") }
                OutlinedButton(onClick = vm::load, modifier = Modifier.fillMaxWidth()) { Text("Try the network again") }
            }

            is WifiStep.BleSearch -> BleSearchView(container.ble, device.name, s, vm)
            is WifiStep.BleConnecting -> LoadingRow("Connecting to ${s.name} over Bluetooth…")
            is WifiStep.Switching -> {
                LapisBadge("switching", color = lapis.amber, filled = true)
                Text("${device.name} is switching to ${s.ssid}", style = MaterialTheme.typography.titleMedium)
                Text(
                    "The answer was lost because the device left the network this phone reached it through. " +
                        "It will come back on Tailscale when the new network is up.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = lapis.muted,
                )
                OutlinedButton(onClick = vm::load, modifier = Modifier.fillMaxWidth()) { Text("Check again") }
            }

            is WifiStep.Joined -> {
                LapisBadge("connected", color = lapis.blue, filled = true)
                Text("${device.name} joined ${s.ssid}", style = MaterialTheme.typography.titleMedium)
                if (s.ip.isNotEmpty()) KeyValueRow("Local address", s.ip)
                Text(
                    "It will be back on Tailscale in a moment.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = lapis.muted,
                )
                OutlinedButton(onClick = vm::load, modifier = Modifier.fillMaxWidth()) { Text("Show its Wi-Fi") }
            }

            is WifiStep.Failed -> {
                ErrorRetry(s.message)
                ListeningNote()
                Button(onClick = vm::useBluetooth, modifier = Modifier.fillMaxWidth()) { Text("Search again") }
                OutlinedButton(onClick = vm::load, modifier = Modifier.fillMaxWidth()) { Text("Try the network again") }
            }
        }
    }
}

@Composable
private fun ListeningNote() {
    Text(WifiViewModel.LISTENING, style = MaterialTheme.typography.bodySmall, color = lapis.muted)
}

@Composable
private fun NetworksView(deviceName: String, s: WifiStep.Networks, vm: WifiViewModel) {
    var forgetActive by remember { mutableStateOf<String?>(null) }
    val info = s.info
    LapisCard(Modifier.fillMaxWidth()) {
        Text("Wi-Fi of $deviceName", style = MaterialTheme.typography.titleSmall)
        if (info != null) {
            KeyValueRow("Network", info.ssid.ifEmpty { "not connected" })
            KeyValueRow("Address", info.ip.ifEmpty { "n/a" })
            KeyValueRow("Signal", info.rssi?.let { "$it dBm" } ?: "n/a")
        } else {
            Text("Connected over Bluetooth to ${s.bleName}", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
        }
    }
    if (info != null && info.saved.isNotEmpty()) {
        SectionTitle("Saved networks")
        info.saved.forEach { saved ->
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(saved.name, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
                if (saved.active) LapisBadge("in use", color = lapis.blue)
                TextButton(
                    enabled = s.forgetting == null && s.connectingTo == null,
                    onClick = { if (saved.active) forgetActive = saved.name else vm.forget(saved.name) },
                ) { Text(if (s.forgetting == saved.name) "Forgetting…" else "Forget") }
            }
        }
    }
    SectionTitle("Networks nearby")
    s.error?.let { ErrorRetry(it) }
    when {
        s.connectingTo != null -> LoadingRow("Joining ${s.connectingTo}… this can take half a minute.")
        s.scanning -> LoadingRow("The device is scanning for networks…")
        else -> NetworkPicker(s.networks, onConnect = vm::connect)
    }
    if (s.connectingTo == null) {
        OutlinedButton(onClick = vm::scan, enabled = !s.scanning, modifier = Modifier.fillMaxWidth()) { Text("Scan again") }
    }
    if (s.via == WifiVia.API && s.connectingTo == null) {
        HorizontalDivider()
        TextButton(onClick = vm::useBluetooth) { Text("Let my phone set it over Bluetooth") }
        Text(
            "For when this phone will not reach the device on the new network: it opens the device's " +
                "Bluetooth window for this phone.",
            style = MaterialTheme.typography.bodySmall,
            color = lapis.muted,
        )
    }
    forgetActive?.let { name ->
        ConfirmDialog(
            title = "Forget $name?",
            text = "$deviceName is on this network now and goes offline until it joins another one.",
            confirmLabel = "Forget",
            danger = true,
            onConfirm = { vm.forget(name) },
            onDismiss = { forgetActive = null },
        )
    }
}

@Composable
private fun NetworkPicker(networks: List<WifiNetwork>, onConnect: (String, String?) -> Unit) {
    var selected by remember { mutableStateOf<String?>(null) }
    // `remember`, never saved state; emptied the moment it is handed over.
    var psk by remember { mutableStateOf("") }
    if (networks.isEmpty()) {
        Text("No networks found.", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
    }
    networks.forEach { network ->
        Row(
            Modifier
                .fillMaxWidth()
                .clickable {
                    selected = network.ssid
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
        if (selected == network.ssid) {
            if (network.secured) {
                OutlinedTextField(
                    value = psk,
                    onValueChange = { psk = it },
                    label = { Text("Password for ${network.ssid}") },
                    singleLine = true,
                    isError = psk.isNotEmpty() && !WifiRules.isValidPsk(psk),
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            Button(
                onClick = {
                    onConnect(network.ssid, psk.takeIf { network.secured })
                    psk = ""
                },
                enabled = !network.secured || WifiRules.isValidPsk(psk),
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Connect") }
        }
        HorizontalDivider()
    }
}

@Composable
private fun BleSearchView(ble: BleAccess, deviceName: String, s: WifiStep.BleSearch, vm: WifiViewModel) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(BlePermissions.granted(context)) }
    var enabled by remember { mutableStateOf(ble.isEnabled) }
    val permissions = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        granted = result.values.all { it }
    }
    val enable = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        enabled = ble.isEnabled
    }
    val ready = ble.isSupported && granted && enabled
    LaunchedEffect(ready) { if (ready) vm.startBleScan() }

    Text("Find $deviceName over Bluetooth", style = MaterialTheme.typography.titleMedium)
    Text(
        "No PIN needed: the phone proves it is paired with this device, then you pick its new network.",
        style = MaterialTheme.typography.bodyMedium,
        color = lapis.muted,
    )
    ListeningNote()
    when {
        !ble.isSupported -> ErrorRetry("This phone has no Bluetooth LE.")
        !granted -> Button(onClick = { permissions.launch(BlePermissions.required()) }, modifier = Modifier.fillMaxWidth()) {
            Text("Allow Bluetooth access")
        }

        !enabled -> Button(
            onClick = {
                val allowed = Build.VERSION.SDK_INT < Build.VERSION_CODES.S ||
                    ContextCompat.checkSelfPermission(context, Manifest.permission.BLUETOOTH_CONNECT) ==
                    PackageManager.PERMISSION_GRANTED
                if (allowed) runCatching { enable.launch(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE)) }
            },
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Turn on Bluetooth") }

        s.error != null -> ErrorRetry(s.error, onRetry = vm::startBleScan)
        else -> {
            LoadingRow("Looking for ${s.remembered.ifEmpty { "LapisZero-XXXX or LapisDeck-XXXX" }} nearby…")
            s.devices.forEach { found -> FoundRow(found, remembered = found.name == s.remembered, onSelect = vm::select) }
            if (s.devices.isNotEmpty() && s.remembered.isNotEmpty() && s.devices.none { it.name == s.remembered }) {
                Text(
                    "${s.remembered} is not among these yet. Pick another only if you know it is $deviceName.",
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
            }
        }
    }
    OutlinedButton(onClick = vm::load, modifier = Modifier.fillMaxWidth()) { Text("Try the network instead") }
}

@Composable
private fun FoundRow(found: FoundDevice, remembered: Boolean, onSelect: (FoundDevice) -> Unit) {
    LapisCard(
        Modifier.fillMaxWidth(),
        borderColor = if (remembered) lapis.blue else MaterialTheme.colorScheme.outlineVariant,
        onClick = { onSelect(found) },
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(found.name, style = MaterialTheme.typography.titleMedium)
                Text(found.address, style = LapisType.MonoSmall, color = lapis.muted)
                if (remembered) LapisBadge("this device", color = lapis.blue, filled = true)
            }
            Text("${found.rssi} dBm", style = LapisType.Mono)
        }
        if (remembered) {
            Button(onClick = { onSelect(found) }, modifier = Modifier.fillMaxWidth()) { Text("Connect") }
        }
    }
}
