package dev.lapiszero.app.data.remote

import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

/**
 * DTOs locked to ANDROID_COMPANION_SPEC.md section 5.4 ("Wire Shapes").
 *
 * Every field name is the daemon's snake_case name via [SerialName]. Enum-like strings stay
 * strings on purpose: a newer daemon may add values and the app must keep decoding.
 */
val LapisJson: Json = Json {
    ignoreUnknownKeys = true // spec: "Clients must ignore unknown fields"
    explicitNulls = false // absent == null in both directions (PATCH bodies stay partial)
    encodeDefaults = true
    coerceInputValues = true
    isLenient = false
}

/** Unix seconds. The spec says integers; a float from an older daemon is truncated, not fatal. */
object UnixSecondsSerializer : KSerializer<Long> {
    override val descriptor: SerialDescriptor =
        PrimitiveSerialDescriptor("UnixSeconds", PrimitiveKind.LONG)

    override fun serialize(encoder: Encoder, value: Long) = encoder.encodeLong(value)

    override fun deserialize(decoder: Decoder): Long {
        if (decoder is JsonDecoder) {
            val p = decoder.decodeJsonElement().jsonPrimitive
            return p.longOrNull ?: p.doubleOrNull?.toLong()
                ?: throw kotlinx.serialization.SerializationException("Not a timestamp: $p")
        }
        return decoder.decodeLong()
    }
}

typealias UnixSeconds =
    @Serializable(with = UnixSecondsSerializer::class)
    Long

// ---------------------------------------------------------------- errors

@Serializable
data class ApiErrorEnvelope(val error: ApiErrorBody? = null)

@Serializable
data class ApiErrorBody(val code: String = "", val message: String = "")

// ---------------------------------------------------------------- health / pairing / devices

@Serializable
data class Health(
    val ok: Boolean = false,
    val version: String = "",
    val hostname: String = "",
    @SerialName("uptime_sec") val uptimeSec: Long = 0,
    val mock: Boolean = false,
    @SerialName("device_id") val deviceId: String = "",
)

@Serializable
data class PairExchangeRequest(
    val nonce: String,
    @SerialName("device_name") val deviceName: String,
    @SerialName("device_model") val deviceModel: String,
)

@Serializable
data class PairExchangeResponse(
    @SerialName("device_id") val deviceId: String,
    @SerialName("api_token") val apiToken: String,
    val hostname: String = "",
) {
    // The token must never reach a log line through an accidental string template.
    override fun toString(): String = "PairExchangeResponse(deviceId=$deviceId, hostname=$hostname)"
}

@Serializable
data class PairedPhone(
    val id: String,
    val name: String = "",
    val model: String = "",
    @SerialName("created_at") val createdAt: UnixSeconds? = null,
    @SerialName("last_seen_at") val lastSeenAt: UnixSeconds? = null,
    val revoked: Boolean = false,
    val current: Boolean = false,
)

@Serializable
data class DevicesResponse(val devices: List<PairedPhone> = emptyList())

// ---------------------------------------------------------------- chats

@Serializable
data class Chat(
    val id: String,
    val title: String = "",
    @SerialName("host_id") val hostId: String = "",
    @SerialName("model_override") val modelOverride: String? = null,
    @SerialName("policy_override") val policyOverride: String? = null,
    val archived: Boolean = false,
    @SerialName("created_at") val createdAt: UnixSeconds = 0,
    @SerialName("updated_at") val updatedAt: UnixSeconds = 0,
    @SerialName("last_message") val lastMessage: Message? = null,
    // Live fields: present on REST responses, absent on `chat_updated` events.
    val active: Boolean = false,
    val step: Int? = null,
    @SerialName("pending_approvals") val pendingApprovals: Int = 0,
    /**
     * Who answers in this chat: [AGENT_LAPIS] (the device's own agent) or the id of an agent
     * tool on the chat's host. Absent on device software from before these chats.
     */
    val agent: String = AGENT_LAPIS,
    /** Folder on the host a tool runs in; empty means the home folder. */
    val workdir: String = "",
) {
    companion object {
        const val AGENT_LAPIS = "lapis"
    }
}

