package dev.lapiszero.app.data.remote

import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

/** Where a daemon lives. `tls = false` is only ever used for a local development daemon. */
data class Endpoint(val host: String, val tls: Boolean) {
    val baseUrl: String get() = (if (tls) "https://" else "http://") + host
    val wsUrl: String get() = (if (tls) "wss://" else "ws://") + host + "/v1/ws"

    companion object {
        /** Hosts the network security config allows cleartext for (adb reverse / emulator). */
        val CLEARTEXT_HOSTS = setOf("127.0.0.1", "localhost", "10.0.2.2")

        private val HOST_RE = Regex("^[A-Za-z0-9]([A-Za-z0-9.-]{0,252}[A-Za-z0-9])?(:[0-9]{1,5})?$")

        /** `host` or `host:port`, nothing else: no scheme, path, userinfo or whitespace. */
        fun isValidHost(host: String): Boolean {
            if (!HOST_RE.matches(host)) return false
            val port = host.substringAfter(':', "")
            return port.isEmpty() || port.toInt() in 1..65535
        }

        fun hostWithoutPort(host: String): String = host.substringBefore(':')

        fun isCleartextAllowed(host: String): Boolean =
            hostWithoutPort(host).lowercase() in CLEARTEXT_HOSTS
    }
}

/** Any non-2xx answer from the daemon, with the spec's `{error:{code,message}}` decoded. */
class LapisApiException(
    val status: Int,
    val code: String,
    override val message: String,
) : IOException("$status $code: $message") {
    val isUnauthorized: Boolean get() = status == 401
    val isBusy: Boolean get() = status == 409 && code == "busy"
    val isNotPaused: Boolean get() = status == 409 && code == "not_paused"
    val isNotFound: Boolean get() = status == 404
    val isUiUnavailable: Boolean get() = status == 504
}

/** Adds `Authorization: Bearer <token>` to requests for the paired device only. */
class BearerInterceptor(
    private val host: String,
    private val port: Int,
    private val token: () -> String?,
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        val bearer = token()
        val sameOrigin = request.url.host == host && request.url.port == port
        if (bearer.isNullOrEmpty() || !sameOrigin || request.header("Authorization") != null) {
            return chain.proceed(request)
        }
        return chain.proceed(request.newBuilder().header("Authorization", "Bearer $bearer").build())
    }
}

data class Screenshot(
    val bytes: ByteArray,
    val width: Int?,
    val height: Int?,
    val clickX: Int?,
    val clickY: Int?,
)

/**
 * REST client for the daemon control plane (spec 5.4). One instance per paired device.
 *
 * Nothing here logs: request and response bodies can carry secrets and the bearer token.
 */
