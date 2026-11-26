package dev.lapiszero.app.data.ble

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** The phone-side protocol driver against a faithful port of the device's session logic. */
@OptIn(ExperimentalCoroutinesApi::class)
class ProvisioningClientTest {
    private val device = FakeLapisDevice()

    private fun TestScope.client(
        link: FakeLapisDevice.Connection,
        events: MutableList<BleEvent>? = null,
    ): ProvisioningClient {
        val client = ProvisioningClient(link, nowMs = { testScheduler.currentTime })
        backgroundScope.launch { client.pump() }
        if (events != null) backgroundScope.launch { client.events.collect { events += it } }
        return client
    }

    private suspend fun ProvisioningClient.pair(pin: String = "482917"): PinResult {
        handshake()
        return verifyPin(pin, "Pixel", "Google Pixel 9")
    }

    private suspend inline fun <reified T : Throwable> expect(block: () -> Unit): T {
        try {
            block()
        } catch (e: Throwable) {
            if (e is T) return e
            throw e
        }
        fail("expected ${T::class.simpleName}")
        throw AssertionError()
    }

    @Test
    fun `full provisioning flow in protocol order`() = runTest {
        val link = device.connect(attMtu = 185)
        val c = client(link)

        val verified = c.pair() as PinResult.Verified
        assertEquals("dev_01", verified.deviceId)
        assertEquals("tok-from-ble-0123456789", verified.apiToken)
        assertFalse("the token stays out of toString", verified.toString().contains("tok-from-ble"))
        assertTrue(c.isVerified)

        val networks = c.wifiScan()
        assertEquals(listOf("HomeWiFi", "café guest"), networks.map { it.ssid })
        assertEquals(-48, networks[0].rssi)
        assertFalse(networks[1].secured)

        val wifi = c.wifiConnect("HomeWiFi", "correct horse")
        assertEquals("connected", wifi.state) // "connecting" was skipped
        assertEquals("192.168.1.42", wifi.localIp)

        val progress = c.tailscaleUp(authKey = null).toList()
        assertEquals(
            listOf(
                TailscaleProgress.AuthUrl("https://login.tailscale.com/a/abc123"),
                TailscaleProgress.Ready("lapis-zero.tailnet.ts.net", "100.91.12.4"),
            ),
            progress,
        )

        assertEquals(listOf("ANTHROPIC_API_KEY"), c.pushSecrets(mapOf("ANTHROPIC_API_KEY" to "sk-ant-secret")))
        assertEquals("sk-ant-secret", device.savedSecrets["ANTHROPIC_API_KEY"])

        c.finish()
        assertTrue(device.finished)

        // The device enforced seq == expected on every message, so this order proves seq 1..6.
        assertEquals(
            listOf("pin_verify", "wifi_scan", "wifi_connect", "tailscale_up", "push_secrets", "finish_provisioning"),
            device.commands.map { it.getValue("cmd").jsonPrimitive.content },
        )
        val pinVerify = device.commands[0]
        assertEquals(64, pinVerify.getValue("proof").jsonPrimitive.content.length)
        assertEquals("Pixel", pinVerify.getValue("device_name").jsonPrimitive.content)
        assertEquals("Google Pixel 9", pinVerify.getValue("device_model").jsonPrimitive.content)
        assertEquals(JsonNull, device.commands[3]["authkey"])
        assertEquals(1, device.registered)
    }

    @Test
    fun `nothing readable crosses the link after the hellos`() = runTest {
        val link = device.connect()
        val c = client(link)
        c.pair()
        c.wifiConnect("HomeWiFi", "correct horse")
        c.pushSecrets(mapOf("ANTHROPIC_API_KEY" to "sk-ant-secret"))
        val wire = link.writes.joinToString("") { it.toString(Charsets.ISO_8859_1) }
        for (secret in listOf("482917", "correct horse", "sk-ant-secret", "HomeWiFi", "pin_verify", "wifi_connect")) {
            assertFalse("'$secret' is visible on the wire", wire.contains(secret))
        }
        // Every frame is flagged as an encrypted envelope and fits the ATT payload.
        assertTrue(link.writes.all { it[3].toInt() == ChunkFraming.FLAG_ENCRYPTED && it.size <= 185 - 3 })
    }

