package dev.lapiszero.app.ui.config

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.provider.Settings
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavHostController
import androidx.navigation.compose.composable
import dev.lapiszero.app.LapisApp
import dev.lapiszero.app.data.remote.DeviceSshKey
import dev.lapiszero.app.data.remote.Host
import dev.lapiszero.app.data.remote.HostSsh
import dev.lapiszero.app.data.remote.HostVnc
import dev.lapiszero.app.data.remote.PairedPhone
import dev.lapiszero.app.data.remote.ProbeCheck
import dev.lapiszero.app.data.remote.ProbeResult
import dev.lapiszero.app.data.remote.Schedule
import dev.lapiszero.app.data.remote.ScheduleCreate
import dev.lapiszero.app.data.remote.SecretStatus
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.domain.SshSupport
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.ThemeMode
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.ChoiceBadge
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.ConfirmDialog
import dev.lapiszero.app.ui.components.ErrorRetry
import dev.lapiszero.app.ui.components.KeyValueRow
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.LoadingRow
import dev.lapiszero.app.ui.components.SectionTitle
import dev.lapiszero.app.ui.components.formatDateTime

object ConfigRoutes {
    const val PREFIX = "config"
    const val MENU = "config"
    const val HOSTS = "config/hosts"
    const val AGENT = "config/agent"
    const val SECRETS = "config/secrets"
    const val SCHEDULES = "config/schedules"
    const val PHONES = "config/phones"
    const val APP = "config/app"

    fun title(route: String): String = when (route) {
        HOSTS -> "KVM hosts"
        AGENT -> "Agent & LLM"
        SECRETS -> "Secrets vault"
        SCHEDULES -> "Scheduled jobs"
        PHONES -> "Paired phones"
        APP -> "App"
        else -> "Config"
    }
}

fun NavGraphBuilder.configGraph(
    nav: NavHostController,
    session: LapisSession,
    snackbar: SnackbarHostState,
    onOpenChat: (String) -> Unit,
) {
    composable(ConfigRoutes.MENU) { ConfigMenu(onOpen = { nav.navigate(it) }) }
    composable(ConfigRoutes.HOSTS) { HostsScreen(configViewModel(session), snackbar) }
    composable(ConfigRoutes.AGENT) { AgentScreen(configViewModel(session), snackbar) }
    composable(ConfigRoutes.SECRETS) { SecretsScreen(configViewModel(session), snackbar) }
    composable(ConfigRoutes.SCHEDULES) { SchedulesScreen(configViewModel(session), snackbar, session, onOpenChat) }
    composable(ConfigRoutes.PHONES) { PhonesScreen(configViewModel(session), snackbar) }
    composable(ConfigRoutes.APP) { AppScreen() }
}

@Composable
private fun configViewModel(session: LapisSession): ConfigViewModel = viewModel(
    factory = viewModelFactory { initializer { ConfigViewModel(session.api, session.repo) } },
)

@Composable
private fun ConfigPage(content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
        content = content,
    )
}

@Composable
private fun ConfigMenu(onOpen: (String) -> Unit) {
    val entries = listOf(
        Triple(ConfigRoutes.HOSTS, "KVM hosts", "Target machines in hosts.toml, with a connection test"),
        Triple(ConfigRoutes.AGENT, "Agent & LLM", "Provider, model, step limits and default policy"),
        Triple(ConfigRoutes.SECRETS, "Secrets vault", "Write-only API keys and VNC passwords"),
        Triple(ConfigRoutes.SCHEDULES, "Scheduled jobs", "Cron prompts, enable/disable, run now"),
        Triple(ConfigRoutes.PHONES, "Paired phones", "See and revoke phones that can control the device"),
        Triple(ConfigRoutes.APP, "App", "Appearance and version of this app"),
    )
    ConfigPage {
        entries.forEach { (route, title, subtitle) ->
            LapisCard(Modifier.fillMaxWidth(), onClick = { onOpen(route) }) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(title, style = MaterialTheme.typography.titleMedium)
                        Text(subtitle, style = MaterialTheme.typography.bodySmall, color = lapis.muted)
                    }
                    Text("›", style = MaterialTheme.typography.headlineSmall, color = lapis.muted)
                }
            }
        }
    }
}

@Composable
private fun <T> SectionBody(section: Section<T>, onRetry: () -> Unit, content: @Composable (T) -> Unit) {
    section.error?.let { ErrorRetry(it, onRetry = onRetry) }
    val data = section.data
    when {
        data != null -> content(data)
        section.loading -> LoadingRow("Loading from the device…")
    }
}