@Serializable
data class Message(
    val id: Long,
    @SerialName("chat_id") val chatId: String = "",
    val role: String = ROLE_ASSISTANT,
    val content: String = "",
    @SerialName("task_id") val taskId: String? = null,
    val metadata: MessageMetadata = MessageMetadata(),
    @SerialName("created_at") val createdAt: UnixSeconds = 0,
) {
    companion object {
        const val ROLE_USER = "user"
        const val ROLE_ASSISTANT = "assistant"
        const val ROLE_TOOL = "tool"
        const val ROLE_SYSTEM = "system"
    }
}

/** Union of the per-role metadata objects (tool / user / assistant); all optional. */
@Serializable
data class MessageMetadata(
    // tool
    val kind: String? = null,
    val status: String? = null, // tool: ok|denied|error; assistant: done|cancelled|failed|stopped
    val detail: String? = null,
    @SerialName("screenshot_id") val screenshotId: Long? = null,
    val step: Int? = null,
    val ts: UnixSeconds? = null,
    // user
    val source: String? = null,
)

@Serializable
data class ChatsResponse(val chats: List<Chat> = emptyList())

@Serializable
data class ChatDetailResponse(
    val chat: Chat,
    val messages: List<Message> = emptyList(),
    @SerialName("has_more") val hasMore: Boolean = false,
)

@Serializable
data class CreateChatRequest(
    @SerialName("host_id") val hostId: String,
    val title: String? = null,
    @SerialName("policy_override") val policyOverride: String? = null,
)

@Serializable
data class SendMessageRequest(val content: String)

@Serializable
data class SendMessageResponse(
    val message: Message,
    @SerialName("task_id") val taskId: String? = null,
)

@Serializable
data class StopResponse(val stopped: Boolean = false)

// ---------------------------------------------------------------- approvals / activity

@Serializable
data class Approval(
    @SerialName("request_id") val requestId: String,
    val host: String = "",
    val action: String = "",
    val summary: String = "",
    val risk: String = "high",
    @SerialName("preview_1bit_120x68_b64") val preview1BitB64: String? = null,
    @SerialName("task_id") val taskId: String = "",
    @SerialName("chat_id") val chatId: String? = null,
    val step: Int = 0,
    @SerialName("screenshot_id") val screenshotId: Long? = null,
    @SerialName("click_x") val clickX: Int? = null,
    @SerialName("click_y") val clickY: Int? = null,
)

@Serializable
data class ApprovalsResponse(val approvals: List<Approval> = emptyList())

@Serializable
data class ApprovalDecisionRequest(val decision: String, val reason: String? = null)

object Decision {
    const val APPROVE_ONCE = "approve_once"
    const val APPROVE_SESSION = "approve_session"
    const val DENY = "deny"
}

@Serializable
data class OkResponse(val ok: Boolean = false)

@Serializable
data class Activity(
    val ts: UnixSeconds = 0,
    @SerialName("task_id") val taskId: String = "",
    val kind: String = "",
    val summary: String = "",
    val detail: String = "",
    val status: String = "ok",
    @SerialName("chat_id") val chatId: String? = null,
    val step: Int = 0,
    @SerialName("screenshot_id") val screenshotId: Long? = null,
) {
    companion object {
        /** A chat's message being run by an agent tool on the host. */
        const val KIND_AGENT_RUN = "agent_run"
    }
}

// ---------------------------------------------------------------- state

