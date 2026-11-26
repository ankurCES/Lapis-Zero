package dev.lapiszero.app

import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.LapisApiClient
import dev.lapiszero.app.data.remote.LapisApiException
import dev.lapiszero.app.data.remote.LapisWsClient
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.ConnectionState
import dev.lapiszero.app.domain.DecideResult
import dev.lapiszero.app.domain.DeckAlert
import dev.lapiszero.app.domain.DeckRepository
import dev.lapiszero.app.ui.deck.MeshRules
import dev.lapiszero.app.ui.deck.QuickActions
import dev.lapiszero.app.ui.deck.ToolResults
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test

/**
 * End-to-end against the real Lapis-Deck Rust daemon with mock hardware. Skipped unless told
 * where it is (it must be a `--mock-hw` daemon: the test switches rails and transmits on the mock mesh):
 *
 *   cargo run -p lapis-daemon -- --mock-hw --dev-pair-token <token> --port 18441
 *   cd companion-android && ./gradlew testDebugUnitTest --tests "*.DeckDaemonIntegrationTest" \
 *       -Plapis.deck.url=http://127.0.0.1:18441 -Plapis.deck.token=<token>
 */
class DeckDaemonIntegrationTest {
    private val url: String? = System.getProperty("lapis.deck.url") ?: System.getenv("LAPIS_DECK_URL")
    private val token: String = System.getProperty("lapis.deck.token") ?: System.getenv("LAPIS_DECK_TOKEN") ?: ""

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private lateinit var api: LapisApiClient
    private lateinit var repo: ChatRepository
    private lateinit var deck: DeckRepository

    @Before
    fun setUp() {
        assumeTrue("no Lapis-Deck daemon configured (set lapis.deck.url)", !url.isNullOrBlank())
        val host = url!!.removePrefix("http://").removePrefix("https://").trimEnd('/')
        val endpoint = Endpoint(host, tls = url.startsWith("https://"))
        api = LapisApiClient(endpoint, { token })
        val ws = LapisWsClient(api.http, endpoint.wsUrl, scope)
        repo = ChatRepository(api, ws, scope)
        deck = DeckRepository(ws, scope)
        deck.start()
    }

    @After
    fun tearDown() {
        if (::repo.isInitialized) repo.stop()
        scope.cancel()
    }

    private suspend fun connect() {
        repo.start()
        repo.connection.await(15_000) { it == ConnectionState.ONLINE }
        repo.approvals.await(10_000) { it.isEmpty() }
    }

    @Test
    fun `health state and every deck read decode against the rust daemon`() = runBlocking<Unit> {
        val health = api.health()
        assertTrue("this test must only ever talk to a mock-hw daemon", health.mock)
        assertEquals(DeviceKind.DECK, health.kind)
        val state = api.state()
        assertTrue(state.isDeck)
        assertEquals(8, state.deck!!.rails.size)

        val status = api.deckStatus()
        assertTrue(status.mock)
        assertTrue(status.battery!!.powerW != null)
        val tools = api.deckTools().associateBy { it.name }
        assertEquals(1, tools.getValue(QuickActions.MESH_SEND).tier)
        assertEquals(2, tools.getValue("hw.gpio_rail").tier)
        for (q in QuickActions.SDR + QuickActions.NETOPS) {
            assertEquals("${q.tool} is the tier the app assumes", QuickActions.DEFAULT_TIERS[q.tool], tools.getValue(q.tool).tier)
        }
        assertTrue(api.gps().fix!!.hasFix)
        assertTrue(api.mesh().nodes.isNotEmpty())
        assertTrue(api.sdr().source.sampleRate > 0)
        api.audit(5)

        try {
            api.setRail("no_such_rail", true)
            fail("unknown rail must be refused")
        } catch (e: LapisApiException) {
            assertTrue(e.isNotFound)
        }
    }

    @Test
    fun `socket frames, quick actions, tier 1 and tier 2 approvals end to end`() = runBlocking<Unit> {
        connect()
        assertTrue(repo.state.value!!.isDeck)
        deck.summary.await { it != null }

        // Live waterfall rows, only after asking.
        deck.setSpectrumSubscribed(true)
        val row = deck.spectrum.await(10_000) { it != null }!!
        assertEquals(512, row.bins.size)
        deck.setSpectrumSubscribed(false)

        // Tier 0 runs at once; the ISM decoder also broadcasts what it heard.
        for (q in QuickActions.SDR + QuickActions.NETOPS) {
            val r = api.invokeTool(q.tool, q.args)
            assertTrue("${q.tool}: ${r.error}", r.ok)
            assertEquals(0, r.tier)
            assertTrue("${q.tool} gives rows", ToolResults.rows(q.tool, r.result).isNotEmpty())
        }
        val decoded = deck.alerts.await(10_000) { list -> list.any { it.kind == DeckAlert.Kind.SDR } }
        assertTrue(decoded.first { it.kind == DeckAlert.Kind.SDR }.title.startsWith("rtl_433"))

        // Tier 1: the call waits for the owner; this phone approves it with a tap.
        val text = "integration ${System.nanoTime() % 100_000}"
        val send = async { api.invokeTool(QuickActions.MESH_SEND, MeshRules.args(0, text)) }
        val t1 = repo.approvals.await(10_000) { it.isNotEmpty() }.single()
        assertEquals(1, t1.tier)
        assertFalse(t1.needsHold)
        assertEquals("android", t1.source)
        assertEquals(DecideResult.Done, repo.decide(t1.requestId, Decision.APPROVE_ONCE))
        val sent = send.await()
        assertTrue(sent.error, sent.ok)
        assertEquals("approve_once", sent.decision)
        deck.liveMesh.await(10_000) { list -> list.any { it.text == text } }

        // Tier 2: a hold is asked for; denied here, and the call says so.
        val rail = async {
            api.invokeTool("hw.gpio_rail", buildJsonObject {
                put("name", "usb_hub")
                put("state", false)
            })
        }
        val t2 = repo.approvals.await(10_000) { it.isNotEmpty() }.single()
        assertEquals(2, t2.tier)
        assertTrue(t2.needsHold)
        assertEquals(1500L, t2.holdDurationMs)
        assertEquals(DecideResult.Done, repo.decide(t2.requestId, Decision.DENY))
        val refused = rail.await()
        assertFalse(refused.ok)
        assertEquals("deny", refused.decision)

        // The owner's own switch and tune (no gate), reflected in the status.
        assertTrue(api.setRail("usb_hub", false).ok)
        assertEquals(false, api.deckStatus().rails.first { it.name == "usb_hub" }.on)
        assertTrue(api.setRail("usb_hub", true).ok)
        assertTrue(api.tune(433_920_000).ok)
        assertEquals(433_920_000L, api.sdr().source.centerHz)
        assertTrue(api.tune(100_000_000).ok)
    }
}
