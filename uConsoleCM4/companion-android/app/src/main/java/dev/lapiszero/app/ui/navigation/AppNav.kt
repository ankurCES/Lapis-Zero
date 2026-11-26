package dev.lapiszero.app.ui.navigation

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import dev.lapiszero.app.AppContainer
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.DeckGate
import dev.lapiszero.app.domain.DevicesState
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.domain.Notice
import dev.lapiszero.app.domain.PairedDevice
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.chats.ApprovalViewModel
import dev.lapiszero.app.ui.chats.ChatListScreen
import dev.lapiszero.app.ui.chats.SessionsScreen
import dev.lapiszero.app.ui.components.ApprovalCard
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.ConfirmDialog
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisIcons
import dev.lapiszero.app.ui.components.StatusDot
import dev.lapiszero.app.ui.components.connectionColor
import dev.lapiszero.app.ui.components.connectionLabel
import dev.lapiszero.app.ui.config.ConfigRoutes
import dev.lapiszero.app.ui.config.configGraph
import dev.lapiszero.app.ui.deck.DeckScreen
import dev.lapiszero.app.ui.device.DeviceScreen
import dev.lapiszero.app.ui.kvm.KvmScreen
import dev.lapiszero.app.ui.onboarding.OnboardingFlow
import kotlinx.coroutines.launch

object Routes {
    const val CHATS = "chats"
    const val CHAT = "chat/{chatId}"
    const val KVM = "kvm"
    const val DEVICE = "device"

    /** Lapis-Deck only: rails, GPS, mesh, SDR, NetOps, live feed, audit. */
    const val DECK = "deck"
    const val CONFIG = ConfigRoutes.MENU

    fun chat(chatId: String) = "chat/$chatId"
}

private data class Tab(val route: String, val label: String, val icon: ImageVector)