@Serializable
data class DeviceState(
    @SerialName("agent_state") val agentState: String = AgentState.IDLE,
    /**
     * The activity kind of the step being executed right now (`screenshot`, `ssh_exec`,
     * `vnc_click`, ...): non-empty only while [agentState] is `ACTING`. Null when the device
     * did not send the field: software from before it, which is not the same as "no step".
     */
    val action: String? = null,
    @SerialName("active_host") val activeHost: String = "",
    val hosts: List<String> = emptyList(),
    @SerialName("active_task") val activeTask: ActiveTask? = null,
    @SerialName("active_chat_id") val activeChatId: String? = null,
    @SerialName("next_cron") val nextCron: String = "",
    @SerialName("wifi_ssid") val wifiSsid: String = "",
    @SerialName("local_ip") val localIp: String = "",
    @SerialName("tailscale_state") val tailscaleState: String = "",
    @SerialName("tailscale_ip") val tailscaleIp: String = "",
    @SerialName("tailscale_fqdn") val tailscaleFqdn: String = "",
    @SerialName("tailscale_auth_url") val tailscaleAuthUrl: String? = null,
    @SerialName("api_key_ready") val apiKeyReady: Boolean = false,
    @SerialName("battery_pct") val batteryPct: Int? = null,
    @SerialName("battery_charging") val batteryCharging: Boolean = false,
    /** The device clock's offset from UTC in minutes; absent on daemons older than 0.3. */
    @SerialName("utc_offset_min") val utcOffsetMin: Int? = null,
    @SerialName("pending_approvals") val pendingApprovals: List<Approval> = emptyList(),
    val pairing: Pairing? = null,
    val ui: UiSettings = UiSettings(),
    val device: DeviceInfo = DeviceInfo(),
) {
    val isRunning: Boolean get() = agentState != AgentState.IDLE
    val canNudge: Boolean
        get() = agentState == AgentState.PAUSED || agentState == AgentState.WAITING_APPROVAL
}

object AgentState {
    const val IDLE = "IDLE"
    const val LISTENING = "LISTENING" // push-to-talk; no daemon publishes it yet
    const val THINKING = "THINKING"
    const val ACTING = "ACTING"
    const val WAITING_APPROVAL = "WAITING_APPROVAL"
    const val PAUSED = "PAUSED"
}

@Serializable
data class ActiveTask(
    val id: String = "",
    val title: String = "",
    val step: Int = 0,
    @SerialName("max_steps") val maxSteps: Int = 0,
)

@Serializable
data class UiSettings(
    val theme: String = "dark",
    val brightness: Int = 100,
    @SerialName("mute_audio") val muteAudio: Boolean = false,
    /** IANA zone name of the device clock, or "" when it was never set. */
    val timezone: String = "",
)

@Serializable
data class UiPatch(
    val theme: String? = null,
    val brightness: Int? = null,
    @SerialName("mute_audio") val muteAudio: Boolean? = null,
    val wake: Boolean? = null,
    /** IANA zone name, e.g. "America/Los_Angeles"; an unknown zone answers 422. */
    val timezone: String? = null,
)

@Serializable
data class DeviceInfo(
    val hostname: String = "",
    val version: String = "",
    val mock: Boolean = false,
)

/** `mode` is "qr", "pin" or "off"; the other fields depend on the mode. */
@Serializable
data class Pairing(
    val mode: String = "off",
    val payload: String? = null,
    val host: String? = null,
    val pin: String? = null,
    @SerialName("device_name") val deviceName: String? = null,
    val status: String? = null,
    @SerialName("expires_in") val expiresIn: Int? = null,
    @SerialName("expires_at") val expiresAt: UnixSeconds? = null,
) {
    val isActive: Boolean get() = mode == "qr" || mode == "pin"
}

@Serializable
data class AgentStateResponse(@SerialName("agent_state") val agentState: String = "")

@Serializable
data class ActiveHostRequest(@SerialName("host_id") val hostId: String)

@Serializable
data class ActiveHostResponse(@SerialName("active_host") val activeHost: String = "")

// ---------------------------------------------------------------- hosts / config / secrets / schedules

@Serializable
data class Host(
    val hostname: String = "",
    @SerialName("tailscale_ip") val tailscaleIp: String = "",
    val os: String = "macos",
    val policy: String = "balanced",
    val vnc: HostVnc = HostVnc(),
    val ssh: HostSsh = HostSsh(),
)

@Serializable
data class HostVnc(
    val port: Int = 5900,
    val username: String = "",
    @SerialName("password_env") val passwordEnv: String = "",
)

