package dev.lapiszero.app.ui.chats

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import dev.lapiszero.app.ui.components.AgentLogo
import dev.lapiszero.app.data.remote.Chat
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.draw.clip
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TextField
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
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
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.lapiszero.app.data.remote.HostAgent
import dev.lapiszero.app.data.remote.Message
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.HeadState
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.domain.ScreenshotStore
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.ApprovalCard
import dev.lapiszero.app.ui.components.ChoiceBadge
import dev.lapiszero.app.ui.components.CollectMessages
import dev.lapiszero.app.ui.components.ErrorRetry
import dev.lapiszero.app.ui.components.LapisBadge
import dev.lapiszero.app.ui.components.ScreenshotDialog
import dev.lapiszero.app.ui.components.ScreenshotThumb
import dev.lapiszero.app.ui.components.formatClock
import dev.lapiszero.app.ui.components.kindLabel
import dev.lapiszero.app.ui.components.policyLabel
import dev.lapiszero.app.ui.head.LapisHead

private val POLICY_OPTIONS = listOf<String?>(null, "strict", "balanced", "autonomous")

private val HostAgent.label: String get() = name.ifBlank { id }

@Composable
fun ChatThreadScreen(
    session: LapisSession,
    chatId: String,
    snackbar: SnackbarHostState,
    approvals: ApprovalViewModel,
) {
    val vm: ChatThreadViewModel = viewModel(
        key = "thread-$chatId",
        factory = viewModelFactory { initializer { ChatThreadViewModel(session.repo, chatId) } },
    )
    val ui by vm.ui.collectAsStateWithLifecycle()
    val draft by vm.draft.collectAsStateWithLifecycle()
    val sending by vm.sending.collectAsStateWithLifecycle()
    val runWith by vm.runWith.collectAsStateWithLifecycle()
    val approvalUi by approvals.ui.collectAsStateWithLifecycle()
    val head by session.repo.head.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, snackbar)

    var viewer by rememberSaveable { mutableStateOf<Long?>(null) }
    var editingFolder by remember { mutableStateOf(false) }
    val thread = ui.thread
    val chat = thread.chat
    val listState = rememberLazyListState()
    val itemCount = thread.messages.size + thread.pending.size
    // Follow the conversation as it grows (new tool cards, the final answer).
    LaunchedEffect(itemCount, ui.running) {
        if (itemCount > 0) listState.animateScrollToItem(listState.layoutInfo.totalItemsCount.coerceAtLeast(1) - 1)
    }

    val agent = runWith.current
    val items = remember(thread.messages) { groupThread(thread.messages) }

    Column(Modifier.fillMaxSize().imePadding()) {
        LazyColumn(
            state = listState,
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            if (thread.hasMore) {
                item(key = "older") {
                    TextButton(onClick = vm::loadOlder, enabled = !thread.loading, modifier = Modifier.fillMaxWidth()) {
                        Text(if (thread.loading) "Loading…" else "Load earlier messages")
                    }
                }
            }
            thread.error?.let { error ->
                item(key = "error") { ErrorRetry(error, onRetry = vm::reload) }
            }
            if (thread.messages.isEmpty() && thread.pending.isEmpty() && thread.error == null) {
                item(key = "empty") { EmptySession(agent, chat?.hostId, thread.loading) }
            }
            items(items, key = { it.key }) { item ->
                when (item) {
                    is ThreadItem.Steps -> StepsBlock(item.steps, session.shots, live = ui.running && item === items.last(), onOpenShot = { viewer = it })
                    is ThreadItem.Single -> when (item.message.role) {
                        Message.ROLE_USER -> UserBubble(item.message.content, sourceLabel(item.message.metadata.source), pending = false)
                        Message.ROLE_SYSTEM -> SystemNote(item.message.content)
                        else -> AssistantMessage(item.message, agent)
                    }
                }
            }
            items(thread.pending, key = { it.localId }) { pending ->
                UserBubble(pending.content, "sending…", pending = true)
            }
            if (ui.running) {
                item(key = "running") {
                    RunningRow(head, ui.agentState, ui.step, ui.maxSteps, agent.label.takeIf { runWith.usesTool }, ui.progress)
                }
            }
        }

        // Sticky approval cards for this chat (the global sheet shows the same requests).
        if (ui.approvals.isNotEmpty()) {
            Column(
                Modifier
                    .fillMaxWidth()
                    .heightIn(max = 360.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 6.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                ui.approvals.forEach { approval ->
                    ApprovalCard(
                        approval = approval,
                        shots = session.shots,
                        busy = approval.requestId in approvalUi.busy,
                        onDecide = { decision, reason -> approvals.decide(approval.requestId, decision, reason) },
                    )
                }
            }
        }

        Composer(
            draft = draft,
            onDraft = vm::setDraft,
            onSend = vm::send,
            sending = sending,
            ui = ui,
            host = chat?.hostId.orEmpty(),
            policy = chat?.policyOverride,
            onHost = vm::setHost,
            onPolicy = vm::setPolicy,
            runWith = runWith,
            onAgent = vm::setAgent,
            onRefreshAgents = vm::refreshAgents,
            onFolder = { editingFolder = true },
            onStop = vm::stop,
            onKill = vm::killAll,
        )
    }

    viewer?.let { id -> ScreenshotDialog(session.shots, id, onDismiss = { viewer = null }) }
    if (editingFolder) {
        var folder by remember { mutableStateOf(runWith.workdir) }
        AlertDialog(
            onDismissRequest = { editingFolder = false },
            title = { Text("Working folder") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    OutlinedTextField(
                        value = folder,
                        onValueChange = { folder = it.take(ChatThreadViewModel.MAX_WORKDIR) },
                        label = { Text("Folder on ${chat?.hostId ?: "the host"}") },
                        placeholder = { Text("~") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                    )
                    Text(
                        "Where ${runWith.current.label} works. Leave it empty for the home folder.",
                        style = MaterialTheme.typography.bodySmall,
                        color = lapis.muted,
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        vm.setWorkdir(folder)
                        editingFolder = false
                    },
                ) { Text("Save") }
            },
            dismissButton = { OutlinedButton(onClick = { editingFolder = false }) { Text("Cancel") } },
        )
    }
}