    @Test
    fun `auth key path sends the key and gets ready without a login url`() = runTest {
        val c = client(device.connect())
        c.pair()
        assertEquals(
            listOf(TailscaleProgress.Ready("lapis-zero.tailnet.ts.net", "100.91.12.4")),
            c.tailscaleUp(authKey = " tskey-auth-kABC ").toList(),
        )
        assertEquals("tskey-auth-kABC", device.commands.last().getValue("authkey").jsonPrimitive.content)
    }

    @Test
    fun `wrong pin is met with silence then the right pin works with seq 1 again`() = runTest {
        val link = device.connect()
        val c = client(link)
        c.handshake()

        val started = testScheduler.currentTime
        assertEquals(PinResult.Rejected(attemptsLeft = 2), c.verifyPin("000000", "Pixel", "m"))
        assertEquals("the verdict is the timeout", ProvisioningClient.PIN_TIMEOUT_MS, testScheduler.currentTime - started)
        assertEquals(1, device.failures)
        assertEquals(1, c.pinFailures)
        assertFalse(c.isVerified)
        assertTrue("the device could not read it", device.commands.isEmpty())

        assertEquals(PinResult.Rejected(attemptsLeft = 1), c.verifyPin("999999", "Pixel", "m"))
        assertEquals(2, device.failures)

        // Same hellos, corrected PIN: the device still expects seq = 1 and accepts it.
        val ok = c.verifyPin("482917", "Pixel", "m")
        assertTrue(ok is PinResult.Verified)
        assertEquals(1, device.handshakes)
        assertEquals(0, c.pinFailures)
        assertEquals(listOf("pin_verify"), device.commands.map { it.getValue("cmd").jsonPrimitive.content })
        // And the stream continues at seq 2 on both sides.
        assertEquals(2, c.wifiScan().size)
    }

    @Test
    fun `three wrong pins lock the device which rotates and drops the link`() = runTest {
        val link = device.connect()
        val events = ArrayList<BleEvent>()
        val c = client(link, events)
        val oldKey = device.publicKey.toHex()
        c.handshake()
        assertEquals(PinResult.Rejected(2), c.verifyPin("000000", "p", "m"))
        assertEquals(PinResult.Rejected(1), c.verifyPin("000001", "p", "m"))
        assertEquals(PinResult.LockedOut, c.verifyPin("000002", "p", "m"))
        runCurrent()
        assertFalse(link.open)
        assertEquals(1, device.rotations)
        assertNotEquals(oldKey, device.publicKey.toHex())
        assertNotEquals("482917", device.pin)
        assertTrue(events.contains(BleEvent.Disconnected))
        // The old session is dead for good: even the PIN that used to be right gets nowhere.
        assertEquals(PinResult.LockedOut, c.verifyPin("482917", "p", "m"))
        assertFalse(c.isVerified)
        assertEquals(1, device.handshakes)

        // Redo the whole handshake on a new connection and read the new ServerHello.
        val link2 = device.connect()
        val c2 = client(link2)
        val again = c2.pair(pin = device.pin)
        assertTrue(again is PinResult.Verified)
        assertEquals(2, device.handshakes)
    }

    @Test
    fun `a device that cannot prove the pin is rejected`() = runTest {
        device.lieAboutProof = true
        val c = client(device.connect())
        val e = expect<HandshakeException> { c.pair() }
        assertTrue(e.message!!.contains("prove"))
        assertFalse(c.isVerified)
        expect<IllegalStateException> { c.wifiScan() }
        assertEquals("nothing was sent after the bad proof", 1, device.commands.size)
    }

