package dev.lapiszero.app.ui.chats

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
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
import dev.lapiszero.app.data.remote.Chat
import dev.lapiszero.app.data.remote.HostAgent
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.AgentLogo
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.StatusDot

/**
 * Open chats as tabs, one per session: a tool on a host (Claude Code on the Mac mini) or the
 * device's own agent. `chatId` is the chat this screen was opened on; `onAllClosed` leaves it.
 */
@Composable
fun SessionsScreen(
    session: LapisSession,
    chatId: String,
    snackbar: SnackbarHostState,
    approvals: ApprovalViewModel,
    onAllClosed: () -> Unit,
) {
    val vm: ChatListViewModel = viewModel(
        key = "sessions",
        factory = viewModelFactory { initializer { ChatListViewModel(session.repo) } },
    )
    val ui by vm.ui.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)
    var selected by rememberSaveable { mutableStateOf(chatId) }
    var opened by rememberSaveable { mutableStateOf<String?>(null) }
    var picking by remember { mutableStateOf(false) }
    // The chat asked for (from the list or a notification) opens as a tab and comes to the front.
    LaunchedEffect(chatId) {
        session.repo.openTab(chatId)
        if (opened != chatId) {
            selected = chatId
            opened = chatId
        }
    }
    LaunchedEffect(vm) { vm.created.collect { selected = it } }

    Column(Modifier.fillMaxSize()) {
        TabStrip(
            tabs = ui.tabs,
            selected = selected,
            onSelect = { selected = it },
            onClose = { id ->
                val ids = ui.tabs.map { it.chat.id }
                val next = ids.getOrNull(ids.indexOf(id) - 1) ?: ids.firstOrNull { it != id }
                vm.closeTab(id)
                when {
                    id != selected -> Unit
                    next != null -> selected = next
                    else -> onAllClosed()
                }
            },
            onNew = { picking = true },
        )
        // One thread at a time; each keeps its own draft and scroll while another is in front.
        key(selected) { ChatThreadScreen(session, selected, snackbar, approvals) }
    }

    if (picking) {
        NewSessionSheet(
            ui = ui,
            onPick = { host, agent ->
                picking = false
                vm.createChat(host, agent)
            },
            onDismiss = { picking = false },
        )
    }
}

