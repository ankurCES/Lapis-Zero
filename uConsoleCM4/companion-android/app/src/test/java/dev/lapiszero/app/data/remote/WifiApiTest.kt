package dev.lapiszero.app.data.remote

import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.errorResponse
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/** `/v1/wifi*`: models, paths, bodies and error envelopes, as both daemons speak them. */
class WifiApiTest {
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
    fun `wifi state scan and forget decode and carry the bearer token`() = runBlocking<Unit> {
        router.json(
            "GET", "/v1/wifi",
            """{"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi": -58,
                "saved": [{"name": "HomeWiFi", "active": true}, {"name": "Office", "active": false}],
                "ble": {"open": false, "expires_at": 0}, "future_field": 1}""",
        )
        router.json(
            "GET", "/v1/wifi/scan",
            """{"networks": [{"ssid": "HomeWiFi", "rssi": -48, "secured": true}, {"ssid": "café guest", "rssi": -71, "secured": false}]}""",
        )
        router.json("POST", "/v1/wifi/forget", """{"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi": null, "saved": [{"name": "HomeWiFi", "active": true}], "ble": {"open": false, "expires_at": 0}}""")
        router.json("POST", "/v1/wifi/ble", """{"ssid": "HomeWiFi", "ip": "192.168.1.42", "rssi": -58, "saved": [], "ble": {"open": true, "expires_at": 1795654057}}""")
        val api = server.api("wifi-token")

        val info = api.wifi()
        assertEquals("HomeWiFi", info.ssid)
        assertEquals(-58, info.rssi)
        assertEquals(listOf(WifiSaved("HomeWiFi", true), WifiSaved("Office", false)), info.saved)
        assertFalse(info.ble.open)
        assertEquals("Bearer wifi-token", router.take("GET /v1/wifi").headers["Authorization"])

        val networks = api.wifiScan()
        assertEquals(listOf("HomeWiFi", "café guest"), networks.map { it.ssid })
        assertFalse(networks[1].secured)

        val after = api.wifiForget("Office")
        assertNull(after.rssi)
        assertEquals("""{"name":"Office"}""", router.take("POST /v1/wifi/forget").body!!.utf8())

        val window = api.openWifiBle()
        assertTrue(window.ble.open)
        assertEquals(1795654057, window.ble.expiresAt)
    }

    @Test
    fun `connect sends the ssid and psk, and an explicit null psk for an open network`() = runBlocking<Unit> {
        router.json("POST", "/v1/wifi/connect", """{"state": "connected", "local_ip": "10.0.0.7"}""")
        val api = server.api()
        assertEquals(WifiConnectResult("connected", "10.0.0.7"), api.wifiConnect("Office", "correct horse"))
        val secured = LapisJson.parseToJsonElement(router.take("POST /v1/wifi/connect").body!!.utf8()).jsonObject
        assertEquals("Office", secured.getValue("ssid").jsonPrimitive.content)
        assertEquals("correct horse", secured.getValue("psk").jsonPrimitive.content)

        api.wifiConnect("café guest", null)
        assertEquals("""{"ssid":"café guest","psk":null}""", router.take("POST /v1/wifi/connect").body!!.utf8())
        assertEquals(JsonNull, LapisJson.parseToJsonElement("""{"psk":null}""").jsonObject["psk"])

        // The long wait is for this call only, and it is never resent.
        assertEquals(LapisApiClient.WIFI_READ_TIMEOUT_S * 1000, api.wifiHttp.readTimeoutMillis.toLong())
        assertFalse(api.wifiHttp.retryOnConnectionFailure)
        assertEquals(TimeUnit.SECONDS.toMillis(60), api.http.readTimeoutMillis.toLong())
    }

    @Test
    fun `daemon errors keep their codes`() = runBlocking<Unit> {
        router.on("GET", "/v1/wifi/scan") { errorResponse(503, "wifi_unavailable", "no Wi-Fi radio") }
        router.on("POST", "/v1/wifi/connect") { errorResponse(422, "invalid", "psk must be 8..63 printable ASCII or 64 hex") }
        router.on("POST", "/v1/wifi/forget") { errorResponse(404, "not_found", "no such profile") }
        router.on("POST", "/v1/wifi/ble") { errorResponse(409, "not_paired", "no phone is paired") }
        val api = server.api()
        suspend fun code(block: suspend () -> Unit): String = try {
            block()
            fail("must throw")
            ""
        } catch (e: LapisApiException) {
            "${e.status} ${e.code}"
        }
        assertEquals("503 wifi_unavailable", code { api.wifiScan() })
        assertEquals("422 invalid", code { api.wifiConnect("x", "short") })
        assertEquals("404 not_found", code { api.wifiForget("nope") })
        assertEquals("409 not_paired", code { api.openWifiBle() })
    }

    @Test
    fun `psk and ssid rules match the daemons`() {
        assertTrue(WifiRules.isValidPsk("12345678"))
        assertTrue(WifiRules.isValidPsk("correct horse battery staple"))
        assertTrue(WifiRules.isValidPsk("a".repeat(63)))
        assertTrue(WifiRules.isValidPsk("0123456789abcdefABCDEF".padEnd(64, '0')))
        assertFalse(WifiRules.isValidPsk("1234567"))
        assertFalse(WifiRules.isValidPsk("g".repeat(64)))
        assertFalse(WifiRules.isValidPsk("a".repeat(65)))
        assertFalse(WifiRules.isValidPsk("tab\there!"))
        assertFalse(WifiRules.isValidPsk("pässwört123"))
        assertTrue(WifiRules.isValidSsid("café guest"))
        assertFalse(WifiRules.isValidSsid(""))
        assertFalse(WifiRules.isValidSsid("é".repeat(17))) // 34 bytes
    }
}
