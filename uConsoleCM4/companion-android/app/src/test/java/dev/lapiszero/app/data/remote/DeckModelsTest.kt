package dev.lapiszero.app.data.remote

import dev.lapiszero.app.Fixtures
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The Lapis-Deck additions decode from what the Rust daemon sends, and change nothing for a Lapis-Zero. */
class DeckModelsTest {
    private inline fun <reified T> decode(json: String): T = LapisJson.decodeFromString(json)

    @Test
    fun `deck state sync carries kind rails and the subsystem summary`() {
        val state = (LapisEvent.parse(Fixtures.deckStateSync()) as LapisEvent.StateSync).state
        assertTrue(state.isDeck)
        assertEquals(DeviceKind.DECK, state.device.kind)
        assertEquals(DeviceKind.DECK, state.deviceKind)
        val deck = state.deck!!
        assertFalse(deck.harnessUp)
        assertEquals(3.21, deck.batteryW!!, 1e-9)
        assertEquals(8, deck.rails.size)
        assertNull("bias tee cannot be read back", deck.rails.first { it.name == "bias_tee" }.on)
        assertEquals("3D", deck.summary.gps?.fix)
        assertTrue(deck.summary.gps!!.ppsLocked)
        assertEquals(4, deck.summary.mesh?.nodes)
        assertEquals(100_000_000L, deck.summary.sdr?.centerHz)
        assertTrue(deck.summary.sdr!!.synthetic)
        // The Lapis-Zero fields are all still there.
        assertEquals("macbook-pro", state.activeHost)
        assertEquals(84, state.batteryPct)
    }

    @Test
    fun `a lapis-zero state sync is not a deck`() {
        val state = (LapisEvent.parse(Fixtures.stateSync()) as LapisEvent.StateSync).state
        assertFalse(state.isDeck)
        assertNull(state.device.kind)
        assertNull(state.deck)
        assertEquals(DeviceKind.ZERO, state.deviceKind)
    }

    @Test
    fun `a deck block alone is enough and nulls inside it are tolerated`() {
        val state = decode<DeviceState>(
            """{"agent_state": "IDLE", "deck": {"harness": "up", "battery_w": null, "rails": [],
                "summary": {"sdr": null, "mesh": null, "gps": {"status": "offline: not connected yet", "fix": null, "used": 0, "pps_locked": false}}}}""",
        )
        assertTrue(state.isDeck)
        assertTrue(state.deck!!.harnessUp)
        assertNull(state.deck.summary.sdr)
        assertNull(state.deck.summary.gps!!.fix)
    }

    @Test
    fun `health says lapis-deck and the harness state, lapis-zero health says neither`() {
        val deck = decode<Health>(Fixtures.DECK_HEALTH)
        assertEquals(DeviceKind.DECK, deck.kind)
        assertEquals(false, deck.harness)
        assertEquals("lapis-deck", deck.hostname)
        val zero = decode<Health>("""{"ok": true, "version": "0.2.0", "hostname": "lapis-zero", "uptime_sec": 5, "mock": true, "device_id": "dev_local"}""")
        assertNull(zero.kind)
        assertNull(zero.harness)
        assertEquals("Lapis-Deck", DeviceKind.label(DeviceKind.DECK))
        assertEquals("", DeviceKind.label(null))
    }

    @Test
    fun `tier 2 approval needs a hold of at least the daemon's hold_ms`() {
        val a = (LapisEvent.parse(Fixtures.deckApproval(tier = 2)) as LapisEvent.ApprovalRequest).approval
        assertEquals(2, a.tier)
        assertEquals("hw.gpio_rail needs hold-to-confirm", a.reason)
        assertEquals("android", a.source)
        assertEquals(1500L, a.holdMs)
        assertEquals(1795654057L, a.expiresAt)
        assertNull(a.chatId)
        assertTrue(a.needsHold)
        assertEquals(1500L, a.holdDurationMs)

        // Tier 2 with no hold time from the daemon still holds the floor; a longer one is honoured.
        assertEquals(Approval.MIN_HOLD_MS, a.copy(holdMs = 0).holdDurationMs)
        assertTrue(a.copy(holdMs = 0).needsHold)
        assertEquals(3000L, a.copy(holdMs = 3000).holdDurationMs)
        assertEquals(Approval.MIN_HOLD_MS, a.copy(holdMs = 10).holdDurationMs)
    }

