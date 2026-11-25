package dev.lapiszero.app.ui.chats

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.remote.Activity
import dev.lapiszero.app.data.remote.AgentState
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.data.remote.Chat
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.HostAgent
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.DecideResult
import dev.lapiszero.app.domain.HostAgentsState
import dev.lapiszero.app.domain.SendResult
import dev.lapiszero.app.domain.SshSupport
import dev.lapiszero.app.domain.ThreadState
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** A chat list entry with its live fields recomputed from the canonical device state. */
data class ChatRow(
    val chat: Chat,
    val active: Boolean,
    val step: Int?,
    val maxSteps: Int?,
    val pendingApprovals: Int,
    /** The agent tool this chat runs with; null for a chat of the device's own agent. */
    val agentName: String? = null,
)

/**
 * `chat_updated` events do not carry `active` / `step` / `pending_approvals`, so the list never
 * trusts the stored copies: they are derived from `state_sync` and the pending approvals.
 */
fun buildChatRows(
    chats: List<Chat>,
    state: DeviceState?,
    approvals: List<Approval>,
    hostFilter: String?,
    agents: Map<String, HostAgentsState> = emptyMap(),
): List<ChatRow> = chats
    .filter { !it.archived && (hostFilter == null || it.hostId == hostFilter) }
    .map { chat ->
        val active = state != null && state.isRunning && state.activeChatId == chat.id
        ChatRow(
            chat = chat,
            active = active,
            step = if (active) state.activeTask?.step else null,
            maxSteps = if (active) state.activeTask?.maxSteps else null,
            pendingApprovals = approvals.count { it.chatId == chat.id },
            agentName = chat.agent.takeIf { it != Chat.AGENT_LAPIS }
                ?.let { ChatRepository.agentName(it, agents[chat.hostId]?.agents.orEmpty()) },
        )
    }

/** What the "Run with" menu of a thread shows: the device's own agent or a tool on the host. */
data class RunWith(
    val current: HostAgent = LAPIS,
    /** What can be picked. Only the current choice while the host's list is being fetched. */
    val options: List<HostAgent> = listOf(LAPIS),
    /** One muted line under the menu: why only the Lapis agent is offered. */
    val note: String? = null,
    /** False on device software without the SSH features: the menu does not open. */
    val enabled: Boolean = true,
    /** The host's list has been asked for and can be asked for again. */
    val refreshable: Boolean = false,
    /** Folder on the host a tool runs in; empty means the home folder. */
    val workdir: String = "",
) {
    val usesTool: Boolean get() = current.id != Chat.AGENT_LAPIS

    companion object {
        val LAPIS = HostAgent(Chat.AGENT_LAPIS, "Lapis agent")
    }
}

fun buildRunWith(chat: Chat?, tools: HostAgentsState?, support: SshSupport): RunWith {
    val found = tools?.agents.orEmpty()
    val agent = chat?.agent ?: Chat.AGENT_LAPIS
    // A chat bound to a tool that is no longer found keeps showing it, by its id: the name
    // went with the list.
    val current = if (agent == Chat.AGENT_LAPIS) RunWith.LAPIS else found.firstOrNull { it.id == agent } ?: HostAgent(agent, agent)
    val workdir = chat?.workdir.orEmpty()
    return when {
        support == SshSupport.Missing ->
            RunWith(current, listOf(current), ChatRepository.NO_SSH_FEATURES, enabled = false, workdir = workdir)

        tools == null || tools.loading -> RunWith(current, listOf(current), workdir = workdir)
        // With SSH down `found` is empty and `reason` says why: only the Lapis agent is offered.
        else -> RunWith(
            current,
            (listOf(RunWith.LAPIS) + found + current).distinctBy { it.id },
            tools.reason,
            refreshable = true,
            workdir = workdir,
        )
    }
}

class ChatListViewModel(private val repo: ChatRepository) : ViewModel() {
    data class Ui(
        val rows: List<ChatRow> = emptyList(),
        val hosts: List<String> = emptyList(),
        val activeHost: String = "",
        val hostFilter: String? = null,
        val connection: ConnectionState = ConnectionState.CONNECTING,
    )

    private val hostFilter = MutableStateFlow<String?>(null)
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()
    private val _created = MutableSharedFlow<String>(extraBufferCapacity = 2)

    /** Ids of chats created from this screen, so the UI can open them. */
    val created: SharedFlow<String> = _created.asSharedFlow()

