package dev.lapiszero.app.ui

import dev.lapiszero.app.MainDispatcherRule
import dev.lapiszero.app.MemorySecureStore
import dev.lapiszero.app.Router
import dev.lapiszero.app.await
import dev.lapiszero.app.awaitTrue
import dev.lapiszero.app.data.ble.BleAccess
import dev.lapiszero.app.data.ble.BleConnection
import dev.lapiszero.app.data.ble.BleTransport
import dev.lapiszero.app.data.ble.FakeLapisDevice
import dev.lapiszero.app.data.ble.FoundDevice
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.domain.DeviceRepository
import dev.lapiszero.app.domain.PairedDevice
import dev.lapiszero.app.endpoint
import dev.lapiszero.app.errorResponse
import dev.lapiszero.app.ui.device.WifiStep
import dev.lapiszero.app.ui.device.WifiVia
import dev.lapiszero.app.ui.device.WifiViewModel
import java.net.ServerSocket
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.SocketEffect
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test

/** "Change Wi-Fi" for a paired device: the API path, the Bluetooth paired-phone path, and every state in between. */
class WifiViewModelTest {
    @get:Rule
    val mainRule = MainDispatcherRule()

    private val server = MockWebServer()
    private val router = Router()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val token = "deck-token-paired-with-this-phone-0123456789"
    private val psk = "correct horse 42"
    private lateinit var devices: DeviceRepository
    private lateinit var paired: PairedDevice
    private val fake = FakeLapisDevice()

    /** Bluetooth stand-in: one advertisement per device, links into [FakeLapisDevice]. */
    private inner class FakeBle(val found: List<FoundDevice>) : BleAccess {
        override val isSupported = true
        override val isEnabled = true
        val links = ArrayList<FakeLapisDevice.Connection>()

        override fun scan(): Flow<List<FoundDevice>> = flowOf(found)

        override suspend fun connect(device: FoundDevice): BleConnection {
            val link = fake.connect()
            links += link
            return object : BleConnection, BleTransport by link {
                override suspend fun close() = link.drop()
            }
        }
    }

    private val deck = FoundDevice("LapisDeck-4F2A", -50, "AA:BB:CC:DD:4F:2A")
    private val other = FoundDevice("LapisZero-77C1", -40, "AA:BB:CC:DD:77:C1")

    @Before
    fun setUp() {
        server.dispatcher = router
        server.start()
        devices = DeviceRepository(MemorySecureStore())
        paired = devices.add(
            "Lapis-Deck", server.endpoint(), token, "dev_01", kind = DeviceKind.DECK, bleName = "LapisDeck-4F2A",
        )
        router.json(
            "GET", "/v1/wifi",
            """{"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi": -58,
                "saved": [{"name": "HomeWiFi", "active": true}, {"name": "Office", "active": false}],
                "ble": {"open": false, "expires_at": 0}}""",
        )
        router.json(
            "GET", "/v1/wifi/scan",
            """{"networks": [{"ssid": "Office", "rssi": -61, "secured": true}, {"ssid": "café guest", "rssi": -71, "secured": false}]}""",
        )
    }

    @After
    fun tearDown() {
        scope.cancel()
        server.close()
    }

    private fun vm(api: LapisApiClient = LapisApiClient(paired.endpoint, { token }), ble: BleAccess = FakeBle(listOf(other, deck))) =
        WifiViewModel(paired, api, ble, devices, scope, tokenTimeoutMs = 400)

    private fun states(vm: WifiViewModel): CopyOnWriteArrayList<WifiStep> {
        val out = CopyOnWriteArrayList<WifiStep>()
        scope.launch(start = CoroutineStart.UNDISPATCHED) { vm.step.collect { out += it } }
        return out
    }

    private fun assertNoPassword(states: List<WifiStep>) {
        states.forEach { assertFalse("the Wi-Fi password is in a screen state: $it", it.toString().contains(psk)) }
    }

