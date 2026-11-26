package dev.lapiszero.app.ui.config

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.lapiszero.app.data.remote.Host
import dev.lapiszero.app.data.remote.HostsResponse
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.PairedPhone
import dev.lapiszero.app.data.remote.ProbeResult
import dev.lapiszero.app.data.remote.Schedule
import dev.lapiszero.app.data.remote.ScheduleCreate
import dev.lapiszero.app.data.remote.SchedulePatch
import dev.lapiszero.app.data.remote.SecretStatus
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.HostEntry
import dev.lapiszero.app.domain.HostOffer
import dev.lapiszero.app.domain.HostPushOutcome
import dev.lapiszero.app.domain.HostSync
import dev.lapiszero.app.domain.PairedDevice
import dev.lapiszero.app.domain.SshSupport
import java.io.IOException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull

data class Section<T>(val data: T? = null, val loading: Boolean = false, val error: String? = null)

sealed interface ProbeUi {
    data object Running : ProbeUi
    data class Done(val result: ProbeResult) : ProbeUi
    data class Failed(val message: String) : ProbeUi
}

/**
 * The agent / LLM part of `lapis.toml` this app edits. It is read from and written back into
 * the raw config object, so every key the app does not know about is preserved untouched.
 */
data class AgentSettings(
    val provider: String = "anthropic",
    val model: String = "",
    val apiKeyEnv: String = "",
    val effort: String = "medium",
    val baseUrl: String = "",
    val maxSteps: Int = 25,
    val requestTimeoutSec: Int = 120,
    val stuckThreshold: Int = 3,
    val vncIdleDisconnectSec: Int = 60,
    val approvalDefaultTier: String = "balanced",
) {
    fun applyTo(config: JsonObject): JsonObject = config
        .withSection("llm") {
            put("provider", JsonPrimitive(provider))
            put("model", JsonPrimitive(model.trim()))
            put("api_key_env", JsonPrimitive(apiKeyEnv.trim()))
            put("effort", JsonPrimitive(effort))
            put("base_url", JsonPrimitive(baseUrl.trim()))
            put("max_steps", JsonPrimitive(maxSteps))
            put("request_timeout_sec", JsonPrimitive(requestTimeoutSec))
        }
        .withSection("kvm") {
            put("stuck_threshold", JsonPrimitive(stuckThreshold))
            put("vnc_idle_disconnect_sec", JsonPrimitive(vncIdleDisconnectSec))
        }
        .withSection("security") {
            put("approval_default_tier", JsonPrimitive(approvalDefaultTier))
        }

    companion object {
        val PROVIDERS = listOf("anthropic", "openai", "openrouter")
        val EFFORTS = listOf("low", "medium", "high", "xhigh", "max")
        val POLICIES = listOf("strict", "balanced", "autonomous")

        fun from(config: JsonObject): AgentSettings {
            val defaults = AgentSettings()
            val llm = config["llm"] as? JsonObject
            val kvm = config["kvm"] as? JsonObject
            val security = config["security"] as? JsonObject
            return AgentSettings(
                provider = llm.str("provider") ?: defaults.provider,
                model = llm.str("model") ?: defaults.model,
                apiKeyEnv = llm.str("api_key_env") ?: defaults.apiKeyEnv,
                effort = llm.str("effort") ?: defaults.effort,
                baseUrl = llm.str("base_url") ?: defaults.baseUrl,
                maxSteps = llm.int("max_steps") ?: defaults.maxSteps,
                requestTimeoutSec = llm.int("request_timeout_sec") ?: defaults.requestTimeoutSec,
                stuckThreshold = kvm.int("stuck_threshold") ?: defaults.stuckThreshold,
                vncIdleDisconnectSec = kvm.int("vnc_idle_disconnect_sec") ?: defaults.vncIdleDisconnectSec,
                approvalDefaultTier = security.str("approval_default_tier") ?: defaults.approvalDefaultTier,
            )
        }

        private fun JsonObject?.str(key: String): String? = (this?.get(key) as? JsonPrimitive)?.contentOrNull

        private fun JsonObject?.int(key: String): Int? = (this?.get(key) as? JsonPrimitive)?.intOrNull

        private fun JsonObject.withSection(
            name: String,
            edit: MutableMap<String, JsonElement>.() -> Unit,
        ): JsonObject {
            val section = ((this[name] as? JsonObject) ?: JsonObject(emptyMap())).toMutableMap().apply(edit)
            return JsonObject(this + (name to JsonObject(section)))
        }
    }
}

