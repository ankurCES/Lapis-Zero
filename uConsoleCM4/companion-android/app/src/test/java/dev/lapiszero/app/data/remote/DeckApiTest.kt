package dev.lapiszero.app.data.remote

import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.errorResponse
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/** The `/v1/deck/...` calls: paths, bodies, bearer auth, error envelopes, the long tool timeout. */
class DeckApiTest {
    private val server = MockWebServer()
    private val router = Router()

    @Before
    fun setUp() {
        server.dispatcher = router
        server.start()
    }

    @After
    fun tearDown() = server.close()

    @Test
    fun `deck reads carry the bearer token and decode`() = runBlocking<Unit> {
        router.json("GET", "/v1/deck/status", Fixtures.DECK_STATUS)
        router.json("GET", "/v1/deck/gps", Fixtures.DECK_GPS)
        router.json("GET", "/v1/deck/mesh", Fixtures.DECK_MESH)
        router.json("GET", "/v1/deck/sdr", Fixtures.DECK_SDR)
        router.json("GET", "/v1/deck/tools", Fixtures.DECK_TOOLS)
        router.json("GET", "/v1/deck/audit", Fixtures.DECK_AUDIT)
        val api = server.api("deck-token")
        assertEquals(78, api.deckStatus().battery!!.percent)
        assertEquals("3D", api.gps().fix!!.fix)
        assertEquals(3, api.mesh().nodes.size)
        assertEquals("synthetic", api.sdr().status)
        assertEquals(6, api.deckTools().size)
        assertEquals(2, api.audit(5).size)
        assertEquals("Bearer deck-token", router.take("GET /v1/deck/status").headers["Authorization"])
        assertEquals("5", router.take("GET /v1/deck/audit").url.queryParameter("limit"))
        router.log.forEach { key ->
            assertTrue("$key is under /v1/deck", key.contains(" /v1/deck/"))
        }
    }

    @Test
    fun `rail switch posts on and maps 409 rail_failed`() = runBlocking<Unit> {
        router.json("POST", "/v1/deck/rails/usb_hub", """{"ok": true, "rail": "usb_hub", "on": true}""")
        val api = server.api()
        val r = api.setRail("usb_hub", true)
        assertTrue(r.ok && r.on)
        assertEquals("""{"on":true}""", router.take("POST /v1/deck/rails/usb_hub").body!!.utf8())

        router.on("POST", "/v1/deck/rails/lte") { errorResponse(409, "rail_failed", "uconsole-4g-cm4 not found") }
        try {
            api.setRail("lte", true)
            fail("409 must throw")
        } catch (e: LapisApiException) {
            assertTrue(e.isRailFailed)
            assertEquals("uconsole-4g-cm4 not found", e.message)
        }
    }

    @Test
    fun `tools invoke posts tool args and context and waits longer than the gate`() = runBlocking<Unit> {
        router.json("POST", "/v1/deck/tools/invoke", Fixtures.INVOKE_MESH_OK)
        val api = server.api()
        val result = api.invokeTool(
            "mesh.send_message",
            buildJsonObject {
                put("channel", 0)
                put("text", "hi")
            },
        )
        assertTrue(result.ok)
        assertEquals(1, result.tier)
        assertEquals("approve_once", result.decision)
        assertEquals(
            """{"tool":"mesh.send_message","args":{"channel":0,"text":"hi"},"context":{"source":"android"}}""",
            router.take("POST /v1/deck/tools/invoke").body!!.utf8(),
        )
        // The gate waits up to 600 s for the owner; only this call reads that long.
        assertTrue(api.toolHttp.readTimeoutMillis >= 660_000)
        assertEquals(60_000, api.http.readTimeoutMillis)
        assertFalse(api.toolHttp.followRedirects)
        // Same interceptor chain (bearer token) as every other call.
        assertEquals(api.http.interceptors, api.toolHttp.interceptors)
    }

    @Test
    fun `tune posts the centre frequency and reports a refusal in the body`() = runBlocking<Unit> {
        router.json("POST", "/v1/deck/sdr/tune", """{"ok": false, "error": "center_hz is required"}""")
        val r = server.api().tune(162_400_000, gainDb = 20.0)
        assertFalse(r.ok)
        assertEquals("center_hz is required", r.error)
        assertEquals("""{"center_hz":162400000,"gain_db":20.0}""", router.take("POST /v1/deck/sdr/tune").body!!.utf8())
    }
}