// ---------------------------------------------------------------------------- hosts

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun HostsScreen(vm: ConfigViewModel, snackbar: SnackbarHostState) {
    val hosts by vm.hosts.collectAsStateWithLifecycle()
    val secrets by vm.secrets.collectAsStateWithLifecycle()
    val probes by vm.probes.collectAsStateWithLifecycle()
    val saving by vm.saving.collectAsStateWithLifecycle()
    val failure by vm.lastFailure.collectAsStateWithLifecycle()
    val support by vm.sshSupport.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    LaunchedEffect(vm) {
        vm.loadHosts()
        vm.loadSecrets()
        vm.loadDeviceKey()
    }
    var editing by remember { mutableStateOf<Pair<String?, Host>?>(null) }
    var deleting by remember { mutableStateOf<String?>(null) }
    var authorising by remember { mutableStateOf<String?>(null) }
    var showKey by remember { mutableStateOf(false) }
    // The device's own SSH key: present exactly when its software has the SSH features.
    val deviceKey = (support as? SshSupport.Ready)?.key

    ConfigPage {
        SectionBody(hosts, onRetry = vm::loadHosts) { data ->
            if (data.hosts.isEmpty()) {
                Text("No hosts configured yet.", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
            }
            data.hosts.forEach { (id, host) ->
                LapisCard(
                    Modifier.fillMaxWidth(),
                    borderColor = if (id == data.activeHost) lapis.blue else MaterialTheme.colorScheme.outlineVariant,
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(id, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                        if (id == data.activeHost) LapisBadge("active", color = lapis.blue, filled = true)
                    }
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.padding(vertical = 4.dp)) {
                        LapisBadge(host.os)
                        LapisBadge(host.policy, color = if (host.policy == "autonomous") lapis.amber else lapis.muted)
                    }
                    KeyValueRow("Hostname", host.hostname)
                    KeyValueRow("Tailscale", host.tailscaleIp)
                    KeyValueRow("VNC", "${host.vnc.username.ifEmpty { "-" }}@:${host.vnc.port}")
                    KeyValueRow("SSH", "${host.ssh.user.ifEmpty { "-" }}@:${host.ssh.port}")
                    probes[id]?.let { ProbeView(it) }
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 6.dp)) {
                        Button(onClick = { vm.probe(id) }, enabled = probes[id] !is ProbeUi.Running) {
                            Text("Test connection")
                        }
                        OutlinedButton(
                            onClick = {
                                vm.clearFailure()
                                editing = id to host
                            },
                        ) { Text("Edit") }
                        if (id != data.activeHost) {
                            OutlinedButton(onClick = { vm.setActiveHost(id) }) { Text("Set active") }
                        }
                        if (deviceKey != null) {
                            OutlinedButton(
                                onClick = {
                                    vm.clearFailure()
                                    authorising = id
                                },
                            ) { Text("Authorise device") }
                            TextButton(onClick = { showKey = true }) { Text("Device key") }
                        }
                        TextButton(onClick = { deleting = id }) {
                            Text("Delete", color = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }
            Button(
                onClick = {
                    vm.clearFailure()
                    editing = null to Host()
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Add host") }
            Text(
                "Hosts cannot be changed while a task is running.",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
            // Why "Authorise device" and "Device key" are not on the cards, when they are not.
            val noSshControls = when (val known = support) {
                SshSupport.Missing -> ChatRepository.NO_SSH_FEATURES
                is SshSupport.Unknown -> known.error?.let { "The device's SSH key could not be read: $it" }
                is SshSupport.Ready -> null
            }
            noSshControls?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = lapis.muted) }
        }
    }

    editing?.let { (originalId, host) ->
        HostEditor(
            originalId = originalId,
            initial = host,
            storedSecrets = secrets.data?.filter { it.isSet }?.map { it.name }?.toSet(),
            takenSecrets = hosts.data?.hosts.orEmpty().filterKeys { it != originalId }.values
                .flatMap { listOfNotNull(it.vnc.passwordEnv, it.ssh.passwordEnv) }.toSet(),
            support = support,
            saving = saving,
            error = failure,
            onSave = { id, edited, vncPassword, sshPassword ->
                vm.saveHost(originalId, id, edited, vncPassword, sshPassword) { ok -> if (ok) editing = null }
            },
            onDismiss = { editing = null },
        )
    }
    authorising?.let { id ->
        AlertDialog(
            onDismissRequest = { authorising = null },
            title = { Text("Authorise the device on $id?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(
                        "This adds the device's own SSH key to $id. It logs in once with the user and " +
                            "password saved for this host; afterwards SSH works without the password.",
                    )
                    Text(
                        "For a host that only takes keys, use Device key and run the copied command there.",
                        style = MaterialTheme.typography.bodySmall,
                        color = lapis.muted,
                    )
                    // Shown here because a snackbar would sit behind this dialog.
                    failure?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
                }
            },
            confirmButton = {
                Button(
                    enabled = !saving,
                    // Closed only while it is still this dialog: the answer can take a while.
                    onClick = { vm.authorizeDevice(id) { ok -> if (ok && authorising == id) authorising = null } },
                ) { Text(if (saving) "Authorising…" else "Authorise") }
            },
            dismissButton = { OutlinedButton(onClick = { authorising = null }) { Text("Cancel") } },
        )
    }
    if (showKey && deviceKey != null) DeviceKeyDialog(deviceKey, onDismiss = { showKey = false })
    deleting?.let { id ->
        ConfirmDialog(
            title = "Delete host $id?",
            text = "Chats bound to it stay in the list but can no longer run.",
            confirmLabel = "Delete",
            danger = true,
            onConfirm = { vm.deleteHost(id) },
            onDismiss = { deleting = null },
        )
    }
}

@Composable
private fun ProbeView(probe: ProbeUi) {
    Column(Modifier.padding(top = 6.dp)) {
        when (probe) {
            ProbeUi.Running -> LoadingRow("Testing Tailscale, VNC and SSH…")
            is ProbeUi.Failed -> Text(probe.message, style = LapisType.MonoSmall, color = MaterialTheme.colorScheme.error)
            is ProbeUi.Done -> {
                ProbeLine("Tailscale reachability", probe.result.tailscale)
                ProbeLine("VNC auth", probe.result.vnc)
                ProbeLine("SSH access", probe.result.ssh)
                agentToolsLine(probe.result)?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = lapis.muted) }
            }
        }
    }
}

/** "Agent tools: Claude Code, opencode, Freya", or null when the probe found none. */
private fun agentToolsLine(result: ProbeResult): String? =
    result.agents.takeIf { it.isNotEmpty() }?.joinToString(prefix = "Agent tools: ") { it.name.ifBlank { it.id } }

