package dev.lapiszero.app.domain

import dev.lapiszero.app.data.local.SecureStore
import dev.lapiszero.app.data.remote.Host
import dev.lapiszero.app.data.remote.HostsResponse
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.data.remote.SecretStatus
import dev.lapiszero.app.ui.config.ConfigRules
import java.io.IOException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.serialization.Serializable

/**
 * The KVM-host passwords typed into this app, by host id and by the NAME of the device secret
 * each was stored under. Device secrets are write-only, so this is the only way to put a host's
 * password on another paired device without typing it again.
 *
 * Its own Keystore-sealed entry, apart from the devices vault. Nothing here logs, and
 * [toString] shows no password.
 */
class HostBook(private val store: SecureStore) {
    @Serializable
    private data class Book(val hosts: Map<String, Map<String, String>> = emptyMap())

    private val lock = Any()
    private var cache: Book? = null

    private fun book(): Book = cache ?: (
        store.read()?.let { runCatching { LapisJson.decodeFromString<Book>(it) }.getOrNull() } ?: Book()
        ).also { cache = it }

    private fun write(next: Book) {
        try {
            store.write(LapisJson.encodeToString(next))
            cache = next
        } catch (e: IOException) {
            // Best effort: without the Keystore this phone just cannot copy that password later.
        }
    }

    /** `passwords` by secret name, as they were just stored on a device for `hostId`. */
    fun remember(hostId: String, passwords: Map<String, String>) {
        if (passwords.isEmpty()) return
        synchronized(lock) {
            val current = book()
            val next = Book(current.hosts + (hostId to current.hosts[hostId].orEmpty() + passwords))
            if (next != current) write(next)
        }
    }

    /** The passwords this phone knows for `hostId` under any of `names`. */
    fun known(hostId: String, names: Collection<String>): Map<String, String> = synchronized(lock) {
        book().hosts[hostId].orEmpty().filterKeys { it in names }
    }

    /** A secret removed from a device in this app is not kept here either. */
    fun forget(names: Collection<String>) {
        synchronized(lock) {
            val current = book()
            val next = Book(
                current.hosts.mapValues { (_, byName) -> byName - names.toSet() }.filterValues { it.isNotEmpty() },
            )
            if (next != current) write(next)
        }
    }

    override fun toString(): String = "HostBook(redacted)"
}

/** One host to write to a device. */
data class HostEntry(
    /** The id the host has on the device it came from when it is being edited or renamed; null for a new one. */
    val originalId: String?,
    val id: String,
    val host: Host,
    /** Passwords typed just now, by the secret name they were typed for. */
    val typed: Map<String, String> = emptyMap(),
    /**
     * Secret names that hold a password where the host comes from; null when that is unknown
     * (then every name the host uses is taken to need one).
     */
    val sourceSet: Set<String>? = null,
) {
    override fun toString(): String = "HostEntry(id=$id, typed=${typed.keys})"
}

sealed interface HostPushOutcome {
    data class Saved(
        val saved: List<String>,
        /** New hosts whose id this device already uses for another host: left alone. */
        val skipped: List<String> = emptyList(),
        /** Saved, but this phone never saw their password and the device does not have it yet. */
        val needPassword: List<String> = emptyList(),
        /** The device software is from before the SSH features: no SSH password went there. */
        val withoutSsh: Boolean = false,
        val hosts: HostsResponse = HostsResponse(),
        /** The vault after the passwords were set; null when none were. */
        val secrets: List<SecretStatus>? = null,
    ) : HostPushOutcome

    data object Unreachable : HostPushOutcome

    data class Failed(val reason: String) : HostPushOutcome
}

/** Hosts another paired device has that this one lacks. */
data class HostOffer(
    val device: PairedDevice,
    val hosts: Map<String, Host> = emptyMap(),
    /** Names of the secrets set on that device; null when they could not be read. */
    val storedSecrets: Set<String>? = null,
    val error: String? = null,
)

/**
 * KVM hosts across paired devices: each device keeps its own `hosts.toml` and vault, reached
 * through its own endpoint and token. Writes respect every target's capabilities the way
 * `ConfigViewModel.saveHost` does for the current one.
 */