/** Decides between onboarding and the paired workspace. */
@Composable
fun LapisRoot(container: AppContainer) {
    val devices by container.devices.state.collectAsStateWithLifecycle()
    val session by container.sessions.current.collectAsStateWithLifecycle()
    // Stays true through a whole pairing flow, even after the device is stored mid-wizard.
    var pairing by rememberSaveable { mutableStateOf(devices.usable.isEmpty()) }
    LaunchedEffect(devices.usable.isEmpty()) {
        if (devices.usable.isEmpty()) pairing = true
    }

    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        val current = session
        when {
            pairing -> OnboardingFlow(
                container = container,
                canCancel = devices.usable.isNotEmpty(),
                onFinished = { pairing = false },
            )

            current == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }

            // A different device means different chats, hosts and view models: start over.
            else -> key(current.device.id) {
                MainShell(container, current, devices, onPairAnother = { pairing = true })
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MainShell(
    container: AppContainer,
    session: LapisSession,
    devices: DevicesState,
    onPairAnother: () -> Unit,
) {
    val nav = rememberNavController()
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route ?: Routes.CHATS
    val state by session.repo.state.collectAsStateWithLifecycle()
    val connection by session.repo.connection.collectAsStateWithLifecycle()
    var switcher by rememberSaveable { mutableStateOf(false) }
    // Deck UI exists only for a Lapis-Deck; a Lapis-Zero never sees it.
    val isDeck = DeckGate.isDeck(state, session.device)
    val deckUnseen by session.deck.unseen.collectAsStateWithLifecycle()

    val approvalsVm: ApprovalViewModel = viewModel(
        key = "approvals-${session.device.id}",
        factory = viewModelFactory { initializer { ApprovalViewModel(session.repo) } },
    )
    val approvals by approvalsVm.ui.collectAsStateWithLifecycle()
    CollectMessages(approvalsVm.messages, snackbar)
    NotificationPermission(container)

    // Device toasts ("stuck loop", "VNC disconnected") and task results.
    LaunchedEffect(session) {
        session.repo.notices.collect { notice ->
            when (notice) {
                is Notice.Toast -> snackbar.showSnackbar(notice.text)
                is Notice.TaskDone -> snackbar.showSnackbar(
                    "Task ${notice.finished.status}: ${notice.finished.summary}".take(140),
                )
            }
        }
    }

    // Tapping an approval notification lands in the chat it belongs to.
    val navRequest by container.navRequests.collectAsStateWithLifecycle()
    LaunchedEffect(navRequest, session.device.id) {
        val request = navRequest ?: return@LaunchedEffect
        if (request.deviceId != null && request.deviceId != session.device.id) {
            container.devices.setActive(request.deviceId) // this shell is replaced; the request stays
            return@LaunchedEffect
        }
        container.navRequests.value = null
        request.chatId?.let { nav.navigate(Routes.chat(it)) { launchSingleTop = true } }
    }

    val tabs = remember(isDeck) { shellTabs(isDeck) }
    val topLevel = tabs.any { it.route == route }
    // Approvals pop up by themselves; "Later" hides the sheet until the set of requests changes.
    var snoozed by remember { mutableStateOf<Set<String>>(emptySet()) }
    val pendingIds = approvals.approvals.mapTo(LinkedHashSet()) { it.requestId }
    val sheetOpen = pendingIds.isNotEmpty() && !snoozed.containsAll(pendingIds)

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    if (!topLevel) {
                        IconButton(onClick = { nav.popBackStack() }) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                        }
                    }
                },
                title = {
                    if (topLevel) {
                        DevicePill(session.device, connection, isDeck, onClick = { switcher = true })
                    } else {
                        Text(titleFor(route), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                },
                actions = {
                    if (pendingIds.isNotEmpty() && !sheetOpen) {
                        LapisBadge(
                            "${pendingIds.size} approval" + if (pendingIds.size > 1) "s" else "",
                            color = lapis.amber,
                            filled = true,
                            onClick = { snoozed = emptySet() },
                        )
                        Spacer(Modifier.width(8.dp))
                    }
                    if (state?.isRunning == true) {
                        KillButton(onClick = { scope.launch { session.repo.killAll() } })
                        Spacer(Modifier.width(8.dp))
                    }
                },
            )
        },
        bottomBar = {
            if (!route.startsWith("chat/")) {
                NavigationBar {
                    tabs.forEach { tab ->
                        val selected = route == tab.route ||
                            (tab.route == Routes.CONFIG && route.startsWith(ConfigRoutes.PREFIX))
                        NavigationBarItem(
                            selected = selected,
                            onClick = {
                                nav.navigate(tab.route) {
                                    popUpTo(Routes.CHATS) { saveState = true }
                                    launchSingleTop = true
                                    restoreState = true
                                }
                            },
                            icon = {
                                if (tab.route == Routes.DECK && deckUnseen > 0) {
                                    BadgedBox(badge = { Badge { Text(if (deckUnseen > 99) "99+" else deckUnseen.toString()) } }) {
                                        Icon(tab.icon, contentDescription = null)
                                    }
                                } else {
                                    Icon(tab.icon, contentDescription = null)
                                }
                            },
                            label = { Text(tab.label) },
                        )
                    }
                }
            }
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        // Consumed, so imePadding / navigationBarsPadding further down only add what is left.
        Column(Modifier.padding(padding).consumeWindowInsets(padding).fillMaxSize()) {
            ConnectionBanner(connection, state, onRetry = { session.repo.reconnectNow() }, onRepair = onPairAnother)
            ShellNavHost(nav, session, snackbar, approvalsVm, isDeck, Modifier.weight(1f))
        }
    }

    if (sheetOpen) {
        val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(onDismissRequest = { snoozed = pendingIds }, sheetState = sheetState) {
            Column(
                Modifier
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp)
                    .navigationBarsPadding(),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Approval needed", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    TextButton(onClick = { snoozed = pendingIds }) { Text("Later") }
                }
                Text(
                    "The agent is paused until you decide here or on the device.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = lapis.muted,
                )
                approvals.approvals.forEach { approval ->
                    ApprovalCard(
                        approval = approval,
                        shots = session.shots,
                        busy = approval.requestId in approvals.busy,
                        onDecide = { decision, reason -> approvalsVm.decide(approval.requestId, decision, reason) },
                    )
                }
                Spacer(Modifier.height(12.dp))
            }
        }
    }

    if (switcher) {
        DeviceSwitcherSheet(
            devices = devices,
            activeId = session.device.id,
            onSelect = {
                switcher = false
                container.devices.setActive(it)
            },
            onRename = { id, name -> container.devices.rename(id, name) },
            onRemove = { id -> scope.launch { container.sessions.forget(id) } },
            onPairAnother = {
                switcher = false
                onPairAnother()
            },
            onDismiss = { switcher = false },
        )
    }
}