    @Test
    fun `tier 1 and pre-tier approvals are a plain tap`() {
        val t1 = (LapisEvent.parse(Fixtures.deckApproval("req_1", tier = 1)) as LapisEvent.ApprovalRequest).approval
        assertEquals(1, t1.tier)
        assertFalse(t1.needsHold)
        assertEquals(0L, t1.holdDurationMs)
        // A Lapis-Zero daemon sends no tier and no hold: today's behaviour.
        val old = (LapisEvent.parse(Fixtures.approval()) as LapisEvent.ApprovalRequest).approval
        assertNull(old.tier)
        assertEquals("", old.reason)
        assertEquals(0L, old.holdMs)
        assertNull(old.expiresAt)
        assertFalse(old.needsHold)
    }

    @Test
    fun `pending approvals in a deck state sync keep their tier`() {
        val state = (LapisEvent.parse(Fixtures.deckStateSync("WAITING_APPROVAL", Fixtures.deckApproval())) as LapisEvent.StateSync).state
        assertTrue(state.pendingApprovals.single().needsHold)
    }

    @Test
    fun `telemetry battery gains draw and the rails`() {
        val t = decode<Telemetry>(
            """{"battery": {"charging": false, "current_a": -0.82, "percent": 78, "power_w": 3.21, "status": "Discharging", "voltage_v": 3.92},
                "rails": ${Fixtures.DECK_RAILS}, "uptime_sec": 8}""",
        )
        assertEquals(3.21, t.battery!!.powerW!!, 1e-9)
        assertEquals(-0.82, t.battery.currentA!!, 1e-9)
        assertEquals("Discharging", t.battery.status)
        assertEquals(8, t.rails.size)
        // Lapis-Zero telemetry decodes as before.
        val zero = decode<Telemetry>(Fixtures.TELEMETRY)
        assertNull(zero.battery!!.powerW)
        assertTrue(zero.rails.isEmpty())
    }

    @Test
    fun `deck rest bodies decode`() {
        val status = decode<DeckStatus>(Fixtures.DECK_STATUS)
        assertEquals(78, status.battery!!.percent)
        assertEquals(3.21, status.battery.powerW!!, 1e-9)
        assertTrue(status.mock)
        assertTrue(status.rails.first { it.name == "wifi" }.cutsPhoneLink)
        assertFalse(status.rails.first { it.name == "gps" }.cutsPhoneLink)

        val tools = decode<DeckToolsResponse>(Fixtures.DECK_TOOLS).tools.associateBy { it.name }
        assertEquals(1, tools.getValue("mesh.send_message").tier)
        assertEquals(2, tools.getValue("hw.gpio_rail").tier)
        assertEquals(60, tools.getValue("sdr.decode_ism").inputSchema["properties"]!!.jsonObject["duration_s"]!!.jsonObject["maximum"]!!.jsonPrimitive.int)

        val mesh = decode<MeshSnapshot>(Fixtures.DECK_MESH)
        assertEquals("!decafbad", mesh.myNode)
        assertEquals(2_712_847_316L, mesh.nodes.first { it.shortName == "BASE" }.num) // above Int.MAX_VALUE
        assertNull(mesh.nodes.first { it.id == "!decafbad" }.snr)
        assertEquals("Ridge Relay", mesh.nodes.first().displayName)
        assertEquals("^all", mesh.messages.first().to)

        val gps = decode<GpsSnapshot>(Fixtures.DECK_GPS)
        assertTrue(gps.fix!!.hasFix)
        assertEquals("3D", gps.fix.fix)
        assertEquals(120L, gps.ppsOffsetNs)
        assertEquals(2, gps.satellites.size)
        assertEquals(41.0, gps.satellites.first().snr!!, 1e-9)
        assertFalse(decode<GpsSnapshot>("""{"status": "offline: not connected yet", "fix": null, "satellites": []}""").fix?.hasFix ?: false)

        val sdr = decode<SdrSnapshot>(Fixtures.DECK_SDR)
        assertEquals("synthetic", sdr.status)
        assertEquals(2_048_000L, sdr.source.sampleRate)
        assertEquals(100_171_000.0, sdr.peaks.first().freqHz, 1.0)

        val audit = decode<AuditResponse>(Fixtures.DECK_AUDIT).entries
        assertEquals("approval", audit.first().event)
        assertEquals(1795653458L, audit.first().ts)

        val denied = decode<ToolInvokeResult>(Fixtures.INVOKE_DENIED)
        assertFalse(denied.ok)
        assertEquals("deny", denied.decision)
        assertEquals("android:Dev phone", denied.by)
        val policy = decode<ToolInvokeResult>("""{"decision":"policy_deny","error":"bogus.tool is not a cyberdeck tool","ok":false}""")
        assertNull(policy.tier)
    }