class HostSync(
    val selfId: String,
    private val devices: DeviceRepository,
    val book: HostBook,
    private val apiFor: (PairedDevice) -> LapisApiClient = { d -> LapisApiClient(d.endpoint, { d.token }) },
) {
    val selfName: String get() = devices.find(selfId)?.name ?: "this device"

    /** The other usable paired devices, CardputerZero or uConsole alike. */
    fun others(): List<PairedDevice> = devices.state.value.usable.filter { it.id != selfId }

    suspend fun pushTo(device: PairedDevice, entries: List<HostEntry>): HostPushOutcome = push(apiFor(device), entries)

    /**
     * Adds or replaces `entries` on the device behind `api`, in one vault write and one host-map
     * write. A secret name another host there already uses is replaced by a derived one, so no
     * password lands on top of another host's.
     */
    suspend fun push(api: LapisApiClient, entries: List<HostEntry>): HostPushOutcome = try {
        val sshFeatures = api.deviceSshKey() != null
        val loaded = api.hosts()
        val current = loaded.hosts
        val (clashing, fine) = entries.partition { it.originalId == null && it.id in current }
        if (fine.isEmpty()) {
            HostPushOutcome.Saved(saved = emptyList(), skipped = clashing.map { it.id }, hosts = loaded)
        } else {
            val replaced = fine.flatMap { listOfNotNull(it.originalId, it.id) }.toSet()
            val taken = current.filterKeys { it !in replaced }.values
                .flatMap { listOfNotNull(it.vnc.passwordEnv, it.ssh.passwordEnv) }
                .filter { it.isNotEmpty() }.toHashSet()
            val next = LinkedHashMap(current)
            val passwords = LinkedHashMap<String, String>()
            val needPassword = ArrayList<String>()
            var withoutSsh = false
            for (entry in fine) {
                val before = current[entry.originalId ?: entry.id]
                var host = entry.host
                if (!sshFeatures) {
                    // Older software does not know `ssh.password_env`: it goes back as it came.
                    if (!host.ssh.passwordEnv.isNullOrEmpty() && host.ssh.passwordEnv != before?.ssh?.passwordEnv) withoutSsh = true
                    host = host.copy(ssh = host.ssh.copy(passwordEnv = before?.ssh?.passwordEnv))
                }
                val vncName = host.vnc.passwordEnv
                val sshName = host.ssh.passwordEnv.orEmpty().takeIf { sshFeatures }.orEmpty()
                val names = listOf(vncName, sshName).filter { it.isNotEmpty() }.distinct()
                val rename = HashMap<String, String>()
                if (vncName.isNotEmpty() && vncName in taken) {
                    rename[vncName] = ConfigRules.vncSecretName(entry.id, "", emptySet(), taken)
                }
                if (sshName.isNotEmpty() && sshName != vncName && sshName in taken) {
                    rename[sshName] = ConfigRules.sshSecretName(entry.id, "", emptySet(), taken + rename.values)
                }
                host = host.copy(
                    vnc = host.vnc.copy(passwordEnv = rename[vncName] ?: vncName),
                    ssh = host.ssh.copy(passwordEnv = host.ssh.passwordEnv?.let { rename[it] ?: it }),
                )
                val known = book.known(entry.id, names) + entry.typed.filterKeys { it in names }
                val delivered = HashMap<String, String>()
                for (name in names) {
                    val target = rename[name] ?: name
                    val value = known[name]
                    val alreadyHere = before != null && (before.vnc.passwordEnv == target || before.ssh.passwordEnv == target)
                    when {
                        value != null -> delivered[target] = value
                        (entry.sourceSet?.contains(name) ?: true) && !alreadyHere -> if (entry.id !in needPassword) needPassword += entry.id
                    }
                }
                passwords += delivered
                // Known under the new name too, for the next copy from this device.
                if (rename.isNotEmpty()) book.remember(entry.id, delivered)
                taken += listOfNotNull(host.vnc.passwordEnv, host.ssh.passwordEnv).filter { it.isNotEmpty() }
                if (entry.originalId != null && entry.originalId != entry.id) next.remove(entry.originalId)
                next[entry.id] = host
            }
            // The vault first: a failure there never leaves a host pointing at a missing secret.
            val secrets = if (passwords.isNotEmpty()) api.patchSecrets(set = passwords) else null
            HostPushOutcome.Saved(
                saved = fine.map { it.id },
                skipped = clashing.map { it.id },
                needPassword = needPassword,
                withoutSsh = withoutSsh,
                hosts = api.putHosts(next),
                secrets = secrets,
            )
        }
    } catch (e: LapisApiException) {
        HostPushOutcome.Failed(if (e.isUnauthorized) "this phone is no longer paired with it" else ChatRepository.describe(e))
    } catch (e: IOException) {
        HostPushOutcome.Unreachable
    }

    /** What each other device has that this one (`have`, by host id) lacks. */
    suspend fun offers(have: Set<String>): List<HostOffer> = coroutineScope {
        others().map { device ->
            async {
                val api = apiFor(device)
                try {
                    val hosts = api.hosts().hosts.filterKeys { it !in have }
                    val stored = try {
                        api.secrets().filter { it.isSet }.map { it.name }.toSet()
                    } catch (e: IOException) {
                        null
                    }
                    HostOffer(device, hosts, stored)
                } catch (e: LapisApiException) {
                    HostOffer(device, error = ChatRepository.describe(e))
                } catch (e: IOException) {
                    HostOffer(device, error = "unreachable")
                }
            }
        }.awaitAll()
    }

    companion object {
        /** One device's part of "Host x: saved on A; B unreachable". */
        fun describe(deviceName: String, hostId: String, outcome: HostPushOutcome): String = when (outcome) {
            is HostPushOutcome.Saved -> when {
                hostId in outcome.skipped -> "$deviceName already has a host named $hostId"
                hostId in outcome.needPassword -> "saved on $deviceName, set its password there"
                outcome.withoutSsh -> "saved on $deviceName without the SSH password (older device software)"
                else -> "saved on $deviceName"
            }

            HostPushOutcome.Unreachable -> "$deviceName unreachable"
            is HostPushOutcome.Failed -> "$deviceName: ${outcome.reason}"
        }

        /** The result of copying hosts onto this device. */
        fun describeCopy(saved: HostPushOutcome.Saved): String = buildList {
            if (saved.saved.isNotEmpty()) add("Copied ${saved.saved.joinToString()}")
            if (saved.needPassword.isNotEmpty()) {
                add("set the password of ${saved.needPassword.joinToString()} here: this phone never saw it")
            }
            if (saved.withoutSsh) add("SSH passwords were left out (older device software)")
            if (saved.skipped.isNotEmpty()) add("${saved.skipped.joinToString()} already here, left alone")
        }.joinToString("; ").ifEmpty { "Nothing to copy" }
    }
}
