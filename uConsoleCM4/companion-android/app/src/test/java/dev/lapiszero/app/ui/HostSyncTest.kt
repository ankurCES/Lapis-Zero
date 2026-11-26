package dev.lapiszero.app.ui

import dev.lapiszero.app.FakeEventSource
import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.MainDispatcherRule
import dev.lapiszero.app.MemorySecureStore
import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.await
import dev.lapiszero.app.awaitTrue
import dev.lapiszero.app.data.local.SecureStore
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.Host
import dev.lapiszero.app.data.remote.HostSsh
import dev.lapiszero.app.data.remote.HostVnc
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.DeviceRepository
import dev.lapiszero.app.domain.HostBook
import dev.lapiszero.app.domain.HostEntry
import dev.lapiszero.app.domain.HostSync
import dev.lapiszero.app.domain.PairedDevice
import dev.lapiszero.app.domain.SshSupport
import dev.lapiszero.app.endpoint
import dev.lapiszero.app.jsonResponse
import dev.lapiszero.app.ui.config.ConfigViewModel
import java.io.IOException
import java.net.ServerSocket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test

/** KVM hosts on any paired device: multi-device save, copy between devices, the phone's host book. */
class HostSyncTest {
    @get:Rule
    val mainRule = MainDispatcherRule()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** A daemon that keeps the hosts and secret names it is sent (never the values in its answers). */
    private class Daemon(sshFeatures: Boolean = true) {
        val server = MockWebServer()
        val router = Router()
        val hosts = ConcurrentHashMap<String, JsonObject>()
        val secrets = ConcurrentHashMap<String, String>()

        init {
            server.dispatcher = router
            server.start()
            if (sshFeatures) router.json("GET", "/v1/device/ssh-key", Fixtures.SSH_KEY)
            router.on("GET", "/v1/hosts") { hostsBody() }
            router.on("PUT", "/v1/hosts") { request ->
                val sent = LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject.getValue("hosts").jsonObject
                hosts.clear()
                sent.forEach { (id, h) -> hosts[id] = h.jsonObject }
                hostsBody()
            }
            router.on("PATCH", "/v1/secrets") { request ->
                val set = LapisJson.parseToJsonElement(request.body!!.utf8()).jsonObject["set"]?.jsonObject.orEmpty()
                set.forEach { (k, v) -> secrets[k] = v.jsonPrimitive.content }
                secretsBody()
            }
            router.on("GET", "/v1/secrets/status") { secretsBody() }
        }

        private fun hostsBody() = jsonResponse("""{"hosts": ${JsonObject(hosts.toMap())}, "active_host": ""}""")

        private fun secretsBody() = jsonResponse(
            """{"secrets": [${secrets.keys.sorted().joinToString { """{"name": "$it", "is_set": true, "masked": "...${it.takeLast(2)}"}""" }}]}""",
        )

        fun host(id: String): JsonObject = hosts.getValue(id)

        fun put(id: String, host: Host) {
            hosts[id] = LapisJson.encodeToJsonElement(Host.serializer(), host).jsonObject
        }
    }

    private val here = Daemon()
    private val deck = Daemon()
    private val old = Daemon(sshFeatures = false)
    private val book = HostBook(MemorySecureStore())
    private val devices = DeviceRepository(MemorySecureStore())
    private lateinit var self: PairedDevice
    private lateinit var deckDevice: PairedDevice
    private lateinit var oldDevice: PairedDevice
    private lateinit var gone: PairedDevice
    private lateinit var repo: ChatRepository

    private val vncName = "LAPIS_VNC_PASSWORD_mac_mini"
    private val sshName = "LAPIS_SSH_PASSWORD_mac_mini"
    private val vncPass = "screen pass 1"
    private val sshPass = "shell pass 2"
    private val macMini = Host("mac-mini", "100.64.0.9", "macos", "strict", HostVnc(5900, "alice", vncName), HostSsh(22, "deploy", null, sshName))

    @Before
    fun setUp() {
        self = devices.add("Lapis-Deck", here.server.endpoint(), "token-here", "dev_h", kind = DeviceKind.DECK)
        deckDevice = devices.add("LapisZero-4F2A", deck.server.endpoint(), "token-deck", "dev_d", kind = DeviceKind.ZERO)
        oldDevice = devices.add("LapisZero-0LD1", old.server.endpoint(), "token-old", "dev_o", kind = DeviceKind.ZERO)
        val closedPort = ServerSocket(0).use { it.localPort }
        gone = devices.add("LapisZero-9C1B", Endpoint("127.0.0.1:$closedPort", tls = false), "token-gone", "dev_g")
        repo = ChatRepository(here.server.api("token-here"), FakeEventSource(), scope)
    }

    @After
    fun tearDown() {
        scope.cancel()
        listOf(here, deck, old).forEach { it.server.close() }
    }

    private fun sync() = HostSync(self.id, devices, book)

    private suspend fun vm(): ConfigViewModel {
        val vm = ConfigViewModel(here.server.api("token-here"), repo, sync())
        vm.loadHosts()
        vm.loadSecrets()
        vm.loadDeviceKey()
        vm.hosts.await { it.data != null }
        vm.secrets.await { it.data != null }
        vm.sshSupport.await { it is SshSupport.Ready }
        return vm
    }