private fun sourceLabel(source: String?): String? = when {
    source == null -> null
    source == "device" -> "typed on device"
    source == "voice" -> "voice on device"
    source == "schedule" -> "scheduled job"
    source.startsWith("android:") -> source.removePrefix("android:")
    else -> source
}

/** What a thread shows: one message, or a run of tool steps folded into one block. */
sealed interface ThreadItem {
    val key: Any

    data class Single(val message: Message) : ThreadItem {
        override val key: Any get() = message.id
    }

    data class Steps(val steps: List<Message>) : ThreadItem {
        override val key: Any get() = "steps-${steps.first().id}"
    }
}

/** Consecutive tool messages become one [ThreadItem.Steps]; everything else stays as it is. */
fun groupThread(messages: List<Message>): List<ThreadItem> {
    val out = mutableListOf<ThreadItem>()
    for (message in messages) {
        val last = out.lastOrNull()
        out += when {
            message.role != Message.ROLE_TOOL -> ThreadItem.Single(message)
            last is ThreadItem.Steps -> { out.removeAt(out.lastIndex); ThreadItem.Steps(last.steps + message) }
            else -> ThreadItem.Steps(listOf(message))
        }
    }
    return out
}

private val UserShape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp, bottomStart = 20.dp, bottomEnd = 6.dp)

@Composable
private fun UserBubble(text: String, caption: String?, pending: Boolean) {
    Row(Modifier.fillMaxWidth().padding(start = 48.dp), horizontalArrangement = Arrangement.End) {
        Column(horizontalAlignment = Alignment.End, modifier = Modifier.alpha(if (pending) 0.6f else 1f)) {
            Surface(
                color = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary,
                shape = UserShape,
            ) {
                SelectionContainer {
                    Text(text, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp))
                }
            }
            if (caption != null) {
                Text(caption, style = LapisType.MonoSmall, color = lapis.muted, modifier = Modifier.padding(top = 4.dp, end = 4.dp))
            }
        }
    }
}

/**
 * The answer: who gave it, then the text as rendered Markdown. No bubble: answers get the width.
 * ponytail: "who" is the chat's agent now; messages do not record which agent answered, so an
 * answer from before a switch shows the new one. Add the agent to the message if that matters.
 */
@Composable
private fun AssistantMessage(message: Message, agent: HostAgent) {
    val status = message.metadata.status
    Column(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgentLogo(agent.id, size = 18.dp)
            Spacer(Modifier.width(8.dp))
            Text(agent.label, style = MaterialTheme.typography.labelLarge, color = lapis.muted)
            if (status != null && status != "done") {
                Spacer(Modifier.width(8.dp))
                LapisBadge(status, color = if (status == "failed") lapis.danger else lapis.amber)
            }
        }
        Spacer(Modifier.height(6.dp))
        MarkdownText(message.content)
    }
}

@Composable
private fun SystemNote(text: String) {
    Text(
        text,
        style = LapisType.MonoSmall,
        color = lapis.muted,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp),
    )
}