@Serializable
data class HostSsh(
    val port: Int = 22,
    val user: String = "",
    @SerialName("identity_file") val identityFile: String? = null,
    /**
     * NAME of the vault secret holding the SSH password; "" means key only. Null when the
     * device did not send the field (software from before SSH passwords): with
     * `explicitNulls = false` it is then not sent back either.
     */
    @SerialName("password_env") val passwordEnv: String? = null,
)

@Serializable
data class HostsResponse(
    val hosts: Map<String, Host> = emptyMap(),
    @SerialName("active_host") val activeHost: String = "",
)

@Serializable
data class HostsRequest(val hosts: Map<String, Host>)

@Serializable
data class ProbeCheck(
    val ok: Boolean = false,
    @SerialName("latency_ms") val latencyMs: Int? = null,
    val detail: String = "",
)

@Serializable
data class ProbeResult(
    @SerialName("host_id") val hostId: String = "",
    val tailscale: ProbeCheck = ProbeCheck(),
    val vnc: ProbeCheck = ProbeCheck(),
    val ssh: ProbeCheck = ProbeCheck(),
    /** Agent tools found on the host; empty when SSH is down or the device software is older. */
    val agents: List<HostAgent> = emptyList(),
)

/** An agent tool on a host (Claude Code, opencode, Freya, ...) that a chat can run with. */
@Serializable
data class HostAgent(val id: String, val name: String = "", val path: String = "")

@Serializable
data class HostAgentsResponse(val agents: List<HostAgent> = emptyList())

/** `POST /v1/hosts/{id}/ssh/authorize`: `already` when the device's key was on the host before. */
@Serializable
data class SshAuthorizeResult(
    val ok: Boolean = false,
    val already: Boolean = false,
    val detail: String = "",
)

/**
 * `lapis.toml` as the daemon returns it. Kept as a raw object so that sections this app version
 * does not know about survive a read-modify-write round trip untouched.
 */
@Serializable
data class ConfigEnvelope(val config: JsonObject = JsonObject(emptyMap()))

@Serializable
data class SecretStatus(
    val name: String,
    @SerialName("is_set") val isSet: Boolean = false,
    val masked: String? = null,
)

@Serializable
data class SecretsResponse(val secrets: List<SecretStatus> = emptyList())

/** Write-only: values travel to the device and are never echoed back. */
@Serializable
data class SecretsPatch(
    val set: Map<String, String> = emptyMap(),
    val unset: List<String> = emptyList(),
) {
    override fun toString(): String = "SecretsPatch(set=${set.keys}, unset=$unset)"
}

@Serializable
data class Schedule(
    val name: String,
    val schedule: String = "",
    val host: String = "",
    val prompt: String = "",
    val enabled: Boolean = false,
    @SerialName("next_run") val nextRun: UnixSeconds? = null,
    @SerialName("last_run_at") val lastRunAt: UnixSeconds? = null,
    @SerialName("last_chat_id") val lastChatId: String? = null,
)

@Serializable
data class SchedulesResponse(val schedules: List<Schedule> = emptyList())

@Serializable
data class ScheduleCreate(
    val name: String,
    val schedule: String,
    val host: String,
    val prompt: String,
    val enabled: Boolean,
)

@Serializable
data class SchedulePatch(
    val name: String? = null,
    val schedule: String? = null,
    val host: String? = null,
    val prompt: String? = null,
    val enabled: Boolean? = null,
)

@Serializable
data class ScheduleRunResponse(
    @SerialName("chat_id") val chatId: String = "",
    @SerialName("task_id") val taskId: String = "",
)

// ---------------------------------------------------------------- telemetry / device