/**
 * One-tap LLM provider settings.
 *
 * The device speaks two wire protocols: Anthropic's Messages API (`anthropic`) and the
 * OpenAI-compatible chat-completions API (`openai`, `openrouter`). Every other provider is
 * the second one with its own base URL and its own key, so a preset is nothing more than
 * those values filled in. Anything not listed here works the same way through the fields.
 */
data class ProviderPreset(
    val name: String,
    /** `llm.provider` on the device. */
    val provider: String,
    /** Empty means the provider's own default endpoint on the device. */
    val baseUrl: String,
    /** Conventional name of the secret that holds this provider's key. */
    val keyEnv: String,
    /** A model that takes screenshots and calls tools; empty when the user has to choose. */
    val model: String,
) {
    private val endpoint: String get() = ConfigRules.endpoint(provider, baseUrl)

    /** Same protocol and same endpoint; the device's built-in URL typed out counts as empty. */
    fun matches(s: AgentSettings): Boolean =
        provider == s.provider && endpoint == ConfigRules.endpoint(s.provider, s.baseUrl)

    /**
     * Switches provider, endpoint, key name and model together: the old model id would mean
     * nothing to another provider, and an endpoint left over from one would receive the key
     * of the next.
     *
     * Settings that already point at this provider's endpoint keep the key name and model the
     * user chose: tapping the preset that is already active changes nothing, and an endpoint
     * entered by hand with the protocol left on `anthropic` (which makes the device call a
     * path the provider answers with a 404 page) only gets its protocol put right.
     */
    fun applyTo(s: AgentSettings): AgentSettings =
        if (endpoint == ConfigRules.endpoint(s.provider, s.baseUrl)) {
            s.copy(provider = provider, baseUrl = baseUrl, model = s.model.ifBlank { model }, apiKeyEnv = s.apiKeyEnv.ifBlank { keyEnv })
        } else {
            s.copy(provider = provider, baseUrl = baseUrl, apiKeyEnv = keyEnv, model = model)
        }

    companion object {
        /**
         * Endpoints and example model ids as the providers documented them on 2026-11-24; each
         * example model takes image input and tool calls on that endpoint. They are starting
         * points: the model field stays editable, and none of this was exercised against the
         * live services from here.
         *
         * Not listed on purpose: providers that cap images per request below what the device
         * currently sends in a long task (Groq: 3, Mistral: 8). They can still be entered by hand.
         */
        val ALL = listOf(
            ProviderPreset("Anthropic", "anthropic", "", "ANTHROPIC_API_KEY", "claude-sonnet-5-5"),
            ProviderPreset("OpenRouter", "openrouter", "", "OPENROUTER_API_KEY", "anthropic/claude-haiku-5.5"),
            ProviderPreset("MiniMax", "openai", "https://api.minimax.io/v1", "MINIMAX_API_KEY", "MiniMax-M3.1-Flash"),
            ProviderPreset("OpenAI", "openai", "", "OPENAI_API_KEY", "gpt-6.2-sol"),
            ProviderPreset("Gemini", "openai", "https://generativelanguage.googleapis.com/v1beta/openai", "GEMINI_API_KEY", "gemini-4-hydrogen"),
            ProviderPreset("xAI", "openai", "https://api.x.ai/v1", "XAI_API_KEY", "grok-4.7"),
            ProviderPreset("DeepSeek", "openai", "https://api.deepseek.com", "DEEPSEEK_API_KEY", "deepseek-flash"),
            ProviderPreset("Together", "openai", "https://api.together.ai/v1", "TOGETHER_API_KEY", "moonshotai/Kimi-K3"),
            ProviderPreset("Fireworks", "openai", "https://api.fireworks.ai/inference/v1", "FIREWORKS_API_KEY", "accounts/fireworks/models/kimi-k3p1-pro"),
        )

        fun matching(s: AgentSettings): ProviderPreset? = ALL.firstOrNull { it.matches(s) }

        /**
         * Leaving a provider button's settings by choosing a protocol by hand means "that
         * protocol's own endpoint": the base URL goes, and a key name or example model that
         * belongs to another provider goes with it, so that provider's key is not sent to the
         * new host. A name or model of the user's own is kept.
         */
        fun withProtocol(s: AgentSettings, provider: String): AgentSettings {
            if (provider == s.provider) return s
            // An endpoint typed by hand for a provider without a button keeps its URL, key name
            // and model: only the protocol was wrong (the mistake seen on a real device).
            if (s.baseUrl.isNotBlank() && matching(s) == null) return s.copy(provider = provider)
            val own = ALL.first { it.provider == provider && it.baseUrl.isEmpty() }
            val foreignKey = ALL.any { it != own && it.keyEnv.equals(s.apiKeyEnv, ignoreCase = true) }
            val foreignModel = ALL.any { it != own && it.model == s.model }
            return s.copy(
                provider = provider,
                baseUrl = "",
                apiKeyEnv = if (foreignKey) own.keyEnv else s.apiKeyEnv,
                model = if (foreignModel) own.model else s.model,
            )
        }

        /**
         * A warning when the key name is the usual one of a provider these settings do not
         * talk to: saving would send that provider's key to another host.
         */
        fun strayKey(s: AgentSettings): String? {
            val owner = ALL.firstOrNull { it.keyEnv.equals(s.apiKeyEnv.trim(), ignoreCase = true) } ?: return null
            val target = ConfigRules.hostOf(ConfigRules.endpoint(s.provider, s.baseUrl))
            if (target == ConfigRules.hostOf(owner.endpoint)) return null
            return "${owner.keyEnv} is normally the ${owner.name} key, but these settings send it to $target."
        }
    }
}