    @Test
    fun `commands before verification are refused locally`() = runTest {
        val c = client(device.connect())
        c.handshake()
        expect<IllegalStateException> { c.wifiScan() }
        expect<IllegalStateException> { c.wifiConnect("x", null) }
        expect<IllegalStateException> { c.pushSecrets(mapOf("A" to "b")) }
        expect<IllegalStateException> { c.tailscaleUp(null).toList() }
        expect<IllegalStateException> { c.finish() }
        expect<IllegalArgumentException> { c.verifyPin("12345", "p", "m") }
        assertEquals("none of them counted as a failed attempt on the device", 0, device.failures)
    }

    @Test
    fun `verifying before the handshake fails cleanly`() = runTest {
        val c = client(device.connect())
        expect<HandshakeException> { c.verifyPin("482917", "p", "m") }
    }

    @Test
    fun `handshake needs an mtu that fits the 49 byte hello in one write`() = runTest {
        val e = expect<HandshakeException> { client(device.connect(attMtu = 23)).handshake() }
        assertTrue(e.message!!.contains("MTU"))
        assertEquals(0, device.handshakes)
        client(device.connect(attMtu = 52)).handshake()
        assertEquals(1, device.handshakes)
    }

    @Test
    fun `messages are chunked to the negotiated mtu in both directions`() = runTest {
        // MTU 52: 45 data bytes per frame, so even pin_verify needs several chunks.
        val link = device.connect(attMtu = 52)
        device.networks = kotlinx.serialization.json.buildJsonArray {
            repeat(20) { i ->
                add(kotlinx.serialization.json.buildJsonObject {
                    put("ssid", kotlinx.serialization.json.JsonPrimitive("Network number $i with a long name"))
                    put("rssi", kotlinx.serialization.json.JsonPrimitive(-40 - i))
                    put("secured", kotlinx.serialization.json.JsonPrimitive(i % 2 == 0))
                })
            }
        }
        val c = client(link)
        assertTrue(c.pair() is PinResult.Verified)
        assertTrue("pin_verify was split", link.writes.size >= 4)
        assertTrue(link.writes.all { it.size <= 52 - 3 })
        val first = link.writes.first()
        assertEquals(0, first[1].toInt()) // chunk_idx
        assertEquals(link.writes.size, first[2].toInt()) // total_chunks
        // msg_id is the same for every chunk of the message.
        assertEquals(1, link.writes.map { it[0] }.toSet().size)

        val networks = c.wifiScan() // ~1.3 kB event, dozens of notification chunks
        assertEquals(20, networks.size)
        assertEquals("Network number 19 with a long name", networks.last().ssid)
        assertEquals(-59, networks.last().rssi)
    }

    @Test
    fun `at mtu 517 written frames stay within the 512 byte attribute limit`() = runTest {
        val link = device.connect(attMtu = 517)
        val c = client(link)
        c.pair()
        val before = link.writes.size
        val big = "k".repeat(1_500)
        assertEquals(listOf("BIG_SECRET"), c.pushSecrets(mapOf("BIG_SECRET" to big)))
        assertEquals("reassembled intact on the device", big, device.savedSecrets["BIG_SECRET"])
        val frames = link.writes.drop(before)
        assertEquals(4, frames.size) // ~1.56 kB envelope in 508-byte slices
        assertEquals(512, frames.first().size)
        assertTrue(frames.all { it.size <= ChunkFraming.MAX_ATT_VALUE })
        // The protocol formula itself is untouched; only what this phone writes is capped.
        assertEquals(510, ChunkFraming.maxChunkData(517))
        assertEquals(508, ChunkFraming.writeChunkData(517))
        assertEquals(178, ChunkFraming.writeChunkData(185))
        assertEquals(16, ChunkFraming.writeChunkData(23))
    }

