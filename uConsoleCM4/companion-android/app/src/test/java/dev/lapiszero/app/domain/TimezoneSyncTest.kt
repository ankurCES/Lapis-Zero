package dev.lapiszero.app.domain

import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.MemorySecureStore
import dev.lapiszero.app.Router
import dev.lapiszero.app.api
import dev.lapiszero.app.data.remote.DeviceState
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.errorResponse
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class TimezoneSyncTest {
    private val server = MockWebServer()
    private val router = Router()

    @Before
    fun setUp() {
        server.dispatcher = router
        server.start()
    }

    @After
    fun tearDown() = server.close()

    private fun state(timezone: String?): DeviceState {
        val json = if (timezone == null) Fixtures.stateSync() else
            Fixtures.stateSync().replace("\"mute_audio\": false}", "\"mute_audio\": false, \"timezone\": \"$timezone\"}")
        return LapisJson.decodeFromString(json)
    }

    private fun sync(zone: String = "America/Los_Angeles") = TimezoneSync(server.api()) { zone }

    @Test
    fun `sends the phone zone when the device has a different one`() = runBlocking<Unit> {
        router.json("PATCH", "/v1/device/ui", """{"theme": "paper", "brightness": 100, "mute_audio": false, "timezone": "America/Los_Angeles"}""")
        assertTrue(sync().syncIfNeeded(state("Europe/Berlin")))
        assertEquals("""{"timezone":"America/Los_Angeles"}""", router.take("PATCH /v1/device/ui").body!!.utf8())
    }

    @Test
    fun `sends it when the device zone is unset or the daemon predates the field`() = runBlocking<Unit> {
        router.json("PATCH", "/v1/device/ui", """{"theme": "paper", "brightness": 100, "mute_audio": false}""")
        assertTrue(sync().syncIfNeeded(state("")))
        assertTrue(sync().syncIfNeeded(state(null)))
        assertEquals(2, router.count("PATCH /v1/device/ui"))
    }

    @Test
    fun `does nothing when the zones already match`() = runBlocking<Unit> {
        assertTrue(sync().syncIfNeeded(state("America/Los_Angeles")))
        assertTrue(sync(zone = "").syncIfNeeded(state("Europe/Berlin")))
        assertEquals(0, router.count("PATCH /v1/device/ui"))
    }

    @Test
    fun `failures are swallowed`() = runBlocking<Unit> {
        // 422: the device does not know this zone. Done; asking again cannot help.
        router.on("PATCH", "/v1/device/ui") { errorResponse(422, "invalid", "Unknown timezone") }
        assertTrue(sync(zone = "Mars/Olympus_Mons").syncIfNeeded(state("")))
        // Unreachable: not done, so the next connection tries once more.
        server.close()
        assertFalse(sync().syncIfNeeded(state("")))
    }

    @Test
    fun `the hand-over is remembered per paired device`() {
        val store = MemorySecureStore()
        val repo = DeviceRepository(store)
        val device = repo.add("lapis-zero", Endpoint("lapis-zero.tailnet.ts.net", true), "token", "dev_a")
        assertFalse("a fresh pairing still owes the device its timezone", device.timezoneSynced)
        repo.markTimezoneSynced(device.id)
        assertTrue(repo.find(device.id)!!.timezoneSynced)
        val writes = store.writes
        repo.markTimezoneSynced(device.id)
        assertEquals("already marked: nothing rewritten", writes, store.writes)
        assertTrue(DeviceRepository(store).find(device.id)!!.timezoneSynced)
        // Pairing again (new token) starts over.
        assertFalse(repo.add("lapis-zero", Endpoint("lapis-zero.tailnet.ts.net", true), "token-2", "dev_b").timezoneSynced)
    }
}
