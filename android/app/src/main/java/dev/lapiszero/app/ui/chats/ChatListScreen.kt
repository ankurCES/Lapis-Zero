package dev.lapiszero.app.ui.chats

import androidx.compose.foundation.layout.Arrangement
import dev.lapiszero.app.ui.components.AgentLogo
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.data.remote.Message
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.ChoiceBadge
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.ConfirmDialog
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.LapisCard
import dev.lapiszero.app.ui.components.StatusDot
import dev.lapiszero.app.ui.components.formatAgo
import dev.lapiszero.app.ui.components.kindLabel
import dev.lapiszero.app.ui.head.LapisHead
import kotlinx.coroutines.launch

@Composable
fun ChatListScreen(session: LapisSession, snackbar: SnackbarHostState, onOpenChat: (String) -> Unit) {
    val vm: ChatListViewModel = viewModel(
        factory = viewModelFactory { initializer { ChatListViewModel(session.repo) } },
    )
    val ui by vm.ui.collectAsStateWithLifecycle()
    val head by session.repo.head.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    CollectMessages(vm.messages, snackbar)
    LaunchedEffect(vm) { vm.created.collect(onOpenChat) }

    var pickHost by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf<ChatRow?>(null) }
    var deleting by remember { mutableStateOf<ChatRow?>(null) }

    Box(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize()) {
            // The agent, acting out what it is doing right now, and the active host: where
            // device-side prompts (voice, keyboard) and new chats go.
            Row(
                // The head's figure has room for its props on both sides: 8 dp less here puts
                // the head itself, not that room, on the page margin.
                Modifier.fillMaxWidth().padding(start = 8.dp, end = 16.dp, top = 6.dp, bottom = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                LapisHead(head, size = 36.dp)
                Text("ACTIVE HOST", style = LapisType.MonoLabel, color = lapis.muted)
                Spacer(Modifier.width(8.dp))
                ChoiceBadge(
                    label = ui.activeHost.ifEmpty { "none" },
                    options = ui.hosts,
                    optionLabel = { it },
                    onPick = { host ->
                        scope.launch {
                            session.repo.setActiveHost(host)
                                .onFailure { snackbar.showSnackbar(it.message ?: "Could not switch host") }
                        }
                    },
                    color = lapis.blue,
                    enabled = ui.hosts.isNotEmpty(),
                )
                Spacer(Modifier.weight(1f))
                IconButton(onClick = vm::refresh) { Icon(Icons.Default.Refresh, contentDescription = "Refresh chats") }
            }
            LazyRow(
                contentPadding = PaddingValues(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                item {
                    FilterChip(
                        selected = ui.hostFilter == null,
                        onClick = { vm.setHostFilter(null) },
                        label = { Text("All") },
                    )
                }
                items(ui.hosts) { host ->
                    FilterChip(
                        selected = ui.hostFilter == host,
                        onClick = { vm.setHostFilter(host) },
                        label = { Text(host, style = LapisType.Mono) },
                    )
                }
            }
            LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 96.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                // Each host with the agents that can work on it: one tap opens a session in a tab.
                val shownHosts = ui.hostFilter?.let(::listOf) ?: ui.hosts
                if (shownHosts.isNotEmpty()) {
                    // Label items keep positional keys: hosts arriving after the list was drawn
                    // then appear in view instead of above it.
                    item { SectionLabel("Start a session") }
                    items(shownHosts, key = { "host-$it" }) { host ->
                        HostTools(
                            host = host,
                            active = host == ui.activeHost,
                            tools = ui.agents[host],
                            onPick = { agent -> vm.createChat(host, agent) },
                            compact = true,
                        )
                    }
                }
                item { SectionLabel("Recent", Modifier.padding(top = 8.dp)) }
                if (ui.rows.isEmpty()) {
                    item(key = "none") {
                        Column(Modifier.fillMaxWidth().padding(vertical = 24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                            Text("No chats yet", style = MaterialTheme.typography.titleMedium)
                            Spacer(Modifier.height(4.dp))
                            Text(
                                "Pick an agent above and it runs your prompt on that host.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = lapis.muted,
                            )
                        }
                    }
                }
                items(ui.rows, key = { it.chat.id }) { row ->
                    ChatRowCard(
                        row = row,
                        onOpen = { onOpenChat(row.chat.id) },
                        onRename = { renaming = row },
                        onArchive = { vm.archive(row.chat.id) },
                        onDelete = { deleting = row },
                    )
                }
            }
        }
        ExtendedFloatingActionButton(
            onClick = {
                if (ui.hosts.isEmpty()) {
                    scope.launch { snackbar.showSnackbar("Add a host under Config before starting a chat") }
                } else {
                    pickHost = true
                }
            },
            // Material clears the label's semantics on an extended FAB: the icon carries the name.
            icon = { Icon(Icons.Default.Add, contentDescription = "New session") },
            text = { Text("New session") },
            modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
        )
    }

    if (pickHost) {
        NewSessionSheet(
            ui = ui,
            onPick = { host, agent ->
                pickHost = false
                vm.createChat(host, agent)
            },
            onDismiss = { pickHost = false },
        )
    }
    renaming?.let { row ->
        var title by remember(row.chat.id) { mutableStateOf(row.chat.title) }
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename chat") },
            text = { OutlinedTextField(value = title, onValueChange = { title = it.take(80) }, singleLine = true) },
            confirmButton = {
                Button(
                    onClick = {
                        vm.rename(row.chat.id, title)
                        renaming = null
                    },
                    enabled = title.isNotBlank(),
                ) { Text("Save") }
            },
            dismissButton = { OutlinedButton(onClick = { renaming = null }) { Text("Cancel") } },
        )
    }
    deleting?.let { row ->
        ConfirmDialog(
            title = "Delete this chat?",
            text = "\"${row.chat.title}\" and all of its messages are removed from the device." +
                if (row.active) " The task running in it is stopped." else "",
            confirmLabel = "Delete",
            danger = true,
            onConfirm = { vm.delete(row.chat.id) },
            onDismiss = { deleting = null },
        )
    }
}