    @Test
    fun `a reachable device shows its wifi and nearby networks and joins one`() = runBlocking<Unit> {
        router.on("POST", "/v1/wifi/connect") {
            // Joining takes a while on a real device.
            MockResponse.Builder().setHeader("Content-Type", "application/json")
                .body("""{"state": "connected", "local_ip": "10.0.0.7"}""").headersDelay(300, TimeUnit.MILLISECONDS).build()
        }
        val vm = vm()
        val seen = states(vm)
        val ready = vm.step.await { it is WifiStep.Networks && !it.scanning && it.networks.isNotEmpty() } as WifiStep.Networks
        assertEquals(WifiVia.API, ready.via)
        assertEquals("HomeWiFi", ready.info!!.ssid)
        assertEquals(listOf("Office", "café guest"), ready.networks.map { it.ssid })

        vm.connect("Office", psk)
        assertEquals(WifiStep.Joined("Office", "10.0.0.7", WifiVia.API), vm.step.await { it is WifiStep.Joined })
        assertTrue(seen.any { it is WifiStep.Networks && it.connectingTo == "Office" })
        assertNoPassword(seen)
        assertTrue(router.bodies.any { (key, body) -> key == "POST /v1/wifi/connect" && body.contains(psk) })
    }

    @Test
    fun `a lost answer while joining means the device is switching networks`() = runBlocking<Unit> {
        router.on("POST", "/v1/wifi/connect") {
            MockResponse.Builder().onResponseStart(SocketEffect.CloseSocket()).build()
        }
        val vm = vm()
        vm.step.await { it is WifiStep.Networks && !it.scanning }
        vm.connect("Office", psk)
        assertEquals(WifiStep.Switching("Office"), vm.step.await { it !is WifiStep.Networks })
        assertEquals("sent once, never retried", 1, router.count("POST /v1/wifi/connect"))
    }

    @Test
    fun `a refused or wrong password stays on the list and says why`() = runBlocking<Unit> {
        router.json("POST", "/v1/wifi/connect", """{"state": "failed", "local_ip": ""}""")
        val vm = vm()
        val seen = states(vm)
        vm.step.await { it is WifiStep.Networks && !it.scanning }
        vm.connect("Office", psk)
        val failed = vm.step.await { it is WifiStep.Networks && it.error != null } as WifiStep.Networks
        assertEquals("Could not join Office. Check the password.", failed.error)
        assertEquals(null, failed.connectingTo)

        router.on("POST", "/v1/wifi/connect") { errorResponse(422, "invalid", "psk must be 8..63 printable ASCII or 64 hex") }
        vm.connect("Office", psk)
        val invalid = vm.step.await { it is WifiStep.Networks && it.error?.contains("printable") == true } as WifiStep.Networks
        assertTrue(invalid.error!!.startsWith("Could not join Office"))

        // A password the daemons would refuse never leaves the phone.
        val before = router.count("POST /v1/wifi/connect")
        vm.connect("Office", "short")
        assertEquals(WifiViewModel.PSK_RULE, (vm.step.value as WifiStep.Networks).error)
        assertEquals(before, router.count("POST /v1/wifi/connect"))
        assertNoPassword(seen)
    }

    @Test
    fun `forgetting a saved network updates the list`() = runBlocking<Unit> {
        router.json("POST", "/v1/wifi/forget", """{"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi": -58, "saved": [{"name": "HomeWiFi", "active": true}], "ble": {"open": false, "expires_at": 0}}""")
        val vm = vm()
        vm.step.await { it is WifiStep.Networks && !it.scanning }
        vm.forget("Office")
        val after = vm.step.await { it is WifiStep.Networks && it.forgetting == null && it.info!!.saved.size == 1 } as WifiStep.Networks
        assertEquals(listOf("HomeWiFi"), after.info!!.saved.map { it.name })
        assertEquals("the scan results stay", 2, after.networks.size)
    }

    @Test
    fun `an unreachable device or older software offers bluetooth`() = runBlocking<Unit> {
        val closedPort = ServerSocket(0).use { it.localPort }
        val unreachable = vm(api = LapisApiClient(Endpoint("127.0.0.1:$closedPort", tls = false), { token }))
        val offline = unreachable.step.await { it is WifiStep.Offline } as WifiStep.Offline
        assertEquals("Could not reach the device", offline.reason)

        router.on("GET", "/v1/wifi") { errorResponse(404, "not_found", "no route") }
        val older = vm().step.await { it is WifiStep.Offline } as WifiStep.Offline
        assertEquals("Its software cannot change Wi-Fi over the network yet.", older.reason)
    }

