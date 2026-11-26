package dev.lapiszero.app.domain

import dev.lapiszero.app.data.local.ChatCache
import dev.lapiszero.app.data.remote.Activity
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.data.remote.ApprovalResolved
import dev.lapiszero.app.data.remote.Chat
import dev.lapiszero.app.data.remote.ClientCommand
import dev.lapiszero.app.data.remote.DeviceSshKey
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.EventSource
import dev.lapiszero.app.data.remote.HostAgent
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.LapisEvent
import dev.lapiszero.app.data.remote.Message
import dev.lapiszero.app.data.remote.NudgeRequest
import dev.lapiszero.app.data.remote.Pairing
import dev.lapiszero.app.data.remote.TaskFinished
import dev.lapiszero.app.data.remote.Telemetry
import dev.lapiszero.app.data.remote.WsFrame
import dev.lapiszero.app.data.remote.WsStatus
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.flow.updateAndGet
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

enum class ConnectionState { CONNECTING, ONLINE, OFFLINE, UNAUTHORIZED }

/** A user prompt shown immediately, before the daemon has confirmed it. */
data class PendingMessage(val localId: Long, val content: String)

data class ThreadState(
    val chat: Chat? = null,
    /** Server-confirmed messages, oldest first. */
    val messages: List<Message> = emptyList(),
    val pending: List<PendingMessage> = emptyList(),
    val hasMore: Boolean = false,
    val loading: Boolean = false,
    val loaded: Boolean = false,
    val error: String? = null,
)

sealed interface SendResult {
    data object Sent : SendResult

    /** `409 busy`: another task is running on the device. */
    data object Busy : SendResult
    data class Failed(val message: String) : SendResult
}

sealed interface DecideResult {
    data object Done : DecideResult

    /** `404`: somebody (the device, another phone, the kill switch) got there first. */
    data object AlreadyResolved : DecideResult
    data class Failed(val message: String) : DecideResult
}

sealed interface Notice {
    data class Toast(val level: String, val text: String) : Notice
    data class TaskDone(val finished: TaskFinished) : Notice
}

/** What this session knows about the device's SSH features (`GET /v1/device/ssh-key`). */
sealed interface SshSupport {
    /** Not asked yet, or the device could not be asked (`error` says why). */
    data class Unknown(val error: String? = null) : SshSupport

    /** The device software is from before the SSH features. Nothing new is sent to it. */
    data object Missing : SshSupport

    data class Ready(val key: DeviceSshKey) : SshSupport
}

/** The agent tools of one host, as far as this session knows. */
data class HostAgentsState(
    val agents: List<HostAgent> = emptyList(),
    val loading: Boolean = false,
    /** Why there is no list (SSH does not work for the host, say), in the device's words. */
    val reason: String? = null,
)

/**
 * Canonical live state of one paired device, fed by the WebSocket and REST.
 *
 * The daemon is the single owner of state: every connection starts with a full `state_sync`
 * and this class rebuilds itself from it, so a decision taken on the CardputerZero (or another
 * phone) shows up here exactly like one taken locally.
 */