/** A new session: who will answer, and where. */
@Composable
private fun EmptySession(agent: HostAgent, host: String?, loading: Boolean) {
    Column(
        Modifier.fillMaxWidth().padding(vertical = 32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (agent.id == Chat.AGENT_LAPIS) {
            LapisHead(HeadState.IDLE, size = 112.dp)
        } else {
            AgentLogo(agent.id, size = 56.dp)
            Spacer(Modifier.height(12.dp))
            Text(agent.label, style = MaterialTheme.typography.titleMedium)
        }
        Spacer(Modifier.height(8.dp))
        Text(
            when {
                loading -> "Loading conversation…"
                agent.id == Chat.AGENT_LAPIS -> "Describe what the agent should do on ${host ?: "the host"}."
                else -> "Ask ${agent.label} to work on ${host ?: "the host"}."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = lapis.muted,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * A run of tool steps. One step shows as it is; more fold under "N steps" with the latest in
 * view, and open while the task runs.
 */
@Composable
private fun StepsBlock(steps: List<Message>, shots: ScreenshotStore, live: Boolean, onOpenShot: (Long) -> Unit) {
    if (steps.size == 1) {
        ToolCallCard(steps.first(), shots, onOpenShot)
        return
    }
    var open by rememberSaveable(steps.first().id) { mutableStateOf(false) }
    val shown = open || live
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(
            Modifier
                .clip(RoundedCornerShape(8.dp))
                .clickable(onClickLabel = if (shown) "Fold steps" else "Show all steps") { open = !shown }
                .padding(vertical = 4.dp, horizontal = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("${steps.size} steps", style = LapisType.MonoLabel, color = lapis.muted)
            Spacer(Modifier.width(6.dp))
            Text(if (shown) "▴" else "▾", color = lapis.muted)
        }
        (if (shown) steps else steps.takeLast(1)).forEach { ToolCallCard(it, shots, onOpenShot) }
    }
}

/**
 * The agent's head acting out what it does, and the same in words beside it.
 * `tool`: the name of the agent tool running this chat, or null for the device's own agent.
 */
@Composable
private fun RunningRow(head: HeadState, agentState: String, step: Int, maxSteps: Int, tool: String?, progress: String?) {
    // The head's own word for it (FETCHING, RUNNING A COMMAND...). Between an approval's decision
    // and the next sync the head is idle while the task still runs: then the daemon's word.
    val doing = if (head == HeadState.IDLE) agentState.replace('_', ' ') else head.label.uppercase()
    Row(verticalAlignment = Alignment.CenterVertically) {
        LapisHead(head, size = 40.dp)
        Spacer(Modifier.width(4.dp))
        Text(
            if (tool == null) {
                "$doing · step $step/$maxSteps"
            } else {
                // A tool on the host has no step budget: its name and its latest progress line.
                listOfNotNull(doing, tool, progress).joinToString(" · ")
            },
            style = LapisType.MonoLabel,
            color = lapis.muted,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Collapsible tool call: `[SSH] git status`, status, detail and the target-screen frame. */
@Composable
private fun ToolCallCard(message: Message, shots: ScreenshotStore, onOpenShot: (Long) -> Unit) {
    val meta = message.metadata
    var expanded by rememberSaveable(message.id) { mutableStateOf(false) }
    val statusColor = when (meta.status) {
        "denied" -> lapis.amber
        "error" -> lapis.danger
        else -> lapis.muted
    }
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth().clickable(onClickLabel = "Toggle details") { expanded = !expanded },
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                LapisBadge(kindLabel(meta.kind), color = lapis.muted)
                Text(
                    message.content,
                    style = LapisType.Mono,
                    maxLines = if (expanded) Int.MAX_VALUE else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                if (meta.status != null && meta.status != "ok") LapisBadge(meta.status, color = statusColor)
                Text(if (expanded) "▴" else "▾", color = lapis.muted)
            }
            if (!expanded && !meta.detail.isNullOrBlank()) {
                Text(
                    "-> ${meta.detail}",
                    style = LapisType.MonoSmall,
                    color = lapis.muted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
            AnimatedVisibility(expanded) {
                Column {
                    Spacer(Modifier.height(8.dp))
                    if (!meta.detail.isNullOrBlank()) {
                        SelectionContainer {
                            Text(
                                meta.detail,
                                style = LapisType.MonoSmall,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .background(MaterialTheme.colorScheme.surfaceContainerHighest, RoundedCornerShape(8.dp))
                                    .padding(8.dp),
                            )
                        }
                    }
                    Text(
                        listOfNotNull(
                            meta.step?.let { "step $it" },
                            (meta.ts ?: message.createdAt).takeIf { it > 0 }?.let(::formatClock),
                            meta.kind,
                        ).joinToString("  ·  "),
                        style = LapisType.MonoSmall,
                        color = lapis.muted,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
            }
            meta.screenshotId?.let { id ->
                Spacer(Modifier.height(8.dp))
                ScreenshotThumb(
                    shots,
                    id,
                    modifier = Modifier.width(if (expanded) 280.dp else 132.dp).clip(RoundedCornerShape(6.dp)),
                    onClick = { onOpenShot(id) },
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Composer(
    draft: String,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
    sending: Boolean,
    ui: ChatThreadViewModel.Ui,
    host: String,
    policy: String?,
    onHost: (String) -> Unit,
    onPolicy: (String?) -> Unit,
    runWith: RunWith,
    onAgent: (String) -> Unit,
    onRefreshAgents: () -> Unit,
    onFolder: () -> Unit,
    onStop: () -> Unit,
    onKill: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 8.dp)) {
        // The session's settings: who answers, in which folder, where, how freely. Wraps as needed.
        FlowRow(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
            itemVerticalAlignment = Alignment.CenterVertically,
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgentLogo(runWith.current.id, size = 18.dp)
                Spacer(Modifier.width(6.dp))
                ChoiceBadge(
                    label = runWith.current.label,
                    // The entry after the tools (null) looks for them again.
                    options = if (runWith.refreshable) runWith.options + null else runWith.options,
                    optionLabel = { it?.label ?: "Look for tools again" },
                    onPick = { picked ->
                        when {
                            picked == null -> onRefreshAgents()
                            picked.id != runWith.current.id -> onAgent(picked.id)
                        }
                    },
                    color = if (runWith.usesTool) lapis.blue else MaterialTheme.colorScheme.onSurface,
                    enabled = runWith.enabled && !ui.running,
                )
            }
            if (runWith.usesTool) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    LapisBadge("Folder", color = lapis.muted, onClick = if (ui.running) null else onFolder)
                    Spacer(Modifier.width(6.dp))
                    // Not in the badge: badges are set in capitals and a path is case-sensitive.
                    Text(runWith.workdir.ifEmpty { "~" }, style = LapisType.Mono, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
            ChoiceBadge(
                label = host.ifEmpty { "host" },
                options = ui.hosts,
                optionLabel = { it },
                onPick = onHost,
                color = lapis.blue,
                enabled = !ui.running && ui.hosts.isNotEmpty(),
            )
            ChoiceBadge(
                label = policyLabel(policy),
                options = POLICY_OPTIONS,
                optionLabel = { policyLabel(it) },
                onPick = onPolicy,
                color = if (policy == "autonomous") lapis.amber else MaterialTheme.colorScheme.onSurface,
                enabled = !ui.running,
            )
            if (ui.busyElsewhere) LapisBadge("agent busy elsewhere", color = lapis.amber)
            if (ui.connection != ConnectionState.ONLINE) LapisBadge("offline", color = lapis.danger)
        }
        runWith.note?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = lapis.muted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
        Spacer(Modifier.height(8.dp))
        if (ui.running) {
            // Unmistakable while a task runs: STOP this task, or KILL everything.
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                Button(
                    onClick = onStop,
                    colors = ButtonDefaults.buttonColors(containerColor = lapis.amber, contentColor = lapis.ink),
                    shape = RoundedCornerShape(26.dp),
                    modifier = Modifier.weight(1f).height(52.dp),
                ) { Text("■  STOP TASK", style = LapisType.MonoLabel) }
                Button(
                    onClick = onKill,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError,
                    ),
                    shape = RoundedCornerShape(26.dp),
                    modifier = Modifier.height(52.dp),
                ) { Text("KILL ALL", style = LapisType.MonoLabel) }
            }
        } else {
            // One rounded field with the send button inside it.
            Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = RoundedCornerShape(26.dp)) {
                Row(verticalAlignment = Alignment.Bottom, modifier = Modifier.padding(end = 6.dp, bottom = 6.dp)) {
                    TextField(
                        value = draft,
                        onValueChange = onDraft,
                        placeholder = {
                            Text(if (runWith.usesTool) "Message ${runWith.current.label}…" else "Ask the agent…")
                        },
                        maxLines = 6,
                        keyboardOptions = KeyboardOptions(
                            capitalization = KeyboardCapitalization.Sentences,
                            imeAction = ImeAction.Send,
                        ),
                        keyboardActions = KeyboardActions(onSend = { onSend() }),
                        colors = TextFieldDefaults.colors(
                            focusedContainerColor = Color.Transparent,
                            unfocusedContainerColor = Color.Transparent,
                            disabledContainerColor = Color.Transparent,
                            focusedIndicatorColor = Color.Transparent,
                            unfocusedIndicatorColor = Color.Transparent,
                            disabledIndicatorColor = Color.Transparent,
                        ),
                        modifier = Modifier.weight(1f),
                    )
                    Box(contentAlignment = Alignment.Center) {
                        FilledIconButton(
                            onClick = onSend,
                            enabled = draft.isNotBlank() && !sending,
                            shape = CircleShape,
                            modifier = Modifier.size(44.dp),
                        ) { Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send", modifier = Modifier.size(20.dp)) }
                        if (sending) CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
                    }
                }
            }
        }
    }
}