@Composable
private fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(text.uppercase(), style = LapisType.MonoLabel, color = lapis.muted, modifier = modifier)
}

@Composable
private fun ChatRowCard(
    row: ChatRow,
    onOpen: () -> Unit,
    onRename: () -> Unit,
    onArchive: () -> Unit,
    onDelete: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    val chat = row.chat
    LapisCard(
        modifier = Modifier.fillMaxWidth(),
        borderColor = when {
            row.pendingApprovals > 0 -> lapis.amber
            row.active -> lapis.blue
            else -> MaterialTheme.colorScheme.outlineVariant
        },
        onClick = onOpen,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgentLogo(chat.agent, size = 22.dp)
            Spacer(Modifier.width(10.dp))
            if (row.active) {
                StatusDot(lapis.blue, pulsing = true)
                Spacer(Modifier.width(8.dp))
            }
            Text(
                chat.title.ifBlank { "Untitled chat" },
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text(formatAgo(chat.updatedAt), style = LapisType.MonoSmall, color = lapis.muted)
            Box {
                IconButton(onClick = { menu = true }) {
                    Icon(Icons.Default.MoreVert, contentDescription = "Chat actions")
                }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; onRename() })
                    DropdownMenuItem(text = { Text("Archive") }, onClick = { menu = false; onArchive() })
                    DropdownMenuItem(text = { Text("Delete") }, onClick = { menu = false; onDelete() })
                }
            }
        }
        chat.lastMessage?.let { last ->
            Text(
                preview(last),
                style = if (last.role == Message.ROLE_TOOL) LapisType.Mono else MaterialTheme.typography.bodyMedium,
                color = lapis.muted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.height(6.dp))
        }
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            LapisBadge(chat.hostId, color = lapis.blue)
            // Only for a chat run by a tool on the host; the device's own agent needs no tag.
            row.agentName?.let { LapisBadge(it) }
            chat.policyOverride?.let { LapisBadge(it, color = lapis.muted) }
            if (row.active) {
                LapisBadge("running · step ${row.step ?: 0}/${row.maxSteps ?: 0}", color = lapis.blue, filled = true)
            }
            if (row.pendingApprovals > 0) {
                LapisBadge(
                    "${row.pendingApprovals} approval" + if (row.pendingApprovals > 1) "s" else "",
                    color = lapis.amber,
                    filled = true,
                )
            }
        }
    }
}

private fun preview(message: Message): String = when (message.role) {
    Message.ROLE_TOOL -> "[${kindLabel(message.metadata.kind)}] ${message.content}"
    Message.ROLE_USER -> "You: ${message.content}"
    else -> markdownPlain(message.content)
}