class ChatRepository(
    private val api: LapisApiClient,
    private val source: EventSource,
    private val scope: CoroutineScope,
    private val cache: ChatCache = ChatCache.None,
    private val deviceKey: String = "",
) {
    private val _connection = MutableStateFlow(ConnectionState.CONNECTING)
    val connection: StateFlow<ConnectionState> = _connection.asStateFlow()

    private val _state = MutableStateFlow<DeviceState?>(null)
    val state: StateFlow<DeviceState?> = _state.asStateFlow()

    private val _chats = MutableStateFlow<List<Chat>>(emptyList())
    val chats: StateFlow<List<Chat>> = _chats.asStateFlow()

    private val _approvals = MutableStateFlow<List<Approval>>(emptyList())
    val approvals: StateFlow<List<Approval>> = _approvals.asStateFlow()

    private val _activity = MutableStateFlow<List<Activity>>(emptyList())

    /** Recent tool calls, oldest first, capped at [MAX_ACTIVITY]. */
    val activity: StateFlow<List<Activity>> = _activity.asStateFlow()

    private val _telemetry = MutableStateFlow<Telemetry?>(null)
    val telemetry: StateFlow<Telemetry?> = _telemetry.asStateFlow()

    private val _pairing = MutableStateFlow<Pairing?>(null)
    val pairing: StateFlow<Pairing?> = _pairing.asStateFlow()

    private val _notices = MutableSharedFlow<Notice>(extraBufferCapacity = 16)
    val notices: SharedFlow<Notice> = _notices.asSharedFlow()

    private val _resolutions = MutableSharedFlow<ApprovalResolved>(extraBufferCapacity = 16)

    /** Every approval that stopped being pending, whoever resolved it. */
    val resolutions: SharedFlow<ApprovalResolved> = _resolutions.asSharedFlow()

    private val _sshSupport = MutableStateFlow<SshSupport>(SshSupport.Unknown())
    val sshSupport: StateFlow<SshSupport> = _sshSupport.asStateFlow()

    private val headTrack = MutableStateFlow(HeadTrack())
    private val _head = MutableStateFlow(HeadState.IDLE)

    /**
     * What the agent's head acts out right now. While the connection is down it keeps the last
     * known state, like everything else here; a new connection starts it over from its snapshot.
     */
    val head: StateFlow<HeadState> = _head.asStateFlow()

    private val _hostAgents = MutableStateFlow<Map<String, HostAgentsState>>(emptyMap())

    /** Agent tools by host id: asked when a thread on that host opens, kept for the session. */
    val hostAgents: StateFlow<Map<String, HostAgentsState>> = _hostAgents.asStateFlow()

    private val threads = ConcurrentHashMap<String, MutableStateFlow<ThreadState>>()
    private val localIds = AtomicLong(-1)
    private var syncedEpoch = -1
    @Volatile
    private var telemetryWanted = false
    private var started = false

    fun thread(chatId: String): StateFlow<ThreadState> = threadFlow(chatId).asStateFlow()

    private fun threadFlow(chatId: String): MutableStateFlow<ThreadState> =
        threads.getOrPut(chatId) { MutableStateFlow(ThreadState()) }

    /** Subscribes to the event stream, then opens the socket. Idempotent. */
    @Synchronized
    fun start() {
        if (started) return
        started = true
        // UNDISPATCHED: both subscriptions exist before the socket can deliver its first frame.
        scope.launch(start = CoroutineStart.UNDISPATCHED) { source.frames.collect { onFrame(it) } }
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            source.status.collect { status ->
                _connection.value = when (status) {
                    is WsStatus.Connected -> ConnectionState.ONLINE
                    is WsStatus.Waiting -> ConnectionState.OFFLINE
                    WsStatus.Unauthorized -> ConnectionState.UNAUTHORIZED
                    is WsStatus.Connecting ->
                        if (status.attempt == 0) ConnectionState.CONNECTING else ConnectionState.OFFLINE

                    WsStatus.Idle -> ConnectionState.CONNECTING
                }
            }
        }
        // A step's hold and a beat run out by the clock, with no frame to say so.
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            headTrack.collectLatest { track ->
                while (true) {
                    val now = nowMs()
                    _head.value = track.shown(now)
                    delay(track.nextChange(now) ?: break)
                }
            }
        }
        scope.launch {
            val cached = cache.chats(deviceKey)
            // Only fill an empty list: fresh data from the daemon always wins.
            if (cached.isNotEmpty()) _chats.update { it.ifEmpty { cached } }
        }
        source.start()
    }

    fun stop() = source.stop()

    fun reconnectNow() = source.reconnectNow()

    // ------------------------------------------------------------------ events

    private fun nowMs(): Long = System.nanoTime() / 1_000_000

    private suspend fun onFrame(frame: WsFrame) {
        val event = frame.event
        // The first sync of a connection starts the head over: what it held belongs to the old one.
        val fresh = event is LapisEvent.StateSync && frame.epoch != syncedEpoch
        headTrack.update { (if (fresh) HeadTrack() else it).on(event, nowMs()) }
        when (event) {
            is LapisEvent.StateSync -> {
                val resync = frame.epoch != syncedEpoch
                applyState(event.state)
                if (resync) {
                    syncedEpoch = frame.epoch
                    onResync()
                }
            }

            is LapisEvent.ApprovalRequest -> _approvals.update { list ->
                if (list.any { it.requestId == event.approval.requestId }) list else list + event.approval
            }

            is LapisEvent.ApprovalResolvedEvent -> {
                removeApproval(event.resolved.requestId)
                _resolutions.emit(event.resolved)
            }

            is LapisEvent.ChatMessageCreated -> onMessage(event.chatId, event.message)
            is LapisEvent.ChatUpdated -> upsertChat(event.chat, fromEvent = true)
            is LapisEvent.ChatDeleted -> {
                _chats.update { list -> list.filterNot { it.id == event.chatId } }
                threads.remove(event.chatId)
                cache.removeChat(deviceKey, event.chatId)
            }

            is LapisEvent.ActivityEvent -> appendActivity(event.activity)
            is LapisEvent.TaskFinishedEvent -> {
                _notices.emit(Notice.TaskDone(event.finished))
                event.finished.chatId?.let { id -> scope.launch { refreshChat(id) } }
            }

            is LapisEvent.TelemetryTick -> _telemetry.value = event.telemetry
            is LapisEvent.PairingEvent ->
                _pairing.value = event.pairing.takeIf { it.isActive }

            is LapisEvent.Toast -> _notices.emit(Notice.Toast(event.level, event.text))
            is LapisEvent.Pong, is LapisEvent.Unknown -> Unit
            // Lapis-Deck frames belong to DeckRepository.
            is LapisEvent.MeshMessageEvent, is LapisEvent.SdrDecodeEvent, is LapisEvent.SdrSpectrumEvent -> Unit
        }
    }

    private suspend fun applyState(state: DeviceState) {
        val before = _approvals.value
        _state.value = state
        _pairing.value = state.pairing?.takeIf { it.isActive }
        // state_sync carries the authoritative pending list: anything missing was resolved.
        _approvals.value = state.pendingApprovals
        val still = state.pendingApprovals.mapTo(HashSet()) { it.requestId }
        before.filterNot { it.requestId in still }.forEach {
            _resolutions.emit(ApprovalResolved(it.requestId, decision = "", by = ""))
        }
    }

    /** First `state_sync` of a new connection: everything else is rebuilt over REST. */
    private fun onResync() {
        if (telemetryWanted) source.send(ClientCommand.SubscribeTelemetry(true))
        scope.launch {
            refreshChats()
            // Reload every thread that is open so no message from the gap is missing.
            threads.keys.toList().forEach { id ->
                if (threads[id]?.value?.loaded == true) loadThread(id)
            }
            val seed = _state.value?.activeChatId ?: _chats.value.firstOrNull()?.id
            if (seed != null && _activity.value.isEmpty()) seedActivity(seed)
        }
    }

    private suspend fun onMessage(chatId: String, message: Message) {
        threads[chatId]?.update { t ->
            val pending = if (message.role == Message.ROLE_USER) {
                // The daemon echoes our own prompt: retire the matching optimistic bubble.
                val match = t.pending.firstOrNull { it.content == message.content }
                if (match != null) t.pending - match else t.pending
            } else {
                t.pending
            }
            t.copy(messages = t.messages.upsert(message), pending = pending)
        }
        var known = false
        _chats.update { list ->
            list.map {
                if (it.id == chatId) {
                    known = true
                    it.copy(lastMessage = message, updatedAt = maxOf(it.updatedAt, message.createdAt))
                } else {
                    it
                }
            }.sortedByDescending { it.updatedAt }
        }
        if (!known) scope.launch { refreshChats() }
    }

    private fun upsertChat(chat: Chat, fromEvent: Boolean) {
        _chats.update { list ->
            val old = list.firstOrNull { it.id == chat.id }
            // `chat_updated` carries neither last_message nor the live fields: keep ours.
            val merged = if (fromEvent && old != null) {
                chat.copy(
                    lastMessage = chat.lastMessage ?: old.lastMessage,
                    active = old.active,
                    step = old.step,
                    pendingApprovals = old.pendingApprovals,
                )
            } else {
                chat
            }
            (list.filterNot { it.id == chat.id } + merged).sortedByDescending { it.updatedAt }
        }
        threads[chat.id]?.update { it.copy(chat = _chats.value.firstOrNull { c -> c.id == chat.id }) }
    }

    private fun appendActivity(activity: Activity) {
        _activity.update { (it + activity).takeLast(MAX_ACTIVITY) }
    }

    private fun removeApproval(requestId: String) {
        _approvals.update { list -> list.filterNot { it.requestId == requestId } }
    }

    // ------------------------------------------------------------------ chats

    suspend fun refreshChats(): Boolean = try {
        val fresh = api.chats()
        _chats.value = fresh.sortedByDescending { it.updatedAt }
        cache.saveChats(deviceKey, fresh)
        true
    } catch (e: IOException) {
        false
    }

    private suspend fun refreshChat(chatId: String) {
        try {
            val detail = api.chat(chatId, limit = 1)
            upsertChat(detail.chat, fromEvent = false)
        } catch (e: IOException) {
            // The list refreshes on the next resync anyway.
        }
    }

    /**
     * `agent`: a tool found on the host (`claude`, ...) for a session run by it; only sent to
     * device software that has tools, and null (the device's own agent) is never sent.
     */
    suspend fun createChat(hostId: String, title: String? = null, policy: String? = null, agent: String? = null): Result<Chat> =
        call {
            val tool = agent?.takeIf { it != Chat.AGENT_LAPIS && checkSshSupport() is SshSupport.Ready }
            api.createChat(hostId, title?.takeIf { it.isNotBlank() }, policy, tool).also {
                upsertChat(it, fromEvent = false)
                openTab(it.id)
            }
        }

    private val _openTabs = MutableStateFlow<List<String>>(emptyList())

    /**
     * Chats open as tabs in the sessions screen, in the order they were opened.
     * ponytail: kept for the app's lifetime, not across restarts; persist it if that is missed.
     */
    val openTabs: StateFlow<List<String>> = _openTabs.asStateFlow()

    fun openTab(chatId: String) = _openTabs.update { if (chatId in it) it else it + chatId }

    fun closeTab(chatId: String) = _openTabs.update { it - chatId }

    suspend fun renameChat(chatId: String, title: String): Result<Chat> =
        patchChat(chatId) { put("title", JsonPrimitive(title.trim())) }

    suspend fun setChatHost(chatId: String, hostId: String): Result<Chat> =
        patchChat(chatId) { put("host_id", JsonPrimitive(hostId)) }

    /** `policy == null` clears the override, so the host's own policy applies again. */
    suspend fun setChatPolicy(chatId: String, policy: String?): Result<Chat> =
        patchChat(chatId) { put("policy_override", policy?.let(::JsonPrimitive) ?: JsonNull) }

    suspend fun archiveChat(chatId: String, archived: Boolean = true): Result<Chat> =
        patchChat(chatId) { put("archived", JsonPrimitive(archived)) }.onSuccess { chat ->
            if (chat.archived) {
                _chats.update { list -> list.filterNot { it.id == chatId } }
                closeTab(chatId)
            }
        }

    private suspend fun patchChat(
        chatId: String,
        build: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit,
    ): Result<Chat> = call {
        api.patchChat(chatId, buildJsonObject(build)).also { upsertChat(it, fromEvent = false) }
    }

    /** Who answers in the chat: [Chat.AGENT_LAPIS] or the id of an agent tool on its host. */
    suspend fun setChatAgent(chatId: String, agent: String): Result<Chat> =
        patchRunWith { api.setChatAgent(chatId, agent) }

    /** The folder on the host a tool runs in; empty means the home folder. */
    suspend fun setChatWorkdir(chatId: String, workdir: String): Result<Chat> =
        patchRunWith { api.setChatWorkdir(chatId, workdir) }

    /** `agent` and `workdir` only go to device software that is known to have them. */
    private suspend fun patchRunWith(patch: suspend () -> Chat): Result<Chat> = call {
        when (val support = checkSshSupport()) {
            is SshSupport.Ready -> patch().also { upsertChat(it, fromEvent = false) }
            SshSupport.Missing -> throw IOException(NO_SSH_FEATURES)
            is SshSupport.Unknown -> throw IOException(support.error ?: "Could not reach the device")
        }
    }

    suspend fun deleteChat(chatId: String): Result<Unit> = call {
        api.deleteChat(chatId)
        _chats.update { list -> list.filterNot { it.id == chatId } }
        closeTab(chatId)
        threads.remove(chatId)
        cache.removeChat(deviceKey, chatId)
    }

    /** Loads the newest page of a chat (cached messages are shown while the request runs). */
    suspend fun loadThread(chatId: String) {
        val flow = threadFlow(chatId)
        if (!flow.value.loaded && flow.value.messages.isEmpty()) {
            val cached = cache.messages(deviceKey, chatId)
            if (cached.isNotEmpty()) flow.update { if (it.messages.isEmpty()) it.copy(messages = cached) else it }
        }
        flow.update { it.copy(loading = true, error = null) }
        try {
            val detail = api.chat(chatId, limit = PAGE)
            val newestFetched = detail.messages.lastOrNull()?.id ?: -1
            flow.update { t ->
                // The page replaces what was shown, except for anything newer that the socket
                // delivered while the request was in flight.
                val arrivedMeanwhile = t.messages.filter { it.id > newestFetched }
                t.copy(
                    chat = detail.chat,
                    messages = arrivedMeanwhile.fold(detail.messages) { acc, m -> acc.upsert(m) },
                    hasMore = detail.hasMore,
                    loading = false,
                    loaded = true,
                )
            }
            upsertChat(detail.chat, fromEvent = false)
            cache.saveMessages(deviceKey, chatId, flow.value.messages)
        } catch (e: IOException) {
            flow.update { it.copy(loading = false, error = describe(e)) }
        }
    }

    suspend fun loadOlder(chatId: String) {
        val flow = threadFlow(chatId)
        val oldest = flow.value.messages.firstOrNull()?.id ?: return
        if (!flow.value.hasMore || flow.value.loading) return
        flow.update { it.copy(loading = true) }
        try {
            val detail = api.chat(chatId, before = oldest, limit = PAGE)
            flow.update { t ->
                t.copy(
                    messages = detail.messages.fold(t.messages) { acc, m -> acc.upsert(m) },
                    hasMore = detail.hasMore,
                    loading = false,
                )
            }
        } catch (e: IOException) {
            flow.update { it.copy(loading = false, error = describe(e)) }
        }
    }

    /**
     * Optimistic send: the prompt appears at once and is confirmed by the `202`, or withdrawn
     * again on `409 busy` (the caller puts the text back into the composer).
     */
    suspend fun sendMessage(chatId: String, content: String): SendResult {
        val text = content.trim()
        if (text.isEmpty()) return SendResult.Failed("Nothing to send")
        val flow = threadFlow(chatId)
        val pending = PendingMessage(localIds.getAndDecrement(), text)
        flow.update { it.copy(pending = it.pending + pending) }
        fun withdraw() = flow.update { t -> t.copy(pending = t.pending.filterNot { it.localId == pending.localId }) }
        return try {
            val response = api.sendMessage(chatId, text)
            flow.update { t ->
                t.copy(
                    pending = t.pending.filterNot { it.localId == pending.localId },
                    messages = t.messages.upsert(response.message),
                )
            }
            SendResult.Sent
        } catch (e: LapisApiException) {
            withdraw()
            if (e.isBusy) SendResult.Busy else SendResult.Failed(describe(e))
        } catch (e: IOException) {
            withdraw()
            SendResult.Failed(describe(e))
        }
    }

    suspend fun stopChat(chatId: String): Result<Boolean> = call { api.stopChat(chatId) }

    // ------------------------------------------------------------------ SSH features / agent tools

    /**
     * Asks the device for its SSH key, which is also how its software is recognised. A key is
     * kept for the session; any other answer is asked for again the next time it matters,
     * because the device may have been updated in between.
     */
    suspend fun checkSshSupport(): SshSupport {
        (_sshSupport.value as? SshSupport.Ready)?.let { return it }
        // ponytail: a device without the features is asked on every thread and hosts screen it
        // opens (one small GET, answered 404); remember the "no" per connection if that shows.
        val answer = try {
            api.deviceSshKey()?.let { SshSupport.Ready(it) } ?: SshSupport.Missing
        } catch (e: IOException) {
            SshSupport.Unknown(describe(e))
        }
        // Two screens may ask at once: a key that arrived is not replaced by a failure.
        return _sshSupport.updateAndGet { if (it is SshSupport.Ready) it else answer }
    }

    /**
     * The agent tools found on a host. A list is fetched once and kept; `refresh` asks again,
     * and so does the next thread on a host whose list could not be had (SSH was not working).
     *
     * Runs in the session's scope, not the caller's: the device looks over SSH, which takes a
     * moment, and a thread closed meanwhile must not leave the host marked as loading for good.
     */
    fun loadHostAgents(hostId: String, refresh: Boolean = false) {
        if (hostId.isEmpty()) return
        val known = _hostAgents.value[hostId]
        if (known != null && (known.loading || (known.reason == null && !refresh))) return
        _hostAgents.update { it + (hostId to (known ?: HostAgentsState()).copy(loading = true)) }
        scope.launch {
            val state = when (val support = checkSshSupport()) {
                is SshSupport.Ready -> try {
                    HostAgentsState(agents = api.hostAgents(hostId))
                } catch (e: IOException) {
                    HostAgentsState(reason = describe(e))
                }

                is SshSupport.Unknown -> HostAgentsState(reason = support.error ?: "Could not reach the device")
                SshSupport.Missing -> null // older device software: nothing to ask and nothing to keep
            }
            _hostAgents.update { if (state == null) it - hostId else it + (hostId to state) }
        }
    }

    // ------------------------------------------------------------------ approvals / control

    /**
     * Sends a decision. The sheet clears as soon as the daemon accepts it; `approval_resolved`
     * (which every client receives) then confirms it for everybody else.
     */
    suspend fun decide(requestId: String, decision: String, reason: String? = null): DecideResult =
        try {
            api.decide(requestId, decision, reason)
            removeApproval(requestId)
            DecideResult.Done
        } catch (e: LapisApiException) {
            if (e.isNotFound) {
                removeApproval(requestId)
                DecideResult.AlreadyResolved
            } else {
                DecideResult.Failed(describe(e))
            }
        } catch (e: IOException) {
            // REST failed below HTTP; the socket may still be alive.
            if (source.send(ClientCommand.ApprovalDecision(requestId, decision, reason))) {
                DecideResult.Done
            } else {
                DecideResult.Failed(describe(e))
            }
        }

    /** Global kill switch: REST first, the socket as a fallback. Returns whether it was sent. */
    suspend fun killAll(): Boolean = try {
        api.killAll()
        true
    } catch (e: LapisApiException) {
        _notices.tryEmit(Notice.Toast("error", "Kill switch failed: ${describe(e)}"))
        false
    } catch (e: IOException) {
        source.send(ClientCommand.KillAll).also { sent ->
            if (!sent) _notices.tryEmit(Notice.Toast("error", "Kill switch failed: ${describe(e)}"))
        }
    }

    suspend fun pause(): Result<String> = call { api.pause() }

    suspend fun resume(): Result<String> = call { api.resume() }

    suspend fun setActiveHost(hostId: String): Result<String> = call {
        api.setActiveHost(hostId).also { active ->
            _state.update { it?.copy(activeHost = active) }
        }
    }

    suspend fun nudge(hostId: String, request: NudgeRequest): Result<Boolean> = call { api.nudge(hostId, request) }

    /** Fast (5 s) telemetry ticks while a client is looking at the cockpit; re-sent on reconnect. */
    fun setTelemetrySubscribed(enabled: Boolean) {
        telemetryWanted = enabled
        source.send(ClientCommand.SubscribeTelemetry(enabled))
    }

    suspend fun refreshTelemetry(): Result<Telemetry> = call { api.telemetry().also { _telemetry.value = it } }

    /** Rebuilds the step ticker from a chat's tool messages (after a reconnect or app start). */
    private suspend fun seedActivity(chatId: String) {
        try {
            val detail = api.chat(chatId, limit = PAGE)
            val seeded = detail.messages.filter { it.role == Message.ROLE_TOOL }.map { m ->
                Activity(
                    ts = m.metadata.ts ?: m.createdAt,
                    taskId = m.taskId.orEmpty(),
                    kind = m.metadata.kind.orEmpty(),
                    summary = m.content,
                    detail = m.metadata.detail.orEmpty(),
                    status = m.metadata.status ?: "ok",
                    chatId = chatId,
                    step = m.metadata.step ?: 0,
                    screenshotId = m.metadata.screenshotId,
                )
            }
            _activity.update { live -> if (live.isEmpty()) seeded.takeLast(MAX_ACTIVITY) else live }
        } catch (e: IOException) {
            // The ticker simply starts empty.
        }
    }

    private inline fun <T> call(block: () -> T): Result<T> = try {
        Result.success(block())
    } catch (e: IOException) {
        Result.failure(IOException(describe(e), e))
    }

    companion object {
        const val MAX_ACTIVITY = 200
        const val PAGE = 50

        /** Said wherever a control is missing because the device answered 404 for its SSH key. */
        const val NO_SSH_FEATURES = "This device software does not have the SSH features yet."

        /** A tool's name when this session has seen its host's list, otherwise its id. */
        fun agentName(id: String, known: List<HostAgent>): String =
            known.firstOrNull { it.id == id }?.name?.takeIf { it.isNotBlank() } ?: id

        /** A short, user-presentable description; never includes request or response bodies. */
        fun describe(e: Throwable): String = when (e) {
            is LapisApiException -> when {
                e.isUnauthorized -> "This phone is no longer paired with the device"
                e.status == 0 -> e.message
                else -> e.message
            }

            is java.net.UnknownHostException -> "Device not found. Is Tailscale connected?"
            is java.net.SocketTimeoutException -> "The device did not answer in time"
            is java.net.ConnectException -> "Could not reach the device"
            is javax.net.ssl.SSLException -> "Secure connection failed"
            else -> e.message?.takeIf { it.isNotBlank() } ?: "Network error"
        }

        /** Inserts or replaces by id and keeps the list ordered by id (oldest first). */
        fun List<Message>.upsert(message: Message): List<Message> {
            val index = indexOfFirst { it.id == message.id }
            if (index >= 0) {
                // Prefer the richer copy (the REST answer has task_id, the event may not).
                val merged = if (message.taskId == null) message.copy(taskId = this[index].taskId) else message
                return toMutableList().also { it[index] = merged }
            }
            if (isEmpty() || last().id < message.id) return this + message
            return (this + message).sortedBy { it.id }
        }
    }
}