@Serializable
data class Telemetry(
    val battery: Battery? = null,
    val cpu: Cpu = Cpu(),
    val memory: Memory = Memory(),
    val wifi: Wifi = Wifi(),
    val tailscale: TailscaleInfo = TailscaleInfo(),
    @SerialName("uptime_sec") val uptimeSec: Long = 0,
    val services: Map<String, String> = emptyMap(),
) {
    @Serializable
    data class Battery(
        val percent: Int = 0,
        @SerialName("voltage_v") val voltageV: Double = 0.0,
        val charging: Boolean = false,
    )

    @Serializable
    data class Cpu(
        @SerialName("temp_c") val tempC: Double? = null,
        val load1: Double? = null,
    )

    @Serializable
    data class Memory(
        @SerialName("total_mb") val totalMb: Double = 0.0,
        @SerialName("available_mb") val availableMb: Double = 0.0,
        @SerialName("zram_total_mb") val zramTotalMb: Double = 0.0,
        @SerialName("zram_used_mb") val zramUsedMb: Double = 0.0,
        @SerialName("daemon_rss_mb") val daemonRssMb: Double = 0.0,
        @SerialName("ui_rss_mb") val uiRssMb: Double = 0.0,
    ) {
        /** Memory in active use system-wide; the CM0 budget is 220 MB. */
        val usedMb: Double get() = (totalMb - availableMb).coerceAtLeast(0.0)
        val overBudget: Boolean get() = usedMb > RSS_BUDGET_MB

        companion object {
            const val RSS_BUDGET_MB = 220.0
        }
    }

    @Serializable
    data class Wifi(
        val ssid: String = "",
        val ip: String = "",
        @SerialName("rssi_dbm") val rssiDbm: Int? = null,
    )

    @Serializable
    data class TailscaleInfo(
        val state: String = "",
        val ip: String = "",
        val fqdn: String = "",
        val peers: Int = 0,
    )
}

@Serializable
data class PowerRequest(val action: String)

@Serializable
data class PowerResponse(val ok: Boolean = false, val action: String = "")

/** `GET /v1/device/ssh-key`: the device's own key. Only the public half ever leaves the device. */
@Serializable
data class DeviceSshKey(
    @SerialName("public_key") val publicKey: String = "",
    val fingerprint: String = "",
)

object PowerAction {
    const val REBOOT = "reboot"
    const val SHUTDOWN = "shutdown"
    const val RESTART_DAEMON = "restart_daemon"
    const val START_BLE_PAIRING = "start_ble_pairing"
    const val START_QR_PAIRING = "start_qr_pairing"
    const val STOP_PAIRING = "stop_pairing"
}

@Serializable
data class NudgeRequest(
    val action: String, // click | type | key
    val x: Int? = null,
    val y: Int? = null,
    val text: String? = null,
    val combo: String? = null,
)

// ---------------------------------------------------------------- WebSocket

@Serializable
data class TaskFinished(
    @SerialName("task_id") val taskId: String = "",
    @SerialName("chat_id") val chatId: String? = null,
    val host: String = "",
    val status: String = "",
    val summary: String = "",
)

@Serializable
data class ApprovalResolved(
    @SerialName("request_id") val requestId: String,
    val decision: String = "",
    val by: String = "",
)

@Serializable
private data class ChatMessageCreatedWire(
    @SerialName("chat_id") val chatId: String,
    val message: Message,
)

@Serializable
private data class ChatUpdatedWire(val chat: Chat)

@Serializable
private data class ChatDeletedWire(@SerialName("chat_id") val chatId: String)

@Serializable
private data class TelemetryTickWire(val telemetry: Telemetry)

@Serializable
private data class ToastWire(val level: String = "info", val text: String = "")

@Serializable
private data class PongWire(val ts: UnixSeconds = 0)

/** Server -> client frames on `/v1/ws`. */
sealed interface LapisEvent {
    data class StateSync(val state: DeviceState) : LapisEvent
    data class ActivityEvent(val activity: Activity) : LapisEvent
    data class ApprovalRequest(val approval: Approval) : LapisEvent
    data class ApprovalResolvedEvent(val resolved: ApprovalResolved) : LapisEvent
    data class TaskFinishedEvent(val finished: TaskFinished) : LapisEvent
    data class ChatMessageCreated(val chatId: String, val message: Message) : LapisEvent
    data class ChatUpdated(val chat: Chat) : LapisEvent
    data class ChatDeleted(val chatId: String) : LapisEvent
    data class PairingEvent(val pairing: Pairing) : LapisEvent
    data class TelemetryTick(val telemetry: Telemetry) : LapisEvent
    data class Toast(val level: String, val text: String) : LapisEvent
    data class Pong(val ts: Long) : LapisEvent