    @Test
    fun `over bluetooth the phone proves itself with this device's token and joins`() = runBlocking<Unit> {
        router.on("GET", "/v1/wifi") { errorResponse(404, "not_found", "no route") }
        fake.pairedTokens += token
        fake.pinOnScreen = false
        val ble = FakeBle(listOf(other, deck))
        val vm = vm(ble = ble)
        val seen = states(vm)
        vm.step.await { it is WifiStep.Offline }

        vm.useBluetooth()
        assertEquals("LapisDeck-4F2A", (vm.step.value as WifiStep.BleSearch).remembered)
        vm.startBleScan()
        val search = vm.step.await { it is WifiStep.BleSearch && it.devices.isNotEmpty() } as WifiStep.BleSearch
        assertEquals("the remembered device comes first", listOf("LapisDeck-4F2A", "LapisZero-77C1"), search.devices.map { it.name })

        vm.select(search.devices.first())
        val networks = vm.step.await { it is WifiStep.Networks && !it.scanning } as WifiStep.Networks
        assertEquals(WifiVia.BLE, networks.via)
        assertEquals("LapisDeck-4F2A", networks.bleName)
        assertEquals(listOf("HomeWiFi", "café guest"), networks.networks.map { it.ssid })

        fake.wifiPassword = psk
        vm.connect("HomeWiFi", psk)
        assertEquals(WifiStep.Joined("HomeWiFi", "192.168.1.42", WifiVia.BLE), vm.step.await { it is WifiStep.Joined })
        awaitTrue { fake.finished }
        assertEquals(
            listOf("token_verify", "wifi_scan", "wifi_connect", "finish_provisioning"),
            fake.commands.map { it.getValue("cmd").jsonPrimitive.content },
        )
        assertEquals("no PIN pairing happened", 0, fake.registered)
        assertNoPassword(seen)
        assertTrue("never asked over the API", router.log.none { it.startsWith("POST /v1/wifi") })
    }

    @Test
    fun `a device that does not know this phone's token is reported after one try`() = runBlocking<Unit> {
        router.on("GET", "/v1/wifi") { errorResponse(404, "not_found", "no route") }
        fake.pairedTokens += "a-token-of-some-other-phone"
        fake.pinOnScreen = false
        val ble = FakeBle(listOf(deck))
        val vm = vm(ble = ble)
        vm.step.await { it is WifiStep.Offline }
        vm.useBluetooth()
        vm.startBleScan()
        val search = vm.step.await { it is WifiStep.BleSearch && it.devices.isNotEmpty() } as WifiStep.BleSearch
        vm.select(search.devices.single())
        assertEquals(WifiStep.Failed(WifiViewModel.NOT_PAIRED), vm.step.await { it is WifiStep.Failed })
        // One connection, one envelope: no second token was tried on that device.
        assertEquals(1, ble.links.size)
        assertEquals(1, ble.links.single().writes.size)
        assertEquals(1, fake.failures)
        assertTrue(fake.commands.isEmpty())
    }

    @Test
    fun `choosing bluetooth for a reachable device opens its window and a new name is remembered`() = runBlocking<Unit> {
        router.json("POST", "/v1/wifi/ble", """{"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi": -58, "saved": [], "ble": {"open": true, "expires_at": 1795654057}}""")
        fake.pairedTokens += token
        val renamed = FoundDevice("LapisDeck-9B10", -45, "AA:BB:CC:DD:9B:10")
        val vm = vm(ble = FakeBle(listOf(renamed)))
        vm.step.await { it is WifiStep.Networks && !it.scanning }
        vm.useBluetooth()
        router.take("POST /v1/wifi/ble")
        vm.startBleScan()
        val search = vm.step.await { it is WifiStep.BleSearch && it.devices.isNotEmpty() } as WifiStep.BleSearch
        vm.select(search.devices.single())
        vm.step.await { it is WifiStep.Networks && it.via == WifiVia.BLE }
        assertEquals("the proven device's name is kept for next time", "LapisDeck-9B10", devices.find(paired.id)!!.bleName)

        // Back to the network: the Bluetooth link is closed without finishing the device's window.
        vm.load()
        vm.step.await { it is WifiStep.Networks && it.via == WifiVia.API }
        assertFalse(fake.finished)
    }
}