    val ui: StateFlow<Ui> = combine(
        repo.chats, repo.state, repo.approvals, hostFilter, combine(repo.connection, repo.hostAgents, ::Pair),
    ) { chats, state, approvals, filter, (connection, agents) ->
        val hosts = state?.hosts ?: chats.map { it.hostId }.distinct()
        // A filter for a host that no longer exists would hide everything.
        val effective = filter?.takeIf { it in hosts }
        Ui(
            rows = buildChatRows(chats, state, approvals, effective, agents),
            hosts = hosts,
            activeHost = state?.activeHost.orEmpty(),
            hostFilter = effective,
            connection = connection,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), Ui())

    fun setHostFilter(host: String?) {
        hostFilter.value = host
    }

    fun refresh() {
        viewModelScope.launch { if (!repo.refreshChats()) _messages.emit("Could not refresh chats") }
    }

    fun createChat(hostId: String) {
        viewModelScope.launch {
            repo.createChat(hostId)
                .onSuccess { _created.emit(it.id) }
                .onFailure { _messages.emit(it.message ?: "Could not create the chat") }
        }
    }

    fun rename(chatId: String, title: String) = run("Could not rename the chat") { repo.renameChat(chatId, title) }

    fun archive(chatId: String) = run("Could not archive the chat") { repo.archiveChat(chatId) }

    fun delete(chatId: String) = run("Could not delete the chat") { repo.deleteChat(chatId) }

    private fun run(fallback: String, block: suspend () -> Result<*>) {
        viewModelScope.launch { block().onFailure { _messages.emit(it.message ?: fallback) } }
    }
}

class ChatThreadViewModel(
    private val repo: ChatRepository,
    val chatId: String,
) : ViewModel() {
    data class Ui(
        val thread: ThreadState = ThreadState(),
        /** The agent is working on this chat right now. */
        val running: Boolean = false,
        /** The agent is busy with a different chat: sending here would answer `409 busy`. */
        val busyElsewhere: Boolean = false,
        val agentState: String = AgentState.IDLE,
        val step: Int = 0,
        val maxSteps: Int = 0,
        /** The latest progress line of a tool working on this chat (activity `agent_run`). */
        val progress: String? = null,
        val hosts: List<String> = emptyList(),
        val approvals: List<Approval> = emptyList(),
        val connection: ConnectionState = ConnectionState.CONNECTING,
    )

    private val _draft = MutableStateFlow("")
    val draft: StateFlow<String> = _draft.asStateFlow()

    private val _sending = MutableStateFlow(false)
    val sending: StateFlow<Boolean> = _sending.asStateFlow()

    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    val ui: StateFlow<Ui> = combine(
        repo.thread(chatId), repo.state, repo.approvals, repo.connection, repo.activity,
    ) { thread, state, approvals, connection, activity ->
        val running = state != null && state.isRunning && state.activeChatId == chatId
        val task = state?.activeTask?.id.orEmpty()
        Ui(
            thread = thread,
            running = running,
            busyElsewhere = state != null && state.isRunning && !running,
            agentState = state?.agentState ?: AgentState.IDLE,
            step = if (running) state.activeTask?.step ?: 0 else 0,
            maxSteps = if (running) state.activeTask?.maxSteps ?: 0 else 0,
            // Of the task running now, where both sides name it: not the last line of an earlier run.
            progress = if (!running) null else activity.lastOrNull {
                it.chatId == chatId && it.kind == Activity.KIND_AGENT_RUN &&
                    (task.isEmpty() || it.taskId.isEmpty() || it.taskId == task)
            }?.summary?.takeIf { it.isNotBlank() },
            hosts = state?.hosts.orEmpty(),
            approvals = approvals.filter { it.chatId == chatId },
            connection = connection,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), Ui())

    val runWith: StateFlow<RunWith> = combine(repo.thread(chatId), repo.hostAgents, repo.sshSupport) { thread, agents, support ->
        buildRunWith(thread.chat, agents[thread.chat?.hostId], support)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), RunWith())

    init {
        viewModelScope.launch { repo.loadThread(chatId) }
        // The tools of the chat's host, once it is known which host that is and after a switch.
        viewModelScope.launch {
            repo.thread(chatId).mapNotNull { it.chat?.hostId }.distinctUntilChanged().collect { repo.loadHostAgents(it) }
        }
    }

    fun setDraft(text: String) {
        _draft.value = text
    }

    fun reload() {
        viewModelScope.launch { repo.loadThread(chatId) }
    }

    fun loadOlder() {
        viewModelScope.launch { repo.loadOlder(chatId) }
    }

    /** Optimistic send; on `409 busy` the text returns to the composer. */
    fun send() {
        val text = _draft.value.trim()
        if (text.isEmpty() || _sending.value) return
        _sending.value = true
        _draft.value = ""
        viewModelScope.launch {
            when (val result = repo.sendMessage(chatId, text)) {
                SendResult.Sent -> Unit
                SendResult.Busy -> {
                    restore(text)
                    _messages.emit("The agent is busy with another task. Stop it or wait for it to finish.")
                }

                is SendResult.Failed -> {
                    restore(text)
                    _messages.emit(result.message)
                }
            }
            _sending.value = false
        }
    }

    private fun restore(text: String) = _draft.update { current -> current.ifEmpty { text } }

    /** Stops the task running in this chat. */
    fun stop() {
        viewModelScope.launch {
            repo.stopChat(chatId).onFailure { _messages.emit(it.message ?: "Could not stop the task") }
        }
    }

    /** Global kill switch (`POST /v1/agent/kill-all`). */
    fun killAll() {
        viewModelScope.launch {
            if (repo.killAll()) _messages.emit("Kill switch sent")
        }
    }

    fun setPolicy(policy: String?) {
        viewModelScope.launch {
            repo.setChatPolicy(chatId, policy)
                .onFailure { _messages.emit(it.message ?: "Could not change the policy") }
        }
    }

    fun setHost(hostId: String) {
        viewModelScope.launch {
            repo.setChatHost(chatId, hostId)
                .onFailure { _messages.emit(it.message ?: "Could not change the host") }
        }
    }

    /** [Chat.AGENT_LAPIS] or the id of a tool found on the chat's host. */
    fun setAgent(agent: String) {
        viewModelScope.launch {
            repo.setChatAgent(chatId, agent)
                .onFailure { _messages.emit(it.message ?: "Could not change what runs this chat") }
        }
    }

    /** Plain text: trimmed and capped in length, nothing else. Empty means the home folder. */
    fun setWorkdir(workdir: String) {
        viewModelScope.launch {
            repo.setChatWorkdir(chatId, workdir.trim().take(MAX_WORKDIR))
                .onFailure { _messages.emit(it.message ?: "Could not change the folder") }
        }
    }

    /** Looks for agent tools on the chat's host again. */
    fun refreshAgents() {
        repo.thread(chatId).value.chat?.hostId?.let { repo.loadHostAgents(it, refresh = true) }
    }

    companion object {
        // The daemon's own limit for a chat's folder; it decides whether the folder exists.
        const val MAX_WORKDIR = 500
    }
}