/**
 * Android 13+: ask once for POST_NOTIFICATIONS, when the workspace first opens. Without it the
 * approval alerts cannot be shown while the app is in the background (Config -> App tells the
 * user so and links to the system setting).
 */
@Composable
private fun NotificationPermission(container: AppContainer) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    val context = LocalContext.current
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    LaunchedEffect(Unit) {
        val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        if (!granted && !container.settings.notificationsAsked) {
            container.settings.markNotificationsAsked()
            launcher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }
}

@Composable
private fun ShellNavHost(
    nav: NavHostController,
    session: LapisSession,
    snackbar: SnackbarHostState,
    approvals: ApprovalViewModel,
    isDeck: Boolean,
    modifier: Modifier,
) {
    NavHost(nav, startDestination = Routes.CHATS, modifier = modifier) {
        composable(Routes.CHATS) {
            ChatListScreen(session, snackbar, onOpenChat = { nav.navigate(Routes.chat(it)) })
        }
        composable(Routes.CHAT) { entry ->
            val chatId = entry.arguments?.getString("chatId").orEmpty()
            SessionsScreen(session, chatId, snackbar, approvals, onAllClosed = { nav.popBackStack() })
        }
        composable(Routes.KVM) { KvmScreen(session, snackbar) }
        composable(Routes.DEVICE) { DeviceScreen(session, snackbar, isDeck) }
        composable(Routes.DECK) {
            if (isDeck) {
                DeckScreen(session, snackbar)
            } else {
                // A Lapis-Zero has no deck: nothing here, and no way in from the tabs either.
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text("This device is not a Lapis-Deck", color = lapis.muted)
                }
            }
        }
        configGraph(nav, session, snackbar, onOpenChat = { nav.navigate(Routes.chat(it)) })
    }
}

/** The bottom bar: the Deck tab only for a Lapis-Deck. */
internal fun shellTabRoutes(isDeck: Boolean): List<String> = listOfNotNull(
    Routes.CHATS,
    Routes.KVM,
    Routes.DEVICE,
    Routes.DECK.takeIf { isDeck },
    Routes.CONFIG,
)

private fun shellTabs(isDeck: Boolean): List<Tab> = shellTabRoutes(isDeck).map { route ->
    when (route) {
        Routes.CHATS -> Tab(route, "Chats", LapisIcons.Chat)
        Routes.KVM -> Tab(route, "Live KVM", LapisIcons.Monitor)
        Routes.DEVICE -> Tab(route, "Device", LapisIcons.Chip)
        Routes.DECK -> Tab(route, "Deck", LapisIcons.Deck)
        else -> Tab(route, "Config", Icons.Default.Settings)
    }
}

private fun titleFor(route: String): String = when {
    route.startsWith("chat/") -> "Sessions"
    else -> ConfigRoutes.title(route)
}