class LapisApiClient(
    val endpoint: Endpoint,
    token: () -> String?,
    baseClient: OkHttpClient = defaultClient(),
) {
    private val base: HttpUrl = endpoint.baseUrl.toHttpUrl()
    private val json = LapisJson

    /** Shared with the WebSocket client so both use one connection pool and one interceptor. */
    val http: OkHttpClient = baseClient.newBuilder()
        .addInterceptor(BearerInterceptor(base.host, base.port, token))
        .build()

    // -- health / devices
    suspend fun health(): Health = get("v1/health")
    suspend fun devices(): List<PairedPhone> = get<DevicesResponse>("v1/devices").devices
    suspend fun revokeDevice(id: String) = delete("v1/devices/${seg(id)}")
    suspend fun state(): DeviceState = get("v1/state")

    // -- chats
    suspend fun chats(hostId: String? = null, archived: Boolean = false): List<Chat> =
        get<ChatsResponse>(
            "v1/chats",
            buildMap {
                if (!hostId.isNullOrEmpty()) put("host_id", hostId)
                if (archived) put("archived", "1")
            },
        ).chats

    suspend fun createChat(
        hostId: String,
        title: String? = null,
        policyOverride: String? = null,
        agent: String? = null,
    ): Chat = send("POST", "v1/chats", CreateChatRequest(hostId, title, policyOverride, agent))

    suspend fun chat(id: String, before: Long? = null, limit: Int = 50): ChatDetailResponse =
        get(
            "v1/chats/${seg(id)}",
            buildMap {
                if (before != null) put("before", before.toString())
                put("limit", limit.toString())
            },
        )

    /** `fields` holds exactly the keys to change (an explicit JSON null clears an override). */
    suspend fun patchChat(id: String, fields: JsonObject): Chat =
        send("PATCH", "v1/chats/${seg(id)}", fields)

    /** `agent`: [Chat.AGENT_LAPIS] or the id of an agent tool on the chat's host. */
    suspend fun setChatAgent(id: String, agent: String): Chat =
        patchChat(id, JsonObject(mapOf("agent" to JsonPrimitive(agent))))

    /** The folder on the host a tool runs in; "" is the home folder. */
    suspend fun setChatWorkdir(id: String, workdir: String): Chat =
        patchChat(id, JsonObject(mapOf("workdir" to JsonPrimitive(workdir))))

    suspend fun deleteChat(id: String) = delete("v1/chats/${seg(id)}")

    suspend fun sendMessage(chatId: String, content: String): SendMessageResponse =
        send("POST", "v1/chats/${seg(chatId)}/messages", SendMessageRequest(content))

    suspend fun stopChat(chatId: String): Boolean =
        post<StopResponse>("v1/chats/${seg(chatId)}/stop").stopped

    // -- approvals / agent control
    suspend fun approvals(): List<Approval> = get<ApprovalsResponse>("v1/approvals").approvals

    suspend fun decide(requestId: String, decision: String, reason: String? = null): Boolean =
        send<ApprovalDecisionRequest, OkResponse>(
            "POST",
            "v1/approvals/${seg(requestId)}",
            ApprovalDecisionRequest(decision, reason?.takeIf { it.isNotBlank() }),
        ).ok

    suspend fun killAll() = postUnit("v1/agent/kill-all")
    suspend fun pause(): String = post<AgentStateResponse>("v1/agent/pause").agentState
    suspend fun resume(): String = post<AgentStateResponse>("v1/agent/resume").agentState

    suspend fun setActiveHost(hostId: String): String =
        send<ActiveHostRequest, ActiveHostResponse>(
            "PUT",
            "v1/agent/active-host",
            ActiveHostRequest(hostId),
        ).activeHost

    suspend fun nudge(hostId: String, nudge: NudgeRequest): Boolean =
        send<NudgeRequest, OkResponse>("POST", "v1/hosts/${seg(hostId)}/nudge", nudge).ok

    suspend fun screenshot(id: Long): Screenshot {
        val response = execute(request("GET", "v1/screenshots/$id.jpg")).ensureSuccess()
        return Screenshot(
            bytes = response.bytes,
            width = response.header("X-Lapis-Width")?.toIntOrNull(),
            height = response.header("X-Lapis-Height")?.toIntOrNull(),
            clickX = response.header("X-Lapis-Click-X")?.toIntOrNull(),
            clickY = response.header("X-Lapis-Click-Y")?.toIntOrNull(),
        )
    }

    // -- configuration
    suspend fun config(): JsonObject = get<ConfigEnvelope>("v1/config").config

    suspend fun putConfig(config: JsonObject): JsonObject =
        send<ConfigEnvelope, ConfigEnvelope>("PUT", "v1/config", ConfigEnvelope(config)).config

    suspend fun hosts(): HostsResponse = get("v1/hosts")

    suspend fun putHosts(hosts: Map<String, Host>): HostsResponse =
        send("PUT", "v1/hosts", HostsRequest(hosts))

    suspend fun probe(hostId: String): ProbeResult = post("v1/hosts/${seg(hostId)}/probe")

    /**
     * The device's own SSH key, or null when the device software is from before the SSH
     * features: it answers 404 here. Such a device has no authorising and no agent tools, and
     * is sent neither `ssh.password_env` nor a chat's `agent` or `workdir`.
     */
    suspend fun deviceSshKey(): DeviceSshKey? = try {
        get<DeviceSshKey>("v1/device/ssh-key")
    } catch (e: LapisApiException) {
        if (e.isNotFound) null else throw e
    }

    /** Adds the device's public key to the host's `~/.ssh/authorized_keys`; 4xx when no login works. */
    suspend fun authorizeSsh(hostId: String): SshAuthorizeResult =
        post("v1/hosts/${seg(hostId)}/ssh/authorize")

    /** The agent tools found on the host; 4xx with the reason when SSH does not work for it. */
    suspend fun hostAgents(hostId: String): List<HostAgent> =
        get<HostAgentsResponse>("v1/hosts/${seg(hostId)}/agents").agents

    suspend fun secrets(): List<SecretStatus> = get<SecretsResponse>("v1/secrets/status").secrets

    suspend fun patchSecrets(
        set: Map<String, String> = emptyMap(),
        unset: List<String> = emptyList(),
    ): List<SecretStatus> =
        send<SecretsPatch, SecretsResponse>("PATCH", "v1/secrets", SecretsPatch(set, unset)).secrets

    suspend fun schedules(): List<Schedule> = get<SchedulesResponse>("v1/schedules").schedules

    suspend fun createSchedule(schedule: ScheduleCreate): List<Schedule> =
        send<ScheduleCreate, SchedulesResponse>("POST", "v1/schedules", schedule).schedules

    suspend fun patchSchedule(name: String, patch: SchedulePatch): List<Schedule> =
        send<SchedulePatch, SchedulesResponse>("PATCH", "v1/schedules/${seg(name)}", patch).schedules

    suspend fun deleteSchedule(name: String) = delete("v1/schedules/${seg(name)}")

    suspend fun runSchedule(name: String): ScheduleRunResponse =
        post("v1/schedules/${seg(name)}/run")

    // -- device cockpit
    suspend fun telemetry(): Telemetry = get("v1/device/telemetry")

    suspend fun patchUi(patch: UiPatch): UiSettings = send("PATCH", "v1/device/ui", patch)

    /** PNG bytes of the 320x170 LCD; throws [LapisApiException] with status 504 when lapis-ui is away. */
    suspend fun screenPng(): ByteArray =
        execute(request("GET", "v1/device/screen.png")).ensureSuccess().bytes

    suspend fun power(action: String): PowerResponse =
        send("POST", "v1/device/power", PowerRequest(action))

    // ------------------------------------------------------------------ plumbing

    private suspend inline fun <reified T> get(
        path: String,
        query: Map<String, String> = emptyMap(),
    ): T = decode(execute(request("GET", path, query = query)))

    private suspend inline fun <reified T> post(path: String): T =
        decode(execute(request("POST", path, body = EMPTY_JSON)))

    private suspend fun postUnit(path: String) {
        execute(request("POST", path, body = EMPTY_JSON)).ensureSuccess()
    }

    private suspend fun delete(path: String) {
        execute(request("DELETE", path)).ensureSuccess()
    }

    private suspend inline fun <reified B, reified T> send(method: String, path: String, body: B): T =
        decode(execute(request(method, path, body = json.encodeToString(body).toRequestBody(JSON))))

    private inline fun <reified T> decode(response: RawResponse): T {
        response.ensureSuccess()
        return try {
            json.decodeFromString<T>(response.text())
        } catch (e: IllegalArgumentException) { // SerializationException
            throw LapisApiException(
                response.code,
                "bad_response",
                "Unexpected response from the device",
            )
        }
    }

    private fun request(
        method: String,
        path: String,
        query: Map<String, String> = emptyMap(),
        body: RequestBody? = null,
    ): Request {
        val url = base.newBuilder().addEncodedPathSegments(path).apply {
            query.forEach { (k, v) -> addQueryParameter(k, v) }
        }.build()
        return Request.Builder().url(url).method(method, body).build()
    }

    private suspend fun execute(request: Request): RawResponse = http.newCall(request).await()

    companion object {
        private val JSON = "application/json; charset=utf-8".toMediaType()
        private val EMPTY_JSON = "{}".toRequestBody(JSON)

        /** Percent-encodes one path segment (ids and schedule names may contain spaces). */
        internal fun seg(raw: String): String =
            HttpUrl.Builder().scheme("http").host("x").addPathSegment(raw).build()
                .encodedPathSegments.first()

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            // Probe, agent-tool detection and authorise log in to the host first: usually a few
            // seconds, about 45 s in the worst case (the daemon's own figure).
            .readTimeout(60, TimeUnit.SECONDS)
            .writeTimeout(20, TimeUnit.SECONDS)
            .followRedirects(false) // the API never redirects; never carry a token elsewhere
            .followSslRedirects(false)
            .build()


        /**
         * `POST /v1/pair/exchange`: the one unauthenticated call. Cleartext is refused for
         * anything but a loopback development daemon before any byte leaves the phone.
         */
        suspend fun pairExchange(
            endpoint: Endpoint,
            nonce: String,
            deviceName: String,
            deviceModel: String,
            client: OkHttpClient = defaultClient(),
        ): PairExchangeResponse {
            requireEndpointAllowed(endpoint)
            val body = LapisJson.encodeToString(PairExchangeRequest(nonce, deviceName, deviceModel))
            val request = Request.Builder()
                .url(endpoint.baseUrl.toHttpUrl().newBuilder().addEncodedPathSegments("v1/pair/exchange").build())
                .post(body.toRequestBody(JSON))
                .build()
            val response = client.newCall(request).await().ensureSuccess()
            return try {
                LapisJson.decodeFromString<PairExchangeResponse>(response.text())
            } catch (e: IllegalArgumentException) {
                throw LapisApiException(response.code, "bad_response", "Unexpected pairing response")
            }
        }

        fun requireEndpointAllowed(endpoint: Endpoint) {
            if (!Endpoint.isValidHost(endpoint.host)) {
                throw LapisApiException(0, "bad_host", "Not a valid host name")
            }
            if (!endpoint.tls && !Endpoint.isCleartextAllowed(endpoint.host)) {
                throw LapisApiException(
                    0,
                    "cleartext_refused",
                    "Plain HTTP is only allowed for a development daemon on 127.0.0.1, " +
                        "localhost or 10.0.2.2",
                )
            }
        }
    }
}

