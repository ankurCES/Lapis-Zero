package dev.lapiszero.app.domain

import dev.lapiszero.app.FakeEventSource
import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.await
import dev.lapiszero.app.awaitTrue
import dev.lapiszero.app.data.ble.BleUuids
import dev.lapiszero.app.data.remote.ClientCommand
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.LapisEvent
import dev.lapiszero.app.data.remote.LapisJson
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class DeckRepositoryTest {
    private val source = FakeEventSource()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private lateinit var deck: DeckRepository

    @Before
    fun setUp() {
        deck = DeckRepository(source, scope, nowMs = { 42L })
        deck.start()
    }

    @After
    fun tearDown() = scope.cancel()

    @Test
    fun `state sync feeds the deck summary, a lapis-zero sync clears it`() = runBlocking<Unit> {
        source.emit(1, Fixtures.deckStateSync())
        assertEquals(3.21, deck.summary.await { it != null }!!.batteryW!!, 1e-9)
        source.emit(2, Fixtures.stateSync())
        deck.summary.await { it == null }
    }

    @Test
    fun `mesh and sdr frames become alerts, newest first, counted until seen`() = runBlocking<Unit> {
        source.emit(1, Fixtures.deckStateSync())
        source.emit(1, Fixtures.meshMessageFrame())
        source.emit(1, Fixtures.SDR_DECODE_FRAME)
        val alerts = deck.alerts.await { it.size == 2 }
        assertEquals(DeckAlert.Kind.SDR, alerts[0].kind)
        assertEquals("rtl_433 · 433.920 MHz", alerts[0].title)
        assertTrue(alerts[0].text.startsWith("model=Acurite-Tower"))
        assertFalse("decode time is noise", alerts[0].text.contains("time="))
        assertTrue(alerts[0].synthetic)
        assertEquals("MESH ch0 · !c0ffee01", alerts[1].title)
        assertEquals("rover checking in", alerts[1].text)
        assertEquals(42L, alerts[1].atMs)
        awaitTrue { deck.unseen.value == 2 }
        assertEquals(listOf(7L), deck.liveMesh.value.map { it.id })

        deck.setWatching(true)
        assertEquals(0, deck.unseen.value)
        source.emit(1, Fixtures.meshMessageFrame(id = 8, text = "seen while watching"))
        deck.alerts.await { it.size == 3 }
        assertEquals("nothing new while the screen is open", 0, deck.unseen.value)
        deck.setWatching(false)

        // Our own transmission echoed back: in the feed, not news.
        source.emit(1, Fixtures.meshMessageFrame(id = 9, text = "from the phone", outgoing = true))
        val own = deck.alerts.await { it.size == 4 }.first()
        assertEquals("MESH ch0 · you → all", own.title)
        assertEquals(0, deck.unseen.value)

        // The same message twice (echo and broadcast) is kept once in the chat.
        source.emit(1, Fixtures.meshMessageFrame(id = 9, text = "from the phone", outgoing = true))
        deck.alerts.await { it.size == 5 }
        assertEquals(listOf(7L, 8L, 9L), deck.liveMesh.value.map { it.id })

        deck.clearAlerts()
        assertTrue(deck.alerts.value.isEmpty())
    }

    @Test
    fun `the alert feed is capped`() = runBlocking<Unit> {
        source.emit(1, Fixtures.deckStateSync())
        repeat(DeckRepository.MAX_ALERTS + 5) { source.emit(1, Fixtures.SDR_DECODE_FRAME) }
        deck.alerts.await { it.size == DeckRepository.MAX_ALERTS }
        awaitTrue { deck.unseen.value == DeckRepository.MAX_ALERTS + 5 }
    }

    @Test
    fun `spectrum is subscribed on demand and again after every reconnect`() = runBlocking<Unit> {
        /** A frame after `frames`, so the collector is known to have handled them. */
        suspend fun settle(epoch: Int, n: Int) {
            source.emit(epoch, Fixtures.meshMessageFrame(id = 100L + n))
            deck.liveMesh.await { list -> list.any { it.id == 100L + n } }
        }
        source.emit(1, Fixtures.deckStateSync())
        source.emit(1, Fixtures.spectrumFrame())
        settle(1, 0)
        assertNull("rows are ignored unless asked for", deck.spectrum.value)

        deck.setSpectrumSubscribed(true)
        assertEquals(ClientCommand.SubscribeSpectrum(true), source.sent.last())
        source.emit(1, Fixtures.spectrumFrame(centerHz = 433_920_000))
        deck.spectrum.await { it?.centerHz == 433_920_000L }

        // A new connection forgets the subscription: it is asked for again on its first sync.
        source.sent.clear()
        source.emit(2, Fixtures.deckStateSync())
        // A second sync on the same connection does not ask again.
        source.emit(2, Fixtures.deckStateSync())
        settle(2, 1)
        assertEquals(listOf<ClientCommand>(ClientCommand.SubscribeSpectrum(true)), source.sent.toList())

        deck.setSpectrumSubscribed(false)
        assertEquals(ClientCommand.SubscribeSpectrum(false), source.sent.last())
        assertNull(deck.spectrum.value)
        source.sent.clear()
        source.emit(3, Fixtures.deckStateSync())
        settle(3, 2)
        assertTrue("not re-subscribed once switched off", source.sent.isEmpty())
    }

    @Test
    fun `describe packet keeps scalar fields, model first`() {
        val packet = LapisJson.parseToJsonElement("""{"id": 1, "temperature_C": 21.5, "nested": {"x": 1}, "model": "Oregon", "time": "now", "flag": null}""")
        assertEquals("model=Oregon id=1 temperature_C=21.5", DeckAlert.describePacket(packet))
        assertEquals("", DeckAlert.describePacket(null))
        assertEquals("POCSAG1200", DeckAlert.describePacket(LapisJson.parseToJsonElement("\"POCSAG1200\"")))
        assertEquals("!a1b2c3d4", DeckAlert.recipient("!a1b2c3d4"))
        assertEquals("all", DeckAlert.recipient("^all"))
    }

    // ------------------------------------------------------------------ device-kind gating

    private val zero = PairedDevice("z", "lapis-zero", "lapis-zero.tailnet.ts.net", true, "t", kind = DeviceKind.ZERO)
    private val deckDevice = zero.copy(id = "d", name = "lapis-deck", kind = DeviceKind.DECK)
    private val unknown = zero.copy(id = "u", kind = "")

    private fun state(json: String): DeviceState = (LapisEvent.parse(json) as LapisEvent.StateSync).state

    @Test
    fun `deck ui is hidden for a lapis-zero and shown for a lapis-deck`() {
        assertFalse(DeckGate.isDeck(state(Fixtures.stateSync()), zero))
        assertTrue(DeckGate.isDeck(state(Fixtures.deckStateSync()), deckDevice))
        // The live snapshot wins over what was remembered (a device reflashed either way).
        assertFalse(DeckGate.isDeck(state(Fixtures.stateSync()), deckDevice))
        assertTrue(DeckGate.isDeck(state(Fixtures.deckStateSync()), zero))
        // Before the first snapshot: what was remembered; nothing remembered means no deck.
        assertTrue(DeckGate.isDeck(null, deckDevice))
        assertFalse(DeckGate.isDeck(null, zero))
        assertFalse(DeckGate.isDeck(null, unknown))
    }

    @Test
    fun `advertised names tell a deck from a cardputer`() {
        assertEquals(DeviceKind.DECK, BleUuids.kindOf("LapisDeck-7F3A"))
        assertEquals(DeviceKind.ZERO, BleUuids.kindOf("LapisZero-4F2A"))
        assertEquals("", BleUuids.kindOf("Lapis device"))
        assertEquals("", BleUuids.kindOf(null))
        assertEquals(listOf("LapisZero-", "LapisDeck-"), BleUuids.NAME_PREFIXES)
    }
}