object ConfigRules {
    /** Same rule as the daemon: a secret-name-like identifier where `-` is also allowed. */
    private val HOST_ID_RE = Regex("^[A-Za-z_][A-Za-z0-9_-]{0,63}$")
    private val SECRET_NAME_RE = Regex("^[A-Za-z_][A-Za-z0-9_]{0,63}$")
    private val CRON_FIELD_RE = Regex("^[0-9A-Za-z*/,-]+$")

    fun isValidHostId(id: String): Boolean = HOST_ID_RE.matches(id)

    /** A name the device stores a secret under. Names that would replace a variable of its own process are refused. */
    fun isValidSecretName(name: String): Boolean = SECRET_NAME_RE.matches(name) && !isReservedName(name)

    private val RESERVED_NAMES = setOf("PATH", "HOME", "USER", "SHELL", "LANG", "TZ", "LAPIS_CONFIG_DIR")

    private fun isReservedName(name: String): Boolean {
        val upper = name.uppercase()
        return upper in RESERVED_NAMES || listOf("LD_", "PYTHON", "DYLD_").any(upper::startsWith)
    }

    const val NOT_A_NAME = "That looks like a key or password, not a name. A name is something like " +
        "MINIMAX_API_KEY; the key itself is entered separately and never shown."

    private val KEY_PREFIXES = listOf("sk-", "gsk_", "AIza", "eyJ", "tskey-", "tskey_auth")

    /** How often a text switches between digits and other characters: `key2025` once, `a1b2c3` five times. */
    private fun alternations(text: String): Int = text.zipWithNext().count { (a, b) -> a.isDigit() != b.isDigit() }

    /**
     * A key or token pasted where its NAME belongs. A name is readable (lapis.toml, the config
     * and secrets lists, error messages), so such input is refused instead of being tidied
     * into a name.
     *
     * A heuristic. What tells a key from a name is digits scattered through it: a name has
     * words and perhaps a number at the end (`minimaxapikey2025`, `EASTUS2`), a key switches
     * between letters and digits all the time. So: a known key prefix, or one run that
     * switches three times or more, or the whole text (separators ignored, which covers keys
     * written in groups such as a UUID) switching six times or more. A short password made of
     * letters cannot be told from a name; the fields that take one are not name fields.
     */
    fun looksLikeSecretValue(text: String): Boolean {
        val t = text.trim()
        val compact = t.filter(Char::isLetterOrDigit)
        return KEY_PREFIXES.any { t.startsWith(it, ignoreCase = true) } ||
            (compact.length >= 20 && alternations(compact) >= 6) ||
            t.split('_', '-', '.', ' ').any { run -> run.length >= 12 && alternations(run) >= 3 }
    }

    /** Why `name` cannot be used as a new secret name, or null. */
    fun secretNameProblem(name: String): String? = when {
        looksLikeSecretValue(name) -> NOT_A_NAME
        !SECRET_NAME_RE.matches(name) -> "A name may only contain letters, digits and _, and cannot start with a digit."
        isReservedName(name) -> "$name is reserved on the device; choose another name."
        else -> null
    }