/** Pending approvals for the active device and the decisions taken from this phone. */
class ApprovalViewModel(private val repo: ChatRepository) : ViewModel() {
    data class Ui(
        val approvals: List<Approval> = emptyList(),
        /** Request ids with a decision in flight. */
        val busy: Set<String> = emptySet(),
    )

    private val busy = MutableStateFlow<Set<String>>(emptySet())
    private val decidedHere = HashSet<String>()
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    val ui: StateFlow<Ui> = combine(repo.approvals, busy) { approvals, inFlight ->
        Ui(approvals, inFlight)
    }.stateIn(viewModelScope, SharingStarted.Eagerly, Ui())

    init {
        viewModelScope.launch {
            repo.resolutions.collect { resolved ->
                val mine = synchronized(decidedHere) { decidedHere.remove(resolved.requestId) }
                if (!mine && resolved.by.isNotEmpty()) {
                    _messages.emit("${describeDecision(resolved.decision)} ${describeBy(resolved.by)}")
                }
            }
        }
    }

    fun decide(requestId: String, decision: String, reason: String? = null) {
        if (requestId in busy.value) return
        busy.update { it + requestId }
        synchronized(decidedHere) { decidedHere.add(requestId) }
        viewModelScope.launch {
            when (val result = repo.decide(requestId, decision, reason)) {
                DecideResult.Done -> Unit
                DecideResult.AlreadyResolved -> _messages.emit("That request was already resolved")
                is DecideResult.Failed -> {
                    synchronized(decidedHere) { decidedHere.remove(requestId) }
                    _messages.emit("Decision not sent: ${result.message}")
                }
            }
            busy.update { it - requestId }
        }
    }

    companion object {
        fun describeDecision(decision: String): String = when (decision) {
            "approve_once" -> "Approved once"
            "approve_session" -> "Approved for the session"
            "deny" -> "Denied"
            else -> "Resolved"
        }

        fun describeBy(by: String): String = when {
            by == "device" -> "on the device"
            by == "kill_switch" -> "by the kill switch"
            by.startsWith("android:") -> "from ${by.removePrefix("android:")}"
            else -> "by $by"
        }
    }
}