    @Test
    fun `invoke request names the tool, its args and the android source`() {
        val body = LapisJson.parseToJsonElement(
            LapisJson.encodeToString(ToolInvokeRequest("mesh.send_message", JsonObject(mapOf("channel" to kotlinx.serialization.json.JsonPrimitive(0))))),
        ).jsonObject
        assertEquals("mesh.send_message", body["tool"]!!.jsonPrimitive.content)
        assertEquals(0, body["args"]!!.jsonObject["channel"]!!.jsonPrimitive.int)
        assertEquals("android", body["context"]!!.jsonObject["source"]!!.jsonPrimitive.content)
        assertFalse("no chat id is sent when there is none", "chat_id" in body["context"]!!.jsonObject)
        assertEquals("""{"on":false}""", LapisJson.encodeToString(RailRequest(false)))
        assertEquals("""{"center_hz":433920000}""", LapisJson.encodeToString(TuneRequest(433_920_000)))
    }

    @Test
    fun `deck websocket frames parse and unknown ones stay ignored`() {
        val mesh = LapisEvent.parse(Fixtures.meshMessageFrame()) as LapisEvent.MeshMessageEvent
        assertEquals("rover checking in", mesh.message.text)
        assertEquals(7L, mesh.message.id)
        assertEquals(-3.5, mesh.message.snr!!, 1e-9)

        // `mesh.send_message` echoes the last message, which may be null: nothing to show.
        assertTrue(LapisEvent.parse("""{"type": "mesh_message", "message": null}""") is LapisEvent.Unknown)

        val decode = LapisEvent.parse(Fixtures.SDR_DECODE_FRAME) as LapisEvent.SdrDecodeEvent
        assertEquals("rtl_433", decode.decode.decoder)
        assertEquals(433_920_000L, decode.decode.freqHz)
        assertTrue(decode.decode.synthetic)
        assertEquals("Acurite-Tower", decode.decode.packet!!.jsonObject["model"]!!.jsonPrimitive.content)

        val spectrum = (LapisEvent.parse(Fixtures.spectrumFrame()) as LapisEvent.SdrSpectrumEvent).spectrum
        assertEquals(512, spectrum.bins.size)
        assertEquals(240, spectrum.bins[256]) // unsigned
        assertEquals(30, spectrum.bins[0])
        assertEquals(100_000_000L, spectrum.centerHz)
        assertEquals(1, spectrum.peaks.size)
        // Broken base64 gives an empty row, not a dropped connection.
        val broken = LapisEvent.parse("""{"type": "sdr_spectrum", "bins": "***"}""") as LapisEvent.SdrSpectrumEvent
        assertEquals(0, broken.spectrum.bins.size)

        assertEquals(LapisEvent.Unknown("waterfall_v2"), LapisEvent.parse("""{"type": "waterfall_v2", "x": 1}"""))
    }

    @Test
    fun `subscribe spectrum command`() {
        assertEquals("""{"type":"subscribe_spectrum","enabled":true}""", ClientCommand.SubscribeSpectrum(true).encode())
        assertEquals("""{"type":"subscribe_spectrum","enabled":false}""", ClientCommand.SubscribeSpectrum(false).encode())
    }

    @Test
    fun `rail labels and order follow the hud`() {
        assertEquals("USB hub", Rail.label("usb_hub"))
        assertEquals("mystery", Rail.label("mystery"))
        assertEquals(listOf("gps", "lora", "sdr", "usb_hub", "bias_tee", "lte", "wifi", "bluetooth"), Rail.ORDER)
    }
}