    @Test
    fun `an error event fails that command and the session stays usable`() = runTest {
        device.wifiScanFails = true
        val c = client(device.connect())
        c.pair()
        val e = expect<BleCommandException> { c.wifiScan() }
        assertEquals("wifi_scan", e.command)
        assertEquals("RuntimeError: nmcli not found", e.message)
        device.wifiScanFails = false
        assertEquals(2, c.wifiScan().size)

        val missing = expect<BleCommandException> { c.wifiConnect("", null) }
        assertEquals("ssid is required", missing.message)
        assertEquals("failed", c.wifiConnect("HomeWiFi", "wrong password").state)
        assertEquals("connected", c.wifiConnect("café guest", null).state)
        assertEquals(JsonNull, device.commands.last()["psk"])
    }

    @Test
    fun `tailscale failure and login urls are reported in order`() = runTest {
        device.authUrls = listOf("https://login.tailscale.com/a/one", "https://login.tailscale.com/a/two")
        device.tailscaleError = "timed out waiting for Tailscale to connect"
        val c = client(device.connect())
        c.pair()
        val seen = ArrayList<TailscaleProgress>()
        val e = expect<BleCommandException> { c.tailscaleUp(null).collect { seen += it } }
        assertEquals("tailscale_up", e.command)
        assertEquals("timed out waiting for Tailscale to connect", e.message)
        assertEquals(
            listOf(
                TailscaleProgress.AuthUrl("https://login.tailscale.com/a/one"),
                TailscaleProgress.AuthUrl("https://login.tailscale.com/a/two"),
            ),
            seen,
        )
        // Still usable afterwards.
        device.tailscaleError = null
        assertTrue(c.tailscaleUp("tskey-auth-x").toList().single() is TailscaleProgress.Ready)
    }

    @Test
    fun `a silent device times out instead of hanging`() = runTest {
        val c = client(device.connect())
        c.pair()
        device.mute += "wifi_scan"
        val before = testScheduler.currentTime
        val e = expect<BleTimeoutException> { c.wifiScan(timeoutMs = 3_000) }
        assertEquals("wifi_scan", e.command)
        assertEquals(3_000, testScheduler.currentTime - before)
        // The device did read that command, so both counters moved on and the session continues.
        device.mute.clear()
        assertEquals("connected", c.wifiConnect("HomeWiFi", "correct horse").state)
        assertEquals(2, c.wifiScan().size)
    }

    @Test
    fun `replayed and plaintext notifications are ignored`() = runTest {
        val events = ArrayList<BleEvent>()
        val c = client(device.connect(), events)
        c.pair()
        c.wifiScan()
        runCurrent()
        val before = events.size
        assertTrue(events.last() is BleEvent.WifiList)

        device.replayLastMessage() // seq already consumed
        device.replayLastMessage()
        // A forged plaintext event (flags = 0) announcing a different token.
        val forged = """{"evt":"pin_verified","proof":"00","device_id":"evil","api_token":"attacker"}""".toByteArray()
        ChunkFraming.split(9, 0, forged, 178).forEach(device::inject)
        // Garbage that claims to be encrypted.
        ChunkFraming.split(10, ChunkFraming.FLAG_ENCRYPTED, ByteArray(40) { 7 }, 178).forEach(device::inject)
        runCurrent()
        assertEquals("no new events", before, events.size)

        // The legitimate stream is unharmed: the next real message still decrypts.
        assertEquals(2, c.wifiScan().size)
    }

    @Test
    fun `a malformed notification is answered with one reset frame`() = runTest {
        val link = device.connect()
        val c = client(link)
        c.pair()
        val writesBefore = link.writes.size
        device.inject(byteArrayOf(5, 3, 2, 1, 0x55)) // chunk_idx >= total_chunks
        runCurrent()
        assertEquals(writesBefore + 1, link.writes.size)
        assertEquals("00000180", link.writes.last().toHex())
        assertEquals(2, c.wifiScan().size)
    }

    @Test
    fun `a reset frame from the device fails the pending command`() = runTest {
        val c = client(device.connect())
        c.pair()
        device.mute += "wifi_scan"
        backgroundScope.launch {
            kotlinx.coroutines.delay(10)
            device.inject(ChunkFraming.resetFrame())
        }
        val e = expect<BleCommandException> { c.wifiScan() }
        assertEquals("wifi_scan", e.command)
        assertTrue(e.message!!.contains("reset"))
    }