    /**
     * The vault name under which a host's VNC password is kept. A name the host already uses
     * is kept when a secret is stored under it (or when the vault could not be read:
     * `stored` is null). Anything else, such as a password typed into what used to be a
     * "secret name" field, is replaced by a name derived from the host id.
     */
    fun vncSecretName(hostId: String, current: String, stored: Set<String>?, taken: Set<String> = emptySet()): String =
        hostSecretName("LAPIS_VNC_PASSWORD_", hostId, current, stored, taken)

    /**
     * The same for an SSH password of the host's own (one that is not the screen's login):
     * `current` is kept only when it really is a stored secret, anything else gives way to a
     * name derived from the host id. `taken` must hold the host's VNC name too, so that this
     * password never lands on top of that one.
     */
    fun sshSecretName(hostId: String, current: String, stored: Set<String>?, taken: Set<String> = emptySet()): String =
        hostSecretName("LAPIS_SSH_PASSWORD_", hostId, current, stored, taken)

    private fun hostSecretName(prefix: String, hostId: String, current: String, stored: Set<String>?, taken: Set<String>): String {
        if (isValidSecretName(current) && (stored == null || current in stored)) return current
        // `taken`: names other hosts use. A renamed host keeps its old name, so a new host
        // with the old id must not be given (and overwrite) that host's password.
        val base = prefix + normalizeSecretName(hostId).take(40)
        return generateSequence(1) { it + 1 }.map { if (it == 1) base else "${base}_$it" }.first { it !in taken }
    }

    private val PUBLIC_KEY_RE = Regex("^[a-z0-9@.-]+ [A-Za-z0-9+/]+={0,3}( [A-Za-z0-9@._:+-]+)?$")

    /**
     * The one line that authorises the device by hand on a host that only takes keys. Null
     * when the key is not a plain `type base64 [comment]` line: it goes into a shell between
     * single quotes, so nothing that could end the quoting is let through.
     */
    fun authorizeCommand(publicKey: String): String? {
        val key = publicKey.trim()
        if (!PUBLIC_KEY_RE.matches(key)) return null
        return "mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo '$key' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys"
    }

    /**
     * The API root the device appends `/chat/completions` to. Pasted values often are the full
     * endpoint or end in a slash; both are trimmed away.
     */
    fun normalizeBaseUrl(input: String): String =
        input.trim().trimEnd('/').removeSuffix("/chat/completions").trimEnd('/')

    /** Empty (the provider's own endpoint) or a plain http(s) URL: no credentials, query or fragment. */
    fun isValidBaseUrl(url: String): Boolean =
        url.isEmpty() || BASE_URL_RE.matches(url)

    private val BASE_URL_RE = Regex(
        "^https?://(\\[[0-9a-f:.]+]|[a-z0-9._-]+)(:[0-9]{1,5})?(/[a-z0-9._~/-]*)?$",
        RegexOption.IGNORE_CASE,
    )

    /** The device's built-in endpoint for each protocol (the daemon's two LLM adapters). */
    private val BUILT_IN_URLS = mapOf(
        "anthropic" to "https://api.anthropic.com",
        "openai" to "https://api.openai.com/v1",
        "openrouter" to "https://openrouter.ai/api/v1",
    )

    /** Where requests go: the base URL as entered, or the device's built-in one for the protocol. */
    fun endpoint(provider: String, baseUrl: String): String =
        normalizeBaseUrl(baseUrl).ifEmpty { BUILT_IN_URLS[provider].orEmpty() }

    /** `host[:port]` of an endpoint, for comparing and for telling the user where a key goes. */
    fun hostOf(endpoint: String): String = endpoint.substringAfter("://").substringBefore('/').lowercase()

    /**
     * Why these settings cannot be saved, or null. `loaded` is what the device has now: a base
     * URL or key name it already accepted is not judged again, so a working setup this app
     * would not let you type can still have its other settings changed.
     */
    fun agentProblem(s: AgentSettings, loaded: AgentSettings, stored: Set<String> = emptySet()): String? = when {
        s.model.isBlank() -> "Enter the model id to use with this provider."
        s.baseUrl != loaded.baseUrl && !isValidBaseUrl(normalizeBaseUrl(s.baseUrl)) ->
            "The base URL must look like https://host/v1, without a key, ? or # in it."
        // `stored`: names the device holds a secret under. Those are names, whatever they look like.
        s.apiKeyEnv != loaded.apiKeyEnv && s.apiKeyEnv !in stored -> secretNameProblem(s.apiKeyEnv)
        else -> null
    }