/** A fully buffered response: the body is read on OkHttp's thread, never on the caller's. */
class RawResponse(val code: Int, private val headers: okhttp3.Headers, val bytes: ByteArray) {
    val isSuccessful: Boolean get() = code in 200..299

    fun header(name: String): String? = headers[name]

    fun text(): String = bytes.toString(Charsets.UTF_8)

    /** Throws [LapisApiException] carrying the daemon's `{error:{code,message}}` on non-2xx. */
    fun ensureSuccess(): RawResponse {
        if (isSuccessful) return this
        val error = runCatching { LapisJson.decodeFromString<ApiErrorEnvelope>(text()).error }
            .getOrNull()
        throw LapisApiException(
            status = code,
            code = error?.code?.ifEmpty { null } ?: "http_$code",
            message = error?.message?.ifEmpty { null } ?: "HTTP $code",
        )
    }
}

/** Bridges an OkHttp [Call] into a cancellable suspend call that returns a buffered body. */
suspend fun Call.await(): RawResponse = suspendCancellableCoroutine { cont ->
    cont.invokeOnCancellation { cancel() }
    enqueue(object : Callback {
        override fun onFailure(call: Call, e: IOException) {
            if (!cont.isCancelled) cont.resumeWithException(e)
        }

        override fun onResponse(call: Call, response: Response) {
            val raw = try {
                response.use { RawResponse(it.code, it.headers, it.body.bytes()) }
            } catch (e: IOException) {
                if (!cont.isCancelled) cont.resumeWithException(e)
                return
            }
            if (!cont.isCancelled) cont.resume(raw)
        }
    })
}