@Composable
private fun DevicePill(device: PairedDevice, connection: ConnectionState, isDeck: Boolean, onClick: () -> Unit) {
    Surface(
        onClick = onClick,
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
    ) {
        Row(Modifier.padding(horizontal = 12.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            StatusDot(connectionColor(connection), pulsing = connection == ConnectionState.CONNECTING)
            Spacer(Modifier.width(8.dp))
            Column {
                Text(
                    device.name,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    connectionLabel(connection) + if (isDeck) " · deck" else "",
                    style = LapisType.MonoSmall,
                    color = lapis.muted,
                )
            }
            Spacer(Modifier.width(6.dp))
            Text("▾", style = MaterialTheme.typography.titleSmall)
        }
    }
}

/** Global kill switch, visible on every tab while the agent is running. */
@Composable
private fun KillButton(onClick: () -> Unit) {
    Button(
        onClick = onClick,
        colors = ButtonDefaults.buttonColors(
            containerColor = MaterialTheme.colorScheme.error,
            contentColor = MaterialTheme.colorScheme.onError,
        ),
        shape = MaterialTheme.shapes.small,
    ) { Text("KILL", style = LapisType.MonoLabel) }
}

@Composable
private fun ConnectionBanner(
    connection: ConnectionState,
    state: DeviceState?,
    onRetry: () -> Unit,
    onRepair: () -> Unit,
) {
    val (text, action, onAction) = when (connection) {
        ConnectionState.ONLINE, ConnectionState.CONNECTING -> return
        ConnectionState.OFFLINE -> Triple(
            if (state == null) {
                "Cannot reach the device. Is Tailscale connected on this phone?"
            } else {
                "Connection lost. Showing the last known state; reconnecting…"
            },
            "Retry",
            onRetry,
        )

        ConnectionState.UNAUTHORIZED -> Triple(
            "This phone is no longer paired with the device (its access was revoked).",
            "Pair again",
            onRepair,
        )
    }
    Row(
        Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(horizontal = 16.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onErrorContainer,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = onAction) { Text(action) }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DeviceSwitcherSheet(
    devices: DevicesState,
    activeId: String,
    onSelect: (String) -> Unit,
    onRename: (String, String) -> Unit,
    onRemove: (String) -> Unit,
    onPairAnother: () -> Unit,
    onDismiss: () -> Unit,
) {
    var removing by remember { mutableStateOf<PairedDevice?>(null) }
    var renaming by remember { mutableStateOf<PairedDevice?>(null) }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .navigationBarsPadding(),
        ) {
            Text("Paired devices", style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(8.dp))
            devices.usable.forEach { device ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable { onSelect(device.id) }
                        .padding(vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    StatusDot(if (device.id == activeId) lapis.blue else MaterialTheme.colorScheme.outlineVariant)
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(device.name, style = MaterialTheme.typography.titleMedium)
                            DeviceKind.label(device.kind).takeIf { it.isNotEmpty() }?.let {
                                Spacer(Modifier.width(6.dp))
                                LapisBadge(it, color = if (device.isDeck) lapis.amber else lapis.muted)
                            }
                        }
                        Text(
                            (if (device.tls) "https://" else "http://") + device.host,
                            style = LapisType.MonoSmall,
                            color = lapis.muted,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    TextButton(onClick = { renaming = device }) { Text("Rename") }
                    TextButton(onClick = { removing = device }) {
                        Text("Remove", color = MaterialTheme.colorScheme.error)
                    }
                }
                HorizontalDivider()
            }
            Spacer(Modifier.height(12.dp))
            OutlinedButton(onClick = onPairAnother, modifier = Modifier.fillMaxWidth()) {
                Text("Pair another device")
            }
            Spacer(Modifier.height(16.dp))
        }
    }
    removing?.let { device ->
        ConfirmDialog(
            title = "Remove ${device.name}?",
            text = "Its token is deleted from this phone. To also revoke it on the device, use " +
                "Config -> Paired phones before removing.",
            confirmLabel = "Remove",
            danger = true,
            onConfirm = { onRemove(device.id) },
            onDismiss = { removing = null },
        )
    }
    renaming?.let { device ->
        var name by remember(device.id) { mutableStateOf(device.name) }
        androidx.compose.material3.AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename device") },
            text = {
                OutlinedTextField(value = name, onValueChange = { name = it.take(40) }, singleLine = true)
            },
            confirmButton = {
                Button(
                    onClick = {
                        onRename(device.id, name)
                        renaming = null
                    },
                    enabled = name.isNotBlank(),
                ) { Text("Save") }
            },
            dismissButton = { OutlinedButton(onClick = { renaming = null }) { Text("Cancel") } },
        )
    }
}