    /**
     * Turns what someone types ("minimax api key", "open-router.key") into a name the device
     * accepts, instead of rejecting it: spaces, `-` and `.` become `_`, anything else that is
     * not a letter, digit or `_` is dropped, and a name cannot start with a digit.
     */
    fun normalizeSecretName(input: String): String =
        input.replace(Regex("[\\s.-]+"), "_")
            .filter { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '_' }
            .trimStart { it in '0'..'9' }
            .take(64)

    /** Shape check only (five fields); the daemon is the authority on the expression itself. */
    fun isPlausibleCron(expression: String): Boolean {
        val fields = expression.trim().split(Regex("\\s+"))
        return fields.size == 5 && fields.all { CRON_FIELD_RE.matches(it) }
    }

    val CRON_PRESETS = listOf(
        "Every hour" to "0 * * * *",
        "Daily 08:00" to "0 8 * * *",
        "Weekdays 09:00" to "0 9 * * 1-5",
        "Mondays 09:00" to "0 9 * * 1",
        "Every 15 min" to "*/15 * * * *",
    )
}

/** Hosts, agent settings, the write-only secrets vault, schedules and paired phones. */
class ConfigViewModel(
    private val api: LapisApiClient,
    private val repo: ChatRepository,
    /** The other paired devices and this phone's host book; null keeps everything on this device. */
    private val sync: HostSync? = null,
) : ViewModel() {
    private val _hosts = MutableStateFlow(Section<HostsResponse>())
    val hosts: StateFlow<Section<HostsResponse>> = _hosts.asStateFlow()

    private val _probes = MutableStateFlow<Map<String, ProbeUi>>(emptyMap())
    val probes: StateFlow<Map<String, ProbeUi>> = _probes.asStateFlow()

    private val _config = MutableStateFlow(Section<JsonObject>())
    val config: StateFlow<Section<JsonObject>> = _config.asStateFlow()

    private val _secrets = MutableStateFlow(Section<List<SecretStatus>>())
    val secrets: StateFlow<Section<List<SecretStatus>>> = _secrets.asStateFlow()

    private val _schedules = MutableStateFlow(Section<List<Schedule>>())
    val schedules: StateFlow<Section<List<Schedule>>> = _schedules.asStateFlow()

    private val _phones = MutableStateFlow(Section<List<PairedPhone>>())
    val phones: StateFlow<Section<List<PairedPhone>>> = _phones.asStateFlow()

    private val _saving = MutableStateFlow(false)
    val saving: StateFlow<Boolean> = _saving.asStateFlow()

    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    private val _lastFailure = MutableStateFlow<String?>(null)

    /** Why the last write failed. Dialogs show it themselves: a snackbar sits behind them. */
    val lastFailure: StateFlow<String?> = _lastFailure.asStateFlow()

    fun clearFailure() {
        _lastFailure.value = null
    }

    private fun reject(reason: String) {
        _lastFailure.value = reason
        _messages.tryEmit(reason)
    }

    private val _openChat = MutableSharedFlow<String>(extraBufferCapacity = 2)

    /** Chat ids to navigate to (the result of "Run now"). */
    val openChat: SharedFlow<String> = _openChat.asSharedFlow()

    private fun <T> load(target: MutableStateFlow<Section<T>>, fetch: suspend () -> T) {
        target.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            try {
                target.value = Section(data = fetch())
            } catch (e: IOException) {
                target.update { it.copy(loading = false, error = ChatRepository.describe(e)) }
            }
        }
    }

    /** Runs a write; returns through `onDone(true)` only when the daemon accepted it. */
    private fun save(failure: String, onDone: (Boolean) -> Unit = {}, block: suspend () -> Unit) {
        if (_saving.value) return
        _saving.value = true
        _lastFailure.value = null
        viewModelScope.launch {
            val ok = try {
                block()
                true
            } catch (e: IOException) {
                val reason = "$failure: ${ChatRepository.describe(e)}"
                _lastFailure.value = reason
                _messages.emit(reason)
                false
            }
            _saving.value = false
            onDone(ok)
        }
    }

    // ------------------------------------------------------------------ hosts

    fun loadHosts() = load(_hosts) { api.hosts() }

    /** This device (id and name) and the other paired devices a host can also be saved to. */
    val selfId: String get() = sync?.selfId.orEmpty()
    val selfName: String get() = sync?.selfName ?: "this device"

    fun otherDevices(): List<PairedDevice> = sync?.others().orEmpty()

    private val _offers = MutableStateFlow(Section<List<HostOffer>>())

    /** Hosts of the other paired devices that this one lacks, for "Copy hosts from another device". */
    val offers: StateFlow<Section<List<HostOffer>>> = _offers.asStateFlow()

    fun loadOffers() {
        val fleet = sync ?: return
        val have = _hosts.value.data?.hosts?.keys.orEmpty()
        load(_offers) { fleet.offers(have) }
    }

    /**
     * Copies hosts (by the device they come from and their id) onto this device, with every
     * password this phone knows for them; the result says which still need one.
     */
    fun copyHosts(picks: List<Pair<String, String>>, onDone: (Boolean) -> Unit = {}) {
        val fleet = sync
        val offers = _offers.value.data
        if (fleet == null || offers == null || picks.isEmpty()) {
            onDone(false)
            return
        }
        val entries = picks.distinctBy { it.second }.mapNotNull { (deviceId, hostId) ->
            val offer = offers.firstOrNull { it.device.id == deviceId } ?: return@mapNotNull null
            val host = offer.hosts[hostId] ?: return@mapNotNull null
            HostEntry(originalId = null, id = hostId, host = host, sourceSet = offer.storedSecrets)
        }
        save("Could not copy the hosts", onDone) {
            when (val result = fleet.push(api, entries)) {
                is HostPushOutcome.Saved -> {
                    _hosts.value = Section(data = result.hosts)
                    result.secrets?.let { _secrets.value = Section(data = it) }
                    _offers.update { s -> s.copy(data = s.data?.map { o -> o.copy(hosts = o.hosts - result.saved.toSet()) }) }
                    _messages.emit(HostSync.describeCopy(result))
                }

                HostPushOutcome.Unreachable -> throw IOException("Could not reach the device")
                is HostPushOutcome.Failed -> throw IOException(result.reason)
            }
        }
    }

    /** Whether the device software has the SSH features, and the device's own public key if so. */
    val sshSupport: StateFlow<SshSupport> = repo.sshSupport

    fun loadDeviceKey() {
        viewModelScope.launch { repo.checkSshSupport() }
    }

    /** Adds or replaces a host (`originalId` set and different from `id` means a rename). */
    /**
     * `vncPassword`, when given, is stored in the vault under `host.vnc.passwordEnv` before the
     * host is saved, so the form can take the password itself instead of the name of a secret.
     * `sshPassword` is the same for an SSH login that differs from the screen's, under
     * `host.ssh.passwordEnv`; both go to the vault in one write.
     */
    /**
     * `alsoTo`: ids of other paired devices to write the host (and the passwords typed here) to as
     * well, each through its own endpoint and token; `here` false leaves this device out.
     */
    fun saveHost(
        originalId: String?,
        id: String,
        host: Host,
        vncPassword: String? = null,
        sshPassword: String? = null,
        alsoTo: Set<String> = emptySet(),
        here: Boolean = true,
        onDone: (Boolean) -> Unit = {},
    ) {
        val current = _hosts.value.data?.hosts.orEmpty()
        val others = sync?.others().orEmpty().filter { it.id in alsoTo }
        if (!here && others.isEmpty()) {
            reject("Choose at least one device to save the host to")
            onDone(false)
            return
        }
        if (!ConfigRules.isValidHostId(id)) {
            reject("Host id may only contain letters, digits, - and _")
            onDone(false)
            return
        }
        if (here && id != originalId && id in current) {
            reject("A host named $id already exists")
            onDone(false)
            return
        }
        // What is saved as the secret's NAME is readable in the host list: it must be a name.
        if (host.vnc.passwordEnv.isNotEmpty() && !ConfigRules.isValidSecretName(host.vnc.passwordEnv)) {
            reject("The VNC password must be stored under a name made of letters, digits and _")
            onDone(false)
            return
        }
        val sshEnv = host.ssh.passwordEnv.orEmpty()
        if (sshEnv.isNotEmpty() && !ConfigRules.isValidSecretName(sshEnv)) {
            reject("The SSH password must be stored under a name made of letters, digits and _")
            onDone(false)
            return
        }
        val password = vncPassword?.trim('\n', '\r')?.takeIf { it.isNotEmpty() }
        if (password != null && (host.vnc.passwordEnv.isEmpty() || '\n' in password || '\r' in password)) {
            reject("The VNC password must be a single line")
            onDone(false)
            return
        }
        val sshPass = sshPassword?.trim('\n', '\r')?.takeIf { it.isNotEmpty() }
        // A password of its own needs a name of its own: under the name shared with VNC it
        // would replace the screen's password.
        if (sshPass != null && (sshEnv.isEmpty() || sshEnv == host.vnc.passwordEnv || '\n' in sshPass || '\r' in sshPass)) {
            reject("The SSH password must be a single line, stored under a name of its own")
            onDone(false)
            return
        }
        // Device software from before the SSH features does not know `ssh.password_env`: a host
        // goes back with what came from the device (nothing), and no SSH password is stored.
        if (here && repo.sshSupport.value !is SshSupport.Ready &&
            (sshPass != null || host.ssh.passwordEnv != current[originalId ?: id]?.ssh?.passwordEnv)
        ) {
            reject(ChatRepository.NO_SSH_FEATURES)
            onDone(false)
            return
        }
        val next = LinkedHashMap(current).apply {
            if (originalId != null && originalId != id) remove(originalId)
            put(id, host)
        }
        val passwords = buildMap {
            if (password != null) put(host.vnc.passwordEnv, password)
            if (sshPass != null) put(sshEnv, sshPass)
        }
        save("Could not save the host", onDone) {
            if (here) {
                if (passwords.isNotEmpty()) _secrets.value = Section(data = api.patchSecrets(set = passwords))
                _hosts.value = Section(data = api.putHosts(next))
            }
            sync?.book?.remember(id, passwords)
            val fleet = sync
            if (fleet == null || others.isEmpty()) {
                _messages.emit("Host $id saved")
                return@save
            }
            // Where a password is known to be needed: what is set here, plus what was just typed.
            val sourceSet = _secrets.value.data?.filter { it.isSet }?.map { it.name }?.toSet()?.plus(passwords.keys)
            val entry = HostEntry(originalId, id, host, passwords, sourceSet)
            val results = coroutineScope {
                others.map { device -> async { device.name to fleet.pushTo(device, listOf(entry)) } }.awaitAll()
            }
            val parts = listOfNotNull("saved on ${fleet.selfName}".takeIf { here }) +
                results.map { (name, outcome) -> HostSync.describe(name, id, outcome) }
            _messages.emit("Host $id: " + parts.joinToString("; "))
        }
    }

    /**
     * Adds the device's own SSH key to the host, using the login stored for it once. Success
     * is a message; a failure stays in [lastFailure] for the dialog that asked.
     */
    fun authorizeDevice(id: String, onDone: (Boolean) -> Unit = {}) = save("Could not authorise the device", onDone) {
        val result = api.authorizeSsh(id)
        if (!result.ok) throw IOException(result.detail.ifBlank { "the host did not accept the key" })
        _messages.emit(if (result.already) "The device was already authorised on $id" else "Device authorised on $id")
    }

    fun deleteHost(id: String) {
        val next = _hosts.value.data?.hosts.orEmpty() - id
        save("Could not delete the host") {
            _hosts.value = Section(data = api.putHosts(next))
            _probes.update { it - id }
        }
    }

    fun setActiveHost(id: String) {
        viewModelScope.launch {
            repo.setActiveHost(id)
                .onSuccess { active -> _hosts.update { s -> s.copy(data = s.data?.copy(activeHost = active)) } }
                .onFailure { _messages.emit(it.message ?: "Could not switch the active host") }
        }
    }

    /** Test connection: Tailscale ping, VNC handshake and SSH `echo ok`. */
    fun probe(id: String) {
        if (_probes.value[id] is ProbeUi.Running) return
        _probes.update { it + (id to ProbeUi.Running) }
        viewModelScope.launch {
            val result = try {
                ProbeUi.Done(api.probe(id))
            } catch (e: IOException) {
                ProbeUi.Failed(ChatRepository.describe(e))
            }
            _probes.update { it + (id to result) }
        }
    }

    // ------------------------------------------------------------------ agent / LLM settings

    fun loadConfig() = load(_config) { api.config() }

    fun saveSettings(settings: AgentSettings, onDone: (Boolean) -> Unit = {}) {
        val base = _config.value.data ?: run {
            onDone(false)
            return
        }
        val clean = settings.copy(apiKeyEnv = settings.apiKeyEnv.trim())
        val stored = _secrets.value.data.orEmpty().filter { it.isSet }.map { it.name }.toSet()
        ConfigRules.agentProblem(clean, AgentSettings.from(base), stored)?.let {
            reject(it)
            onDone(false)
            return
        }
        save("Could not save the settings", onDone) {
            _config.value = Section(data = api.putConfig(clean.copy(baseUrl = ConfigRules.normalizeBaseUrl(clean.baseUrl)).applyTo(base)))
            _messages.emit("Settings saved")
        }
    }

    // ------------------------------------------------------------------ secrets (write-only)

    fun loadSecrets() = load(_secrets) { api.secrets() }

    /** The value goes to the device and is dropped here; only `is_set` and the tail come back. */
    fun setSecret(name: String, value: String, onDone: (Boolean) -> Unit = {}) {
        // A name the device already lists is not judged again; a new one must look like a name.
        val listed = _secrets.value.data.orEmpty().any { it.name == name }
        (if (listed) null else ConfigRules.secretNameProblem(name))?.let {
            reject(it)
            onDone(false)
            return
        }
        // A pasted key often ends in a line break; spaces are left alone (a password may have them).
        val cleaned = value.trim('\n', '\r')
        if (cleaned.isBlank() || '\n' in cleaned || '\r' in cleaned) {
            reject("A secret must be a single non-empty line")
            onDone(false)
            return
        }
        save("Could not store the secret", onDone) {
            _secrets.value = Section(data = api.patchSecrets(set = mapOf(name to cleaned)))
            _messages.emit("$name stored on the device")
        }
    }

    fun unsetSecret(name: String) = save("Could not remove the secret") {
        _secrets.value = Section(data = api.patchSecrets(unset = listOf(name)))
        sync?.book?.forget(listOf(name))
        _messages.emit("$name removed")
    }

    // ------------------------------------------------------------------ schedules

    fun loadSchedules() = load(_schedules) { api.schedules() }

    fun saveSchedule(originalName: String?, schedule: ScheduleCreate, onDone: (Boolean) -> Unit = {}) {
        if (schedule.name.isBlank() || schedule.prompt.isBlank() || schedule.host.isBlank()) {
            _messages.tryEmit("Name, host and prompt are required")
            onDone(false)
            return
        }
        if (!ConfigRules.isPlausibleCron(schedule.schedule)) {
            _messages.tryEmit("The schedule must be a 5-field cron expression, for example 0 8 * * *")
            onDone(false)
            return
        }
        save("Could not save the schedule", onDone) {
            val list = if (originalName == null) {
                api.createSchedule(schedule.copy(name = schedule.name.trim()))
            } else {
                api.patchSchedule(
                    originalName,
                    SchedulePatch(
                        name = schedule.name.trim(),
                        schedule = schedule.schedule.trim(),
                        host = schedule.host,
                        prompt = schedule.prompt,
                        enabled = schedule.enabled,
                    ),
                )
            }
            _schedules.value = Section(data = list)
        }
    }

    fun setScheduleEnabled(name: String, enabled: Boolean) = save("Could not update the schedule") {
        _schedules.value = Section(data = api.patchSchedule(name, SchedulePatch(enabled = enabled)))
    }

    fun deleteSchedule(name: String) = save("Could not delete the schedule") {
        api.deleteSchedule(name)
        _schedules.update { s -> s.copy(data = s.data?.filterNot { it.name == name }) }
    }

    fun runSchedule(name: String) = save("Could not start the job") {
        val run = api.runSchedule(name)
        _messages.emit("$name started")
        repo.refreshChats()
        if (run.chatId.isNotEmpty()) _openChat.emit(run.chatId)
    }

    // ------------------------------------------------------------------ paired phones

    fun loadPhones() = load(_phones) { api.devices() }

    fun revokePhone(id: String) = save("Could not revoke the phone") {
        api.revokeDevice(id)
        _phones.value = Section(data = api.devices())
        _messages.emit("Access revoked")
    }
}