    @Test
    fun `losing the link mid-command surfaces as link lost and poisons the session`() = runTest {
        val link = device.connect(attMtu = 52)
        val c = client(link)
        c.pair()
        link.failWritesFrom = link.writes.size + 1 // the second chunk of the next message fails
        expect<BleLinkLostException> { c.pushSecrets(mapOf("ANTHROPIC_API_KEY" to "sk-ant-a-long-secret-value-that-needs-chunks")) }
        link.failWritesFrom = null
        // The spent sequence number is never reused: the session refuses to continue.
        expect<BleLinkLostException> { c.wifiScan() }
        assertTrue(device.savedSecrets.isEmpty())
    }

    @Test
    fun `finish tolerates the device dropping the link right after bye`() = runTest {
        val link = device.connect()
        val events = ArrayList<BleEvent>()
        val c = client(link, events)
        c.pair()
        c.finish()
        runCurrent()
        assertTrue(device.finished)
        assertFalse(link.open)
        assertTrue(events.contains(BleEvent.Bye))
        assertTrue(events.contains(BleEvent.Disconnected))
        expect<BleLinkLostException> { c.wifiScan() }
    }

    @Test
    fun `event parsing covers every documented event`() {
        fun parse(json: String) = BleEvent.parse(json.toByteArray())
        val verified = parse("""{"evt":"pin_verified","proof":"ab","device_id":"dev_01","api_token":"t"}""") as BleEvent.PinVerified
        assertEquals("dev_01", verified.deviceId)
        assertFalse(verified.toString().contains("api_token"))
        assertEquals(
            BleEvent.WifiList(listOf(WifiNetwork("a", -58, true), WifiNetwork("b"))),
            parse("""{"evt":"wifi_list","networks":[{"ssid":"a","rssi":-58,"secured":true,"extra":1},{"ssid":"b"},{"bad":1}]}"""),
        )
        assertEquals(BleEvent.WifiStatus("connecting", ""), parse("""{"evt":"wifi_status","state":"connecting","local_ip":""}"""))
        assertEquals(BleEvent.TailscaleAuth("https://login.tailscale.com/a/x"), parse("""{"evt":"tailscale_auth","url":"https://login.tailscale.com/a/x"}"""))
        assertEquals(BleEvent.Ready("f.ts.net", "100.1.2.3"), parse("""{"evt":"ready","tailscale_fqdn":"f.ts.net","tailscale_ip":"100.1.2.3"}"""))
        assertEquals(BleEvent.SecretsSaved(listOf("A", "B")), parse("""{"evt":"secrets_saved","keys":["A","B"]}"""))
        assertEquals(BleEvent.Bye, parse("""{"evt":"bye"}"""))
        assertEquals(BleEvent.Error("wifi_scan", "boom"), parse("""{"evt":"error","cmd":"wifi_scan","message":"boom"}"""))
        assertEquals(BleEvent.Unknown("future"), parse("""{"evt":"future","x":1}"""))
        assertEquals(null, parse("""{"no_evt":1}"""))
        assertEquals(null, parse("not json"))
        assertEquals(null, parse("[]"))
    }

    @Test
    fun `gatt uuids match the protocol document`() {
        assertEquals("7c4f0001-9a3b-4c6e-8e21-51a0c0de0001", BleUuids.SERVICE.toString())
        assertEquals("7c4f0001-9a3b-4c6e-8e21-51a0c0de0002", BleUuids.HANDSHAKE.toString())
        assertEquals("7c4f0001-9a3b-4c6e-8e21-51a0c0de0003", BleUuids.CMD_TX.toString())
        assertEquals("7c4f0001-9a3b-4c6e-8e21-51a0c0de0004", BleUuids.EVT_RX.toString())
        assertEquals(517, BleUuids.REQUESTED_MTU)
        assertEquals(3, ProvisioningClient.MAX_PIN_FAILURES)
    }
}