    private fun collect(vm: ConfigViewModel): CopyOnWriteArrayList<String> {
        val out = CopyOnWriteArrayList<String>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { vm.messages.collect { out += it } }
        return out
    }

    private fun Daemon.bodies(): String = router.bodies.joinToString("\n") { (key, body) -> "$key $body" }

    @Test
    fun `a host is saved to every chosen device through its own token and the result names each`() = runBlocking<Unit> {
        // The other deck already has a host whose VNC password is stored under the same name.
        deck.put("arch-dev", macMini.copy(hostname = "arch-dev", vnc = HostVnc(5900, "bob", vncName), ssh = HostSsh(22, "bob", null, "")))
        deck.secrets[vncName] = "bob's own"
        val vm = vm()
        val messages = collect(vm)
        assertEquals(listOf("LapisZero-4F2A", "LapisZero-0LD1", "LapisZero-9C1B"), vm.otherDevices().map { it.name })

        var saved: Boolean? = null
        vm.saveHost(null, "mac-mini", macMini, vncPass, sshPass, alsoTo = setOf(deckDevice.id, oldDevice.id, gone.id)) { saved = it }
        awaitTrue(15_000) { saved != null }
        assertEquals(true, saved)
        val message = messages.single { it.startsWith("Host mac-mini") }
        assertEquals(
            "Host mac-mini: saved on Lapis-Deck; saved on LapisZero-4F2A; " +
                "saved on LapisZero-0LD1 without the SSH password (older device software); LapisZero-9C1B unreachable",
            message,
        )

        // This device: both passwords, under the names the form chose.
        assertEquals(mapOf(vncName to vncPass, sshName to sshPass), here.secrets.toMap())
        // Each device was asked with its own token, never another's.
        assertTrue(here.router.requests.all { it.headers["Authorization"] == "Bearer token-here" })
        assertTrue(deck.router.requests.all { it.headers["Authorization"] == "Bearer token-deck" })
        assertTrue(old.router.requests.all { it.headers["Authorization"] == "Bearer token-old" })

        // The other deck: bob's password is left alone, ours goes under a name of its own.
        assertEquals("bob's own", deck.secrets[vncName])
        val renamed = "${vncName}_2"
        assertEquals(vncPass, deck.secrets[renamed])
        assertEquals(sshPass, deck.secrets[sshName])
        val onDeck = deck.host("mac-mini")
        assertEquals(renamed, onDeck.getValue("vnc").jsonObject.getValue("password_env").jsonPrimitive.content)
        assertEquals(sshName, onDeck.getValue("ssh").jsonObject.getValue("password_env").jsonPrimitive.content)
        assertEquals(vncName, deck.host("arch-dev").getValue("vnc").jsonObject.getValue("password_env").jsonPrimitive.content)

        // Older software: the VNC password, no SSH password and no `password_env` it does not know.
        assertEquals(mapOf(vncName to vncPass), old.secrets.toMap())
        assertNull(old.host("mac-mini").getValue("ssh").jsonObject["password_env"])

        // Passwords only ever in vault writes, and never in a message.
        for (daemon in listOf(here, deck, old)) {
            val leaks = daemon.router.bodies.filter { (_, body) -> body.contains(vncPass) || body.contains(sshPass) }.map { it.first }
            assertEquals(setOf("PATCH /v1/secrets"), leaks.toSet())
        }
        assertTrue(messages.none { it.contains(vncPass) || it.contains(sshPass) })
        // The phone remembers what was typed, for copies later.
        assertEquals(mapOf(vncName to vncPass, sshName to sshPass), book.known("mac-mini", listOf(vncName, sshName)))
        assertFalse(book.toString().contains(vncPass))
    }

    @Test
    fun `saving only elsewhere, an id the other device already uses, and a password it never got`() = runBlocking<Unit> {
        deck.put("mac-mini", macMini.copy(hostname = "someone else's"))
        val vm = vm()
        val messages = collect(vm)
        var saved: Boolean? = null
        vm.saveHost(null, "mac-mini", macMini, vncPass, sshPass, alsoTo = setOf(deckDevice.id), here = false) { saved = it }
        awaitTrue { saved != null }
        assertEquals("Host mac-mini: LapisZero-4F2A already has a host named mac-mini", messages.last())
        assertEquals("someone else's", deck.host("mac-mini").getValue("hostname").jsonPrimitive.content)
        assertTrue("left this device out", here.hosts.isEmpty())

        // A host whose password is stored here but was never typed into this phone.
        here.put("nas", macMini.copy(hostname = "nas", vnc = HostVnc(5900, "", "LAPIS_VNC_PASSWORD_nas"), ssh = HostSsh(22, "admin", null, "")))
        here.secrets["LAPIS_VNC_PASSWORD_nas"] = "typed on the device itself"
        vm.loadHosts()
        vm.loadSecrets()
        vm.secrets.await { s -> s.data?.any { it.name == "LAPIS_VNC_PASSWORD_nas" } == true }
        vm.saving.await { !it }
        vm.saveHost("nas", "nas", here.hosts.getValue("nas").let { LapisJson.decodeFromJsonElement(Host.serializer(), it) }, alsoTo = setOf(deckDevice.id))
        awaitTrue { messages.any { it.startsWith("Host nas") } }
        val text = messages.last { it.startsWith("Host nas") }
        assertEquals("Host nas: saved on Lapis-Deck; saved on LapisZero-4F2A, set its password there", text)
        assertFalse(deck.secrets.containsKey("LAPIS_VNC_PASSWORD_nas"))
    }