@Composable
private fun DeviceKeyDialog(key: DeviceSshKey, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val command = ConfigRules.authorizeCommand(key.publicKey)
    var copied by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Device key") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    "The device's own SSH key. Only this public half ever leaves the device.",
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
                SectionTitle("Fingerprint")
                SelectionContainer { Text(key.fingerprint, style = LapisType.MonoSmall) }
                SectionTitle("Public key")
                SelectionContainer { Text(key.publicKey, style = LapisType.MonoSmall) }
                Text(
                    if (command != null) {
                        "A host that only takes keys: run the copied command there once, as the user the device logs in as."
                    } else {
                        "This key is not in the usual form, so no command is offered: select and copy the key itself."
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
            }
        },
        confirmButton = {
            Button(
                enabled = command != null,
                onClick = {
                    // The platform clipboard; a public key is not a secret.
                    context.getSystemService(ClipboardManager::class.java)
                        ?.setPrimaryClip(ClipData.newPlainText("Authorise Lapis-Zero", command))
                    copied = true
                },
            ) { Text(if (copied) "Copied" else "Copy command") }
        },
        dismissButton = { OutlinedButton(onClick = onDismiss) { Text("Close") } },
    )
}

@Composable
private fun ProbeLine(label: String, check: ProbeCheck) {
    Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
        LapisBadge(if (check.ok) "ok" else "fail", color = if (check.ok) lapis.blue else lapis.danger, filled = true)
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyMedium)
            if (check.detail.isNotBlank()) {
                Text(check.detail, style = LapisType.MonoSmall, color = lapis.muted, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
        check.latencyMs?.let { Text("$it ms", style = LapisType.Mono) }
    }
}