    /** A frame type this app version does not know; ignored by design. */
    data class Unknown(val type: String) : LapisEvent

    companion object {
        /** Returns null for frames that are not a JSON object with a string `type`. */
        fun parse(text: String, json: Json = LapisJson): LapisEvent? {
            val obj = runCatching { json.parseToJsonElement(text) }.getOrNull() as? JsonObject
                ?: return null
            val type = (obj["type"] as? JsonPrimitive)?.contentOrNull ?: return null
            return runCatching { decode(type, obj, json) }.getOrNull()
        }

        private fun decode(type: String, obj: JsonObject, json: Json): LapisEvent = when (type) {
            "state_sync" -> StateSync(json.decodeFromJsonElement<DeviceState>(obj))
            "activity_event" -> ActivityEvent(json.decodeFromJsonElement<Activity>(obj))
            "approval_request" -> ApprovalRequest(json.decodeFromJsonElement<Approval>(obj))
            "approval_resolved" ->
                ApprovalResolvedEvent(json.decodeFromJsonElement<ApprovalResolved>(obj))
            "task_finished" -> TaskFinishedEvent(json.decodeFromJsonElement<TaskFinished>(obj))
            "chat_message_created" ->
                json.decodeFromJsonElement<ChatMessageCreatedWire>(obj)
                    .let { ChatMessageCreated(it.chatId, it.message) }
            "chat_updated" -> ChatUpdated(json.decodeFromJsonElement<ChatUpdatedWire>(obj).chat)
            "chat_deleted" -> ChatDeleted(json.decodeFromJsonElement<ChatDeletedWire>(obj).chatId)
            "pairing" -> PairingEvent(json.decodeFromJsonElement<Pairing>(obj))
            "telemetry_tick" ->
                TelemetryTick(json.decodeFromJsonElement<TelemetryTickWire>(obj).telemetry)
            "toast" -> json.decodeFromJsonElement<ToastWire>(obj).let { Toast(it.level, it.text) }
            "pong" -> Pong(json.decodeFromJsonElement<PongWire>(obj).ts)
            else -> Unknown(type)
        }
    }
}

/** Client -> server frames on `/v1/ws`. */
sealed interface ClientCommand {
    fun toJson(): JsonElement

    data object Ping : ClientCommand {
        override fun toJson() = obj("type" to JsonPrimitive("ping"))
    }

    data class SubscribeTelemetry(val enabled: Boolean) : ClientCommand {
        override fun toJson() = obj(
            "type" to JsonPrimitive("subscribe_telemetry"),
            "enabled" to JsonPrimitive(enabled),
        )
    }

    data class SendPrompt(val chatId: String, val content: String) : ClientCommand {
        override fun toJson() = obj(
            "type" to JsonPrimitive("send_prompt"),
            "chat_id" to JsonPrimitive(chatId),
            "content" to JsonPrimitive(content),
        )
    }

    data class ApprovalDecision(
        val requestId: String,
        val decision: String,
        val reason: String? = null,
    ) : ClientCommand {
        override fun toJson() = obj(
            "type" to JsonPrimitive("approval_decision"),
            "request_id" to JsonPrimitive(requestId),
            "decision" to JsonPrimitive(decision),
            *(if (reason.isNullOrBlank()) emptyArray() else arrayOf("reason" to JsonPrimitive(reason))),
        )
    }

    data object KillAll : ClientCommand {
        override fun toJson() = obj("type" to JsonPrimitive("kill_all"))
    }

    fun encode(): String = toJson().toString()
}

private fun obj(vararg pairs: Pair<String, JsonElement>): JsonElement = JsonObject(mapOf(*pairs))