    @Test
    fun `copying hosts from another device brings the passwords this phone knows and flags the rest`() = runBlocking<Unit> {
        // Typed into this phone once, when mac-mini was saved on the other deck.
        book.remember("mac-mini", mapOf(vncName to vncPass, sshName to sshPass))
        deck.put("mac-mini", macMini)
        deck.secrets[vncName] = vncPass
        deck.secrets[sshName] = sshPass
        deck.put("nas", macMini.copy(hostname = "nas", vnc = HostVnc(5900, "", "LAPIS_VNC_PASSWORD_nas"), ssh = HostSsh(22, "admin", null, "")))
        deck.secrets["LAPIS_VNC_PASSWORD_nas"] = "never typed here"
        deck.put("printer", macMini.copy(hostname = "printer", vnc = HostVnc(5900, "", "LAPIS_VNC_PASSWORD_printer"), ssh = HostSsh(22, "pi", null, "")))
        val shared = macMini.copy(vnc = HostVnc(5900, "carol", "LAPIS_VNC_PASSWORD_shared"), ssh = HostSsh(22, "carol", null, ""))
        deck.put("shared", shared.copy(hostname = "shared"))
        here.put("shared", shared.copy(hostname = "shared here"))
        old.put("legacy", macMini.copy(hostname = "legacy", vnc = HostVnc(5900, "", "LAPIS_VNC_PASSWORD_legacy"), ssh = HostSsh(22, "root", null, null)))
        val vm = vm()
        val messages = collect(vm)

        vm.loadOffers()
        val offers = vm.offers.await { it.data != null }.data!!
        val byName = offers.associateBy { it.device.name }
        assertEquals(setOf("mac-mini", "nas", "printer"), byName.getValue("LapisZero-4F2A").hosts.keys)
        assertEquals(setOf("legacy"), byName.getValue("LapisZero-0LD1").hosts.keys)
        assertEquals("unreachable", byName.getValue("LapisZero-9C1B").error)

        var done: Boolean? = null
        vm.copyHosts(listOf(deckDevice.id to "mac-mini", deckDevice.id to "nas", deckDevice.id to "printer", oldDevice.id to "legacy")) { done = it }
        awaitTrue { done != null }
        assertEquals(true, done)
        // printer has no password on its device, so nothing is missing for it.
        assertEquals("Copied mac-mini, nas, printer, legacy; set the password of nas here: this phone never saw it", messages.last())
        assertEquals(setOf("shared", "mac-mini", "nas", "printer", "legacy"), here.hosts.keys)
        assertEquals("shared here", here.host("shared").getValue("hostname").jsonPrimitive.content)
        assertEquals(mapOf(vncName to vncPass, sshName to sshPass), here.secrets.toMap())
        assertEquals("one vault write for the whole copy", 1, here.router.count("PATCH /v1/secrets"))
        assertTrue("the copied hosts are shown", vm.hosts.value.data!!.hosts.keys.containsAll(listOf("mac-mini", "nas")))
        assertTrue("and no longer offered", vm.offers.value.data!!.all { it.hosts.isEmpty() })
        assertTrue(messages.none { it.contains(vncPass) || it.contains(sshPass) || it.contains("never typed here") })
        assertFalse("the other devices were only read", deck.router.log.any { it.startsWith("PUT") || it.startsWith("PATCH") })
    }

    @Test
    fun `the host book is sealed apart and survives a keystore that refuses`() {
        val store = MemorySecureStore()
        HostBook(store).remember("mac-mini", mapOf(vncName to vncPass))
        assertTrue(store.writes == 1)
        val reopened = HostBook(store)
        assertEquals(mapOf(vncName to vncPass), reopened.known("mac-mini", listOf(vncName, sshName)))
        assertEquals(emptyMap<String, String>(), reopened.known("nas", listOf(vncName)))
        reopened.forget(listOf(vncName))
        assertEquals(emptyMap<String, String>(), HostBook(store).known("mac-mini", listOf(vncName)))

        val refusing = object : SecureStore {
            override fun read(): String? = null
            override fun write(value: String) {
                throw IOException("Secure storage is unavailable on this phone")
            }
            override fun clear() = Unit
        }
        HostBook(refusing).remember("mac-mini", mapOf(vncName to vncPass)) // no crash, nothing kept
        assertEquals("HostEntry(id=mac-mini, typed=[$vncName])", HostEntry(null, "mac-mini", macMini, mapOf(vncName to vncPass)).toString())
    }
}