@Composable
private fun HostEditor(
    originalId: String?,
    initial: Host,
    storedSecrets: Set<String>?,
    takenSecrets: Set<String>,
    support: SshSupport,
    saving: Boolean,
    error: String?,
    onSave: (String, Host, String?, String?) -> Unit,
    onDismiss: () -> Unit,
) {
    var id by rememberSaveable { mutableStateOf(originalId.orEmpty()) }
    var hostname by rememberSaveable { mutableStateOf(initial.hostname) }
    var tailscale by rememberSaveable { mutableStateOf(initial.tailscaleIp) }
    var os by rememberSaveable { mutableStateOf(initial.os) }
    var policy by rememberSaveable { mutableStateOf(initial.policy) }
    var vncPort by rememberSaveable { mutableStateOf(initial.vnc.port.toString()) }
    var vncUser by rememberSaveable { mutableStateOf(initial.vnc.username) }
    // Deliberately `remember`, not `rememberSaveable`: a password must not land in saved state.
    var vncPassword by remember { mutableStateOf("") }
    var sshPort by rememberSaveable { mutableStateOf(initial.ssh.port.toString()) }
    var sshUser by rememberSaveable { mutableStateOf(initial.ssh.user) }
    // "Same login as the screen": the username and password entered for VNC serve SSH too. On
    // for a new host; an existing one shows what is stored.
    var sameLogin by rememberSaveable {
        mutableStateOf(
            originalId == null || (
                !initial.ssh.passwordEnv.isNullOrEmpty() &&
                    initial.ssh.passwordEnv == initial.vnc.passwordEnv && initial.ssh.user == initial.vnc.username
                ),
        )
    }
    // `remember` for the same reason as the VNC password.
    var sshPassword by remember { mutableStateOf("") }
    var identity by rememberSaveable { mutableStateOf(initial.ssh.identityFile.orEmpty()) }
    val vncPortValue = vncPort.toIntOrNull()?.takeIf { it in 1..65535 }
    val sshPortValue = sshPort.toIntOrNull()?.takeIf { it in 1..65535 }
    val valid = ConfigRules.isValidHostId(id) && hostname.isNotBlank() && tailscale.isNotBlank() &&
        vncPortValue != null && sshPortValue != null
    // The password goes to the vault; the host only records the name it is kept under.
    // With the vault unread, an existing name is trusted only as long as nothing is stored
    // under it from here: a password typed now goes under a derived name, never under a value
    // that may itself be a password left in the old "secret name" field.
    val vncSecret = ConfigRules.vncSecretName(
        id,
        initial.vnc.passwordEnv,
        storedSecrets ?: if (vncPassword.isNotEmpty()) emptySet() else null,
        takenSecrets,
    )
    val vncStored = storedSecrets?.contains(vncSecret) // null: the vault has not been read
    // The SSH login controls need device software that knows `ssh.password_env`.
    val sshFeatures = support is SshSupport.Ready
    val same = sshFeatures && sameLogin && vncUser.isNotBlank()
    // An SSH password of the host's own is kept under a name of its own, by the rules of the
    // VNC one. The name shared with VNC is never taken for it: a password stored there would
    // replace the screen's.
    val ownSshName = initial.ssh.passwordEnv.orEmpty().takeUnless { it == initial.vnc.passwordEnv || it == vncSecret }.orEmpty()
    val sshSecret = ConfigRules.sshSecretName(
        id,
        ownSshName,
        storedSecrets ?: if (sshPassword.isNotEmpty()) emptySet() else null,
        takenSecrets + vncSecret,
    )
    val sshKept = ownSshName.isNotEmpty() && sshSecret == ownSshName
    // What the host records is always a NAME, or nothing: never what was typed as a password.
    val sshEnv = when {
        !sshFeatures -> initial.ssh.passwordEnv // older device software: goes back as it came
        same -> vncSecret
        sshPassword.isNotEmpty() || sshKept -> sshSecret
        else -> "" // key only
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (originalId == null) "Add host" else "Edit $originalId") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                PlainField("Host id (letters, digits, - and _)", id, { id = it.trim() }, isError = id.isNotEmpty() && !ConfigRules.isValidHostId(id))
                PlainField("Display hostname", hostname, { hostname = it })
                PlainField("Tailscale IP or MagicDNS name", tailscale, { tailscale = it.trim() })
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("OS", style = MaterialTheme.typography.bodyMedium)
                    ChoiceBadge(os, listOf("macos", "linux", "windows"), { it }, { os = it })
                    Text("Policy", style = MaterialTheme.typography.bodyMedium)
                    ChoiceBadge(policy, AgentSettings.POLICIES, { it }, { policy = it })
                }
                SectionTitle("VNC")
                PlainField("Port", vncPort, { vncPort = it.filter(Char::isDigit).take(5) }, number = true, isError = vncPortValue == null)
                PlainField("Username", vncUser, { vncUser = it })
                PasswordField(if (vncStored == true) "Password (stored; type to replace)" else "Password", vncPassword, { vncPassword = it })
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    LapisBadge(
                        when (vncStored) {
                            true -> "password stored"
                            false -> "no password stored"
                            null -> "password status unknown"
                        },
                        color = if (vncStored == true) lapis.blue else lapis.amber,
                        filled = vncStored == true,
                    )
                }
                Text(
                    "Kept write-only in the Secrets vault" + (if (storedSecrets != null) " as $vncSecret" else "") + ". " +
                        if (os == "macos") {
                            "For macOS Screen Sharing use the Mac's account name and its login password; " +
                                "or enable \"VNC viewers may control screen with password\" and leave the username empty."
                        } else {
                            "Leave the username empty for a plain VNC password."
                        },
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
                SectionTitle("SSH")
                PlainField("Port", sshPort, { sshPort = it.filter(Char::isDigit).take(5) }, number = true, isError = sshPortValue == null)
                if (sshFeatures && vncUser.isNotBlank()) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("Same login as the screen", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                        Switch(
                            checked = sameLogin,
                            onCheckedChange = {
                                sameLogin = it
                                // Turned off: start from the screen's user rather than from nothing.
                                if (!it && sshUser.isBlank()) sshUser = vncUser.trim()
                            },
                        )
                    }
                }
                if (same) {
                    KeyValueRow("SSH user", vncUser.trim())
                } else {
                    PlainField("User", sshUser, { sshUser = it })
                }
                if (sshFeatures && !same) {
                    PasswordField(
                        if (sshKept && storedSecrets != null) "SSH password (stored; type to replace)" else "SSH password (optional)",
                        sshPassword,
                        { sshPassword = it },
                    )
                }
                val sshHelp = when {
                    support == SshSupport.Missing -> ChatRepository.NO_SSH_FEATURES
                    !sshFeatures -> null // not known yet: the plain fields, without a claim either way
                    else -> when {
                        same -> "SSH uses the screen's username and password. "
                        sshKept -> "The stored password stays until replaced here or removed in the Secrets vault. "
                        else -> "Without a password the device logs in with its own key only. "
                    } + if (os == "macos") "On the Mac, Remote Login must be on (System Settings > General > Sharing)." else "The host must accept SSH logins."
                }
                sshHelp?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = lapis.muted) }
                PlainField("Advanced: identity file on the device (optional)", identity, { identity = it.trim() })
                // Shown here because a snackbar would sit behind this dialog.
                if (error != null) {
                    Text(error, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
            }
        },
        confirmButton = {
            Button(
                enabled = valid && !saving,
                onClick = {
                    onSave(
                        id,
                        Host(
                            hostname = hostname.trim(),
                            tailscaleIp = tailscale,
                            os = os,
                            policy = policy,
                            vnc = HostVnc(vncPortValue ?: 5900, vncUser.trim(), vncSecret),
                            ssh = HostSsh(sshPortValue ?: 22, if (same) vncUser.trim() else sshUser.trim(), identity.ifEmpty { null }, sshEnv),
                        ),
                        vncPassword.ifEmpty { null },
                        // Only a password of the host's own: the shared one is the VNC password above.
                        sshPassword.takeIf { sshFeatures && !same && it.isNotEmpty() },
                    )
                },
            ) { Text(if (saving) "Saving…" else "Save") }
        },
        dismissButton = { OutlinedButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Write-only: masked, never prefilled, and held by the caller in `remember` only. */
@Composable
private fun PasswordField(label: String, value: String, onChange: (String) -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun PlainField(
    label: String,
    value: String,
    onChange: (String) -> Unit,
    number: Boolean = false,
    isError: Boolean = false,
    singleLine: Boolean = true,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = singleLine,
        isError = isError,
        keyboardOptions = KeyboardOptions(
            keyboardType = if (number) KeyboardType.Number else KeyboardType.Text,
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
}

// ---------------------------------------------------------------------------- agent / LLM

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AgentScreen(vm: ConfigViewModel, snackbar: SnackbarHostState) {
    val config by vm.config.collectAsStateWithLifecycle()
    val secrets by vm.secrets.collectAsStateWithLifecycle()
    val saving by vm.saving.collectAsStateWithLifecycle()
    val failure by vm.lastFailure.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    LaunchedEffect(vm) {
        vm.loadConfig()
        vm.loadSecrets()
    }
    var settingKey by remember { mutableStateOf<String?>(null) }

    ConfigPage {
        SectionBody(config, onRetry = vm::loadConfig) { raw ->
            val loaded = remember(raw) { AgentSettings.from(raw) }
            var s by remember(raw) { mutableStateOf(loaded) }
            var maxSteps by remember(raw) { mutableStateOf(loaded.maxSteps.toString()) }
            var timeout by remember(raw) { mutableStateOf(loaded.requestTimeoutSec.toString()) }
            var stuck by remember(raw) { mutableStateOf(loaded.stuckThreshold.toString()) }
            var idle by remember(raw) { mutableStateOf(loaded.vncIdleDisconnectSec.toString()) }
            val maxStepsValue = maxSteps.toIntOrNull()?.takeIf { it in 1..200 }
            val timeoutValue = timeout.toIntOrNull()?.takeIf { it >= 1 }
            val stuckValue = stuck.toIntOrNull()?.takeIf { it >= 1 }
            val idleValue = idle.toIntOrNull()?.takeIf { it >= 0 }
            val numbersOk = maxStepsValue != null && timeoutValue != null && stuckValue != null && idleValue != null
            val edited = s.copy(
                maxSteps = maxStepsValue ?: s.maxSteps,
                requestTimeoutSec = timeoutValue ?: s.requestTimeoutSec,
                stuckThreshold = stuckValue ?: s.stuckThreshold,
                vncIdleDisconnectSec = idleValue ?: s.vncIdleDisconnectSec,
            )
            // The same rule the view model enforces on save.
            val storedNames = secrets.data.orEmpty().filter { it.isSet }.map { it.name }.toSet()
            val problem = ConfigRules.agentProblem(edited, loaded, storedNames)
            val canSave = numbersOk && problem == null
            // Compared as it would be sent: a trailing slash or space is not an unsaved change.
            val sent = edited.copy(model = edited.model.trim(), baseUrl = ConfigRules.normalizeBaseUrl(edited.baseUrl))
            val dirty = !numbersOk || (edited != loaded && sent != loaded)
            // Storing a key may also save the provider it was added for, but nothing else: a
            // half-typed model id or a policy being considered is not saved behind the user's back.
            val choiceChanged = sent.provider != loaded.provider || sent.baseUrl != loaded.baseUrl || sent.apiKeyEnv != loaded.apiKeyEnv
            val onlyChoice = numbersOk &&
                edited.copy(provider = loaded.provider, baseUrl = loaded.baseUrl, apiKeyEnv = loaded.apiKeyEnv, model = loaded.model) == loaded
            val nameProblem = ConfigRules.secretNameProblem(s.apiKeyEnv)
            val baseUrlBad = s.baseUrl != loaded.baseUrl && !ConfigRules.isValidBaseUrl(ConfigRules.normalizeBaseUrl(s.baseUrl))
            // null until the vault has been read: "not stored" is only said when that is known.
            val keyStored = secrets.data?.any { it.name == s.apiKeyEnv && it.isSet }
            val caseTwin = secrets.data
                ?.firstOrNull { it.isSet && it.name != s.apiKeyEnv && it.name.equals(s.apiKeyEnv, ignoreCase = true) }?.name
            val current = ProviderPreset.matching(s)

            SectionTitle("LLM")
            LapisCard(Modifier.fillMaxWidth()) {
                Text("Provider", style = MaterialTheme.typography.bodyMedium)
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                    modifier = Modifier.padding(vertical = 4.dp),
                ) {
                    ProviderPreset.ALL.forEach { preset ->
                        LapisBadge(
                            preset.name,
                            color = if (preset == current) lapis.blue else lapis.muted,
                            filled = preset == current,
                            onClick = { s = preset.applyTo(s) },
                        )
                    }
                }
                Text(
                    "Any other provider with an OpenAI-compatible API works too: set the protocol to " +
                        "openai and fill in its base URL, a model id and a name for its key. The model " +
                        "must accept images and tool calls; the ids filled in are examples.",
                    style = MaterialTheme.typography.bodySmall,
                    color = lapis.muted,
                )
                Spacer(Modifier.height(6.dp))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text("Protocol", style = MaterialTheme.typography.bodyMedium)
                    // Means "that protocol's own endpoint": see ProviderPreset.withProtocol.
                    ChoiceBadge(
                        s.provider, AgentSettings.PROVIDERS, { it },
                        { s = ProviderPreset.withProtocol(s, it) },
                        color = lapis.blue,
                    )
                    Text("Effort", style = MaterialTheme.typography.bodyMedium)
                    ChoiceBadge(s.effort, AgentSettings.EFFORTS, { it }, { s = s.copy(effort = it) })
                }
                Spacer(Modifier.height(6.dp))
                PlainField("Model id", s.model, { s = s.copy(model = it) }, isError = s.model.isBlank())
                PlainField("Base URL (empty = the provider's own)", s.baseUrl, { s = s.copy(baseUrl = it.trim()) }, isError = baseUrlBad)
                // Not tidied into a name as it is typed: a key pasted here must be refused, not
                // turned into something that passes for a name and is then stored in the clear.
                PlainField("Key name (not the key itself)", s.apiKeyEnv, { s = s.copy(apiKeyEnv = it.trim()) }, isError = nameProblem != null)
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    when {
                        keyStored == true -> LapisBadge("key stored", color = lapis.blue, filled = true)
                        keyStored == false -> LapisBadge("key not stored", color = lapis.amber)
                        secrets.error != null -> {
                            LapisBadge("key status unknown", color = lapis.amber)
                            TextButton(onClick = { vm.loadSecrets() }) { Text("Retry") }
                        }
                        else -> LapisBadge("checking key", color = lapis.muted)
                    }
                    Spacer(Modifier.weight(1f))
                    TextButton(
                        enabled = nameProblem == null || keyStored == true,
                        onClick = {
                            vm.clearFailure()
                            settingKey = s.apiKeyEnv
                        },
                    ) {
                        Text(
                            when (keyStored) {
                                true -> "Replace key"
                                false -> "Add key"
                                null -> "Set key"
                            },
                        )
                    }
                }
                // Said where the change is made: the Save button is a screen further down.
                if (dirty) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("Not saved yet.", style = MaterialTheme.typography.bodySmall, color = lapis.amber, modifier = Modifier.weight(1f))
                        TextButton(enabled = canSave && !saving, onClick = { vm.saveSettings(edited) }) { Text("Save now") }
                    }
                }
                PlainField("Request timeout (seconds)", timeout, { timeout = it.filter(Char::isDigit).take(4) }, number = true, isError = timeoutValue == null)
            }
            SectionTitle("Agent loop")
            LapisCard(Modifier.fillMaxWidth()) {
                PlainField("Max steps per task (1-200)", maxSteps, { maxSteps = it.filter(Char::isDigit).take(3) }, number = true, isError = maxStepsValue == null)
                PlainField("Stuck-loop threshold (identical frames)", stuck, { stuck = it.filter(Char::isDigit).take(3) }, number = true, isError = stuckValue == null)
                PlainField("VNC idle disconnect (seconds)", idle, { idle = it.filter(Char::isDigit).take(5) }, number = true, isError = idleValue == null)
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp), modifier = Modifier.padding(top = 6.dp)) {
                    Text("Default approval policy", style = MaterialTheme.typography.bodyMedium)
                    ChoiceBadge(
                        s.approvalDefaultTier, AgentSettings.POLICIES, { it }, { s = s.copy(approvalDefaultTier = it) },
                        color = if (s.approvalDefaultTier == "autonomous") lapis.amber else MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
            Button(
                enabled = !saving && canSave,
                onClick = { vm.saveSettings(edited) },
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (saving) "Saving…" else "Save to lapis.toml") }
            // One thing at a time: what blocks saving first, then what would go wrong after it.
            // A key name is only repeated here once it is known to look like a name.
            val warning = when {
                problem != null -> problem
                !numbersOk -> "Check the numbers marked in red."
                nameProblem != null -> "The key name saved on the device: $nameProblem" +
                    if (ConfigRules.looksLikeSecretValue(s.apiKeyEnv)) " If it is a key, revoke it at the provider." else ""
                // Where the key goes comes before the protocol hint, which is true of any base URL.
                else -> ProviderPreset.strayKey(s) ?: when {
                    s.baseUrl.startsWith("http://", ignoreCase = true) ->
                        "http:// sends the key unencrypted: use it only for a server on your own network."
                    // The combination behind "Task failed: Extra data: line 1 column 5": a non-Anthropic
                    // endpoint called with Anthropic's path answers with a 404 page.
                    s.provider == "anthropic" && s.baseUrl.isNotBlank() ->
                        "Protocol anthropic with a base URL only suits an Anthropic-compatible endpoint. " +
                            "For most other providers set the protocol to openai, or tap the provider above."
                    caseTwin != null -> "The device has a secret named $caseTwin. Names are case-sensitive: ${s.apiKeyEnv} is a different one."
                    keyStored == false -> "The key ${s.apiKeyEnv} is not on the device yet: tap Add key."
                    else -> null
                }
            }
            if (warning != null) Text(warning, style = MaterialTheme.typography.bodySmall, color = lapis.amber)
            Text(
                if (dirty) {
                    "Not saved yet. Saved settings apply to the next chat."
                } else {
                    "Saved settings apply to the next chat. Other sections of lapis.toml are sent back unchanged."
                },
                style = MaterialTheme.typography.bodySmall,
                color = if (dirty) lapis.amber else lapis.muted,
            )

            settingKey?.let { name ->
                SecretEditor(
                    fixedName = name,
                    saving = saving,
                    error = failure,
                    onSave = { n, v ->
                        vm.setSecret(n, v) { ok ->
                            // Only while this dialog is still the one open: after Cancel the user
                            // may have changed the form, and what was captured here is stale.
                            if (ok && settingKey == n) {
                                settingKey = null
                                // The key was added for the provider chosen above. Without this the device
                                // would keep using the old one until Save is found further down the page.
                                if (canSave && choiceChanged && onlyChoice) vm.saveSettings(edited)
                            }
                        }
                    },
                    onDismiss = { settingKey = null },
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------- secrets

@Composable
private fun SecretsScreen(vm: ConfigViewModel, snackbar: SnackbarHostState) {
    val secrets by vm.secrets.collectAsStateWithLifecycle()
    val saving by vm.saving.collectAsStateWithLifecycle()
    val failure by vm.lastFailure.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    LaunchedEffect(vm) { vm.loadSecrets() }
    var setting by remember { mutableStateOf<String?>(null) } // "" = new secret
    var removing by remember { mutableStateOf<String?>(null) }

    ConfigPage {
        Text(
            "Write-only: values are sent to the device and can never be read back. Only whether a " +
                "secret is set, and its last four characters, are shown.",
            style = MaterialTheme.typography.bodySmall,
            color = lapis.muted,
        )
        SectionBody(secrets, onRetry = vm::loadSecrets) { list ->
            list.forEach { secret ->
                SecretRow(
                    secret,
                    onSet = {
                        vm.clearFailure()
                        setting = secret.name
                    },
                    onRemove = { removing = secret.name },
                )
            }
            Button(
                onClick = {
                    vm.clearFailure()
                    setting = ""
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Add a key or secret") }
            Text(
                "Any name works: an API key for any provider, a VNC password, a token. To use a key " +
                    "for chats, choose its provider under Config › Agent & LLM.",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
        }
    }

    setting?.let { name ->
        SecretEditor(
            fixedName = name.ifEmpty { null },
            saving = saving,
            error = failure,
            onSave = { n, v -> vm.setSecret(n, v) { ok -> if (ok) setting = null } },
            onDismiss = { setting = null },
        )
    }
    removing?.let { name ->
        ConfirmDialog(
            title = "Remove $name?",
            text = "The value is deleted from secrets.env on the device.",
            confirmLabel = "Remove",
            danger = true,
            onConfirm = { vm.unsetSecret(name) },
            onDismiss = { removing = null },
        )
    }
}

@Composable
private fun SecretRow(secret: SecretStatus, onSet: () -> Unit, onRemove: () -> Unit) {
    LapisCard(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(secret.name, style = LapisType.Mono, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
            LapisBadge(if (secret.isSet) "set" else "not set", color = if (secret.isSet) lapis.blue else lapis.amber, filled = secret.isSet)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            // Only the masked tail the daemon chose to reveal; never a value.
            Text(secret.masked ?: "", style = LapisType.Mono, color = lapis.muted, modifier = Modifier.weight(1f))
            TextButton(onClick = onSet) { Text(if (secret.isSet) "Replace" else "Set") }
            if (secret.isSet) {
                TextButton(onClick = onRemove) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            }
        }
    }
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
private fun SecretEditor(
    fixedName: String?,
    saving: Boolean,
    error: String?,
    onSave: (String, String) -> Unit,
    onDismiss: () -> Unit,
) {
    // Deliberately `remember`, not `rememberSaveable`: a secret must not land in saved state.
    var typed by remember { mutableStateOf("") }
    var value by remember { mutableStateOf("") }
    // What is typed is left alone (rewriting text under a composing keyboard garbles it); the
    // name it becomes is shown underneath. New names are upper case, like the ones the
    // provider buttons give, so "minimax api key" and the MiniMax button mean the same secret.
    val name = fixedName ?: ConfigRules.normalizeSecretName(typed).uppercase()
    val nameProblem = when {
        fixedName != null || typed.isBlank() -> null
        ConfigRules.looksLikeSecretValue(typed) -> ConfigRules.NOT_A_NAME
        else -> ConfigRules.secretNameProblem(name)
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (fixedName == null) "Add a key or secret" else "Set $fixedName") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                if (fixedName == null) {
                    // One tap for the usual provider key names; any other name can be typed.
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        ProviderPreset.ALL.forEach { preset ->
                            LapisBadge(
                                preset.name,
                                color = if (name == preset.keyEnv) lapis.blue else lapis.muted,
                                onClick = { typed = preset.keyEnv },
                            )
                        }
                    }
                    PlainField("Name, e.g. OPENROUTER_API_KEY", typed, { typed = it }, isError = nameProblem != null)
                    Text(
                        when {
                            nameProblem != null -> nameProblem
                            typed.isNotBlank() && name != typed -> "Stored as $name."
                            else -> "A name, not the key: letters, digits and _."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = if (nameProblem != null) MaterialTheme.colorScheme.error else lapis.muted,
                    )
                }
                OutlinedTextField(
                    value = value,
                    onValueChange = { value = it },
                    label = { Text("Value") },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
                Text("It is sent over the encrypted connection and not kept on this phone.", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
                // Shown here because a snackbar would sit behind this dialog.
                if (error != null) {
                    Text(error, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
            }
        },
        confirmButton = {
            Button(
                enabled = !saving && value.isNotBlank() && nameProblem == null && ConfigRules.isValidSecretName(name),
                onClick = { onSave(name, value) },
            ) { Text(if (saving) "Saving…" else "Store") }
        },
        dismissButton = { OutlinedButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// ---------------------------------------------------------------------------- schedules

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SchedulesScreen(
    vm: ConfigViewModel,
    snackbar: SnackbarHostState,
    session: LapisSession,
    onOpenChat: (String) -> Unit,
) {
    val schedules by vm.schedules.collectAsStateWithLifecycle()
    val saving by vm.saving.collectAsStateWithLifecycle()
    val state by session.repo.state.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    LaunchedEffect(vm) {
        vm.loadSchedules()
        vm.openChat.collect(onOpenChat)
    }
    var editing by remember { mutableStateOf<Pair<String?, Schedule>?>(null) }
    var deleting by remember { mutableStateOf<String?>(null) }
    val hosts = state?.hosts.orEmpty()

    ConfigPage {
        state?.nextCron?.takeIf { it.isNotBlank() }?.let { KeyValueRow("Next job", it) }
        SectionBody(schedules, onRetry = vm::loadSchedules) { list ->
            if (list.isEmpty()) Text("No scheduled jobs.", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
            list.forEach { job ->
                LapisCard(Modifier.fillMaxWidth()) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(job.name, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                        Switch(checked = job.enabled, onCheckedChange = { vm.setScheduleEnabled(job.name, it) }, enabled = !saving)
                    }
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.padding(vertical = 2.dp)) {
                        LapisBadge(job.schedule)
                        LapisBadge(job.host, color = lapis.blue)
                    }
                    Text(job.prompt, style = MaterialTheme.typography.bodyMedium, maxLines = 3, overflow = TextOverflow.Ellipsis)
                    KeyValueRow("Next run", if (job.enabled) formatDateTime(job.nextRun) else "disabled")
                    KeyValueRow("Last run", formatDateTime(job.lastRunAt))
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 4.dp)) {
                        Button(onClick = { vm.runSchedule(job.name) }, enabled = !saving) { Text("Run now") }
                        job.lastChatId?.let { chatId ->
                            OutlinedButton(onClick = { onOpenChat(chatId) }) { Text("Last result") }
                        }
                        OutlinedButton(onClick = { editing = job.name to job }) { Text("Edit") }
                        TextButton(onClick = { deleting = job.name }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
                    }
                }
            }
            Button(
                onClick = { editing = null to Schedule(name = "", schedule = "0 8 * * *", host = hosts.firstOrNull().orEmpty()) },
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Add job") }
        }
    }

    editing?.let { (originalName, job) ->
        ScheduleEditor(
            originalName = originalName,
            initial = job,
            hosts = hosts,
            saving = saving,
            onSave = { edited -> vm.saveSchedule(originalName, edited) { ok -> if (ok) editing = null } },
            onDismiss = { editing = null },
        )
    }
    deleting?.let { name ->
        ConfirmDialog(
            title = "Delete job $name?",
            text = "The schedule is removed from lapis.toml.",
            confirmLabel = "Delete",
            danger = true,
            onConfirm = { vm.deleteSchedule(name) },
            onDismiss = { deleting = null },
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ScheduleEditor(
    originalName: String?,
    initial: Schedule,
    hosts: List<String>,
    saving: Boolean,
    onSave: (ScheduleCreate) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf(initial.name) }
    var cron by rememberSaveable { mutableStateOf(initial.schedule) }
    var host by rememberSaveable { mutableStateOf(initial.host) }
    var prompt by rememberSaveable { mutableStateOf(initial.prompt) }
    var enabled by rememberSaveable { mutableStateOf(initial.enabled) }
    val cronOk = ConfigRules.isPlausibleCron(cron)

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (originalName == null) "Add job" else "Edit $originalName") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                PlainField("Name", name, { name = it.take(60) })
                PlainField("Cron: minute hour day month weekday", cron, { cron = it }, isError = !cronOk)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    ConfigRules.CRON_PRESETS.forEach { (label, expression) ->
                        LapisBadge(label, color = if (cron.trim() == expression) lapis.blue else lapis.muted, onClick = { cron = expression })
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text("Host", style = MaterialTheme.typography.bodyMedium)
                    ChoiceBadge(host.ifEmpty { "choose" }, hosts, { it }, { host = it }, color = lapis.blue, enabled = hosts.isNotEmpty())
                }
                PlainField("Prompt", prompt, { prompt = it }, singleLine = false)
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Enabled", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                    Switch(checked = enabled, onCheckedChange = { enabled = it })
                }
            }
        },
        confirmButton = {
            Button(
                enabled = !saving && name.isNotBlank() && prompt.isNotBlank() && host.isNotBlank() && cronOk,
                onClick = { onSave(ScheduleCreate(name.trim(), cron.trim(), host, prompt.trim(), enabled)) },
            ) { Text(if (saving) "Saving…" else "Save") }
        },
        dismissButton = { OutlinedButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// ---------------------------------------------------------------------------- paired phones

@Composable
private fun PhonesScreen(vm: ConfigViewModel, snackbar: SnackbarHostState) {
    val phones by vm.phones.collectAsStateWithLifecycle()
    val saving by vm.saving.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    LaunchedEffect(vm) { vm.loadPhones() }
    var revoking by remember { mutableStateOf<PairedPhone?>(null) }

    ConfigPage {
        SectionBody(phones, onRetry = vm::loadPhones) { list ->
            list.forEach { phone ->
                LapisCard(Modifier.fillMaxWidth(), borderColor = if (phone.current) lapis.blue else MaterialTheme.colorScheme.outlineVariant) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(phone.name.ifBlank { phone.id }, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                        if (phone.current) LapisBadge("this phone", color = lapis.blue, filled = true)
                        if (phone.revoked) LapisBadge("revoked", color = lapis.danger)
                    }
                    if (phone.model.isNotBlank()) KeyValueRow("Model", phone.model)
                    KeyValueRow("Id", phone.id)
                    KeyValueRow("Paired", formatDateTime(phone.createdAt))
                    KeyValueRow("Last seen", formatDateTime(phone.lastSeenAt))
                    if (!phone.revoked) {
                        TextButton(onClick = { revoking = phone }, enabled = !saving) {
                            Text("Revoke access", color = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }
        }
    }
    revoking?.let { phone ->
        ConfirmDialog(
            title = "Revoke ${phone.name.ifBlank { phone.id }}?",
            text = if (phone.current) {
                "This is the phone in your hand: it loses access immediately and has to be paired again."
            } else {
                "Its token stops working immediately."
            },
            confirmLabel = "Revoke",
            danger = true,
            onConfirm = { vm.revokePhone(phone.id) },
            onDismiss = { revoking = null },
        )
    }
}

// ---------------------------------------------------------------------------- app

@Composable
private fun AppScreen() {
    val app = LocalContext.current.applicationContext as LapisApp
    val mode by app.container.settings.themeMode.collectAsStateWithLifecycle()
    ConfigPage {
        SectionTitle("Appearance")
        LapisCard(Modifier.fillMaxWidth()) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf(ThemeMode.SYSTEM to "System", ThemeMode.PAPER to "Paper", ThemeMode.INK to "Ink").forEach { (m, label) ->
                    if (mode == m) {
                        Button(onClick = {}) { Text(label) }
                    } else {
                        OutlinedButton(onClick = { app.container.settings.setThemeMode(m) }) { Text(label) }
                    }
                }
            }
        }
        SectionTitle("Approval alerts")
        LapisCard(Modifier.fillMaxWidth()) {
            val context = LocalContext.current
            var allowed by remember { mutableStateOf(app.container.notifier.canPost()) }
            LifecycleResumeEffect(Unit) {
                allowed = app.container.notifier.canPost()
                onPauseOrDispose { }
            }
            KeyValueRow("Notifications", if (allowed) "allowed" else "blocked", valueColor = if (allowed) lapis.blue else lapis.amber)
            Text(
                "While a task runs, pending approvals are announced with Approve once / Deny buttons " +
                    "when the app is in the background.",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
            OutlinedButton(
                onClick = {
                    val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                        .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    runCatching { context.startActivity(intent) }
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Notification settings") }
        }
        SectionTitle("About")
        LapisCard(Modifier.fillMaxWidth()) {
            KeyValueRow("Version", dev.lapiszero.app.BuildConfig.VERSION_NAME)
            KeyValueRow("Protocol", "REST/WS v1 · lapis-ble-v1")
            Text(
                "Device hosts and tokens are stored encrypted with a key in the Android Keystore.",
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
            )
        }
    }
}