@Composable
private fun TabStrip(
    tabs: List<ChatRow>,
    selected: String,
    onSelect: (String) -> Unit,
    onClose: (String) -> Unit,
    onNew: () -> Unit,
) {
    val list = rememberLazyListState()
    val index = tabs.indexOfFirst { it.chat.id == selected }
    LaunchedEffect(index) { if (index >= 0) list.animateScrollToItem(index) }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
            LazyRow(
                state = list,
                modifier = Modifier.weight(1f),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(start = 8.dp),
            ) {
                itemsIndexed(tabs, key = { _, row -> row.chat.id }) { _, row ->
                    SessionTab(row, row.chat.id == selected, onClick = { onSelect(row.chat.id) }, onClose = { onClose(row.chat.id) })
                }
            }
            IconButton(onClick = onNew) { Icon(Icons.Default.Add, contentDescription = "New tab") }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

@Composable
private fun SessionTab(row: ChatRow, selected: Boolean, onClick: () -> Unit, onClose: () -> Unit) {
    val chat = row.chat
    val shape = RoundedCornerShape(topStart = 10.dp, topEnd = 10.dp)
    Column(Modifier.padding(top = 6.dp).width(IntrinsicSize.Max)) {
        Row(
            Modifier
                .fillMaxWidth()
                .background(if (selected) MaterialTheme.colorScheme.surfaceContainerHigh else MaterialTheme.colorScheme.surface, shape)
                .clickable(onClickLabel = "Open session", onClick = onClick)
                .padding(start = 10.dp, top = 2.dp, bottom = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            AgentLogo(chat.agent, size = 16.dp)
            Spacer(Modifier.width(8.dp))
            Text(
                tabTitle(row),
                style = MaterialTheme.typography.labelLarge,
                color = if (selected) MaterialTheme.colorScheme.onSurface else lapis.muted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 132.dp),
            )
            when {
                row.pendingApprovals > 0 -> { Spacer(Modifier.width(6.dp)); StatusDot(lapis.amber, size = 7.dp) }
                row.active -> { Spacer(Modifier.width(6.dp)); StatusDot(lapis.blue, pulsing = true, size = 7.dp) }
            }
            IconButton(onClick = onClose, modifier = Modifier.size(32.dp)) {
                Icon(Icons.Default.Close, contentDescription = "Close tab", modifier = Modifier.size(14.dp), tint = lapis.muted)
            }
        }
        // The selected tab is underlined across its full width.
        Box(
            Modifier
                .fillMaxWidth()
                .height(2.dp)
                .background(if (selected) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.surface),
        )
    }
}

fun tabTitle(row: ChatRow): String = row.chat.title.ifBlank { row.agentName ?: "Lapis agent" }

/**
 * Where a new session runs and with what: each host with the device's own agent and the agent
 * tools found on it, by their logos.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewSessionSheet(ui: ChatListViewModel.Ui, onPick: (host: String, agent: String?) -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .navigationBarsPadding(),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text("Start a session", style = MaterialTheme.typography.titleLarge)
            if (ui.hosts.isEmpty()) {
                Text("Add a host under Config first.", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
            }
            ui.hosts.forEach { host ->
                HostTools(host, active = host == ui.activeHost, tools = ui.agents[host], onPick = { onPick(host, it) })
            }
            Spacer(Modifier.height(8.dp))
        }
    }
}

/**
 * One host: its name, then a tile per agent that can work on it. `compact`: one scrolling line of
 * small pills (the chat list), instead of wrapping tiles and the reason a host has no tools (the
 * sheet; a thread on that host says it too).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun HostTools(
    host: String,
    active: Boolean,
    tools: dev.lapiszero.app.domain.HostAgentsState?,
    onPick: (agent: String?) -> Unit,
    modifier: Modifier = Modifier,
    compact: Boolean = false,
) {
    val all = listOf(RunWith.LAPIS) + tools?.agents.orEmpty()
    val pick = { tool: HostAgent -> onPick(tool.id.takeIf { it != Chat.AGENT_LAPIS }) }
    if (compact) {
        // One line: the host's name, then its agents; it scrolls sideways when they do not fit.
        Row(
            modifier.horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(host, style = LapisType.Mono, color = if (active) lapis.blue else lapis.muted)
            all.forEach { tool -> ToolTile(tool, compact = true, onClick = { pick(tool) }) }
            if (tools?.loading == true) Text("…", style = LapisType.Mono, color = lapis.muted)
        }
        return
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(host, style = LapisType.Mono)
            if (active) {
                Spacer(Modifier.width(8.dp))
                Text("ACTIVE", style = LapisType.MonoLabel, color = lapis.blue)
            }
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            all.forEach { tool -> ToolTile(tool, compact = false, onClick = { pick(tool) }) }
        }
        // Device software without tools has no list (null): the Lapis agent is all there is.
        when {
            tools?.loading == true -> Text("Looking for agent tools…", style = MaterialTheme.typography.bodySmall, color = lapis.muted)
            tools?.reason != null -> Text(tools.reason, style = MaterialTheme.typography.bodySmall, color = lapis.muted, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
    }
}

@Composable
private fun ToolTile(tool: HostAgent, compact: Boolean, onClick: () -> Unit) {
    Surface(
        onClick = onClick,
        shape = RoundedCornerShape(if (compact) 18.dp else 12.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f)),
    ) {
        Row(
            Modifier.padding(horizontal = if (compact) 10.dp else 12.dp, vertical = if (compact) 7.dp else 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            AgentLogo(tool.id, size = if (compact) 18.dp else 22.dp)
            Spacer(Modifier.width(if (compact) 8.dp else 10.dp))
            Text(
                if (tool.id == Chat.AGENT_LAPIS) "Lapis agent" else tool.name.ifBlank { tool.id },
                style = if (compact) MaterialTheme.typography.labelMedium else MaterialTheme.typography.labelLarge,
            )
        }
    }
}
