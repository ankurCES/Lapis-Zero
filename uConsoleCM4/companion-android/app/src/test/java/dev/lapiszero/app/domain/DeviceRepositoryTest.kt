package dev.lapiszero.app.domain

import dev.lapiszero.app.MemorySecureStore
import dev.lapiszero.app.data.ble.BleUuids
import dev.lapiszero.app.data.remote.DeviceKind
import dev.lapiszero.app.data.remote.Endpoint
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceRepositoryTest {
    private val store = MemorySecureStore()
    private val prod = Endpoint("lapis-zero.tailnet.ts.net", tls = true)
    private val second = Endpoint("lapis-two.tailnet.ts.net", tls = true)
    private val dev = Endpoint("127.0.0.1:8421", tls = false)

    @Test
    fun `starts empty and stores nothing`() {
        val repo = DeviceRepository(store)
        assertTrue(repo.state.value.devices.isEmpty())
        assertNull(repo.state.value.active)
        assertNull(store.blob)
    }

    @Test
    fun `more than one device can be paired and switched between`() {
        val repo = DeviceRepository(store)
        val a = repo.add("lapis-zero", prod, "token-a", "dev_a")
        val b = repo.add("lapis-two", second, "token-b", "dev_b")
        val c = repo.add("mock", dev, "dev-local-token", "dev_local")
        assertEquals(3, repo.state.value.usable.size)
        assertEquals("the newest pairing becomes active", c.id, repo.state.value.active?.id)

        repo.setActive(a.id)
        assertEquals("token-a", repo.state.value.active?.token)
        assertEquals("https://lapis-zero.tailnet.ts.net", repo.state.value.active?.endpoint?.baseUrl)
        repo.setActive(b.id)
        assertEquals("lapis-two", repo.state.value.active?.name)
        repo.setActive("no-such-id")
        assertEquals(b.id, repo.state.value.active?.id)
    }

    @Test
    fun `state survives a restart through the secure store only`() {
        val first = DeviceRepository(store)
        val a = first.add("lapis-zero", prod, "token-a", "dev_a")
        first.add("mock", dev, "dev-local-token", "dev_local")
        first.setActive(a.id)

        val reloaded = DeviceRepository(store)
        assertEquals(first.state.value, reloaded.state.value)
        assertEquals(a.id, reloaded.state.value.active?.id)
        assertEquals("token-a", reloaded.find(a.id)?.token)
        assertFalse(reloaded.find(a.id)!!.endpoint.host.isEmpty())
        // The blob in the vault is the one and only copy of host + token.
        assertTrue(store.blob!!.contains("token-a"))
        assertTrue(store.blob!!.contains("lapis-zero.tailnet.ts.net"))
    }

    @Test
    fun `pairing the same host again replaces the old token`() {
        val repo = DeviceRepository(store)
        val old = repo.add("lapis-zero", prod, "old-token", "dev_a")
        val new = repo.add("lapis-zero", prod, "new-token", "dev_b")
        assertEquals(1, repo.state.value.devices.size)
        assertNull(repo.find(old.id))
        assertEquals("new-token", repo.find(new.id)?.token)
        assertFalse(store.blob!!.contains("old-token"))
    }

    @Test
    fun `removing the active device falls back to another and the last removal clears the vault`() {
        val repo = DeviceRepository(store)
        val a = repo.add("lapis-zero", prod, "token-a", "dev_a")
        val b = repo.add("lapis-two", second, "token-b", "dev_b")
        repo.remove(b.id)
        assertEquals(a.id, repo.state.value.active?.id)
        assertFalse(store.blob!!.contains("token-b"))
        repo.remove(a.id)
        assertNull(repo.state.value.active)
        assertNull("nothing is left behind", store.blob)
    }

    @Test
    fun `ble provisioning keeps the token before the host is known`() {
        val repo = DeviceRepository(store)
        val existing = repo.add("mock", dev, "dev-local-token", "dev_local")
        val pending = repo.add("LapisZero-4F2A", null, "ble-token", "dev_01")
        assertFalse(pending.isProvisioned)
        assertTrue("token is persisted right after the PIN proof", store.blob!!.contains("ble-token"))
        assertEquals("a pending device is not usable yet", listOf(existing.id), repo.state.value.usable.map { it.id })
        assertEquals(existing.id, repo.state.value.active?.id)

        val done = repo.completeProvisioning(pending.id, prod, "lapis-zero")
        assertNotNull(done)
        assertEquals(pending.id, repo.state.value.active?.id)
        assertEquals("lapis-zero", repo.state.value.active?.name)
        assertEquals("ble-token", repo.state.value.active?.token)
        assertEquals("https://lapis-zero.tailnet.ts.net", repo.state.value.active?.endpoint?.baseUrl)
        assertNull(repo.completeProvisioning("missing", prod))
    }

    @Test
    fun `abandoned provisioning entries are purged`() {
        val repo = DeviceRepository(store)
        val kept = repo.add("mock", dev, "dev-local-token", "dev_local")
        repo.add("LapisZero-AAAA", null, "orphan-1", "dev_01")
        val current = repo.add("LapisZero-BBBB", null, "orphan-2", "dev_02")
        repo.purgeUnprovisioned(except = current.id)
        assertEquals(setOf(kept.id, current.id), repo.state.value.devices.map { it.id }.toSet())
        repo.purgeUnprovisioned()
        assertEquals(listOf(kept.id), repo.state.value.devices.map { it.id })
        assertFalse(store.blob!!.contains("orphan"))
    }

    @Test
    fun `rename keeps identity and token`() {
        val repo = DeviceRepository(store)
        val a = repo.add("lapis-zero", prod, "token-a", "dev_a")
        repo.rename(a.id, "  Desk unit ")
        assertEquals("Desk unit", repo.find(a.id)?.name)
        repo.rename(a.id, "   ")
        assertEquals("Desk unit", repo.find(a.id)?.name)
        assertEquals("token-a", repo.find(a.id)?.token)
    }

    @Test
    fun `tokens never show up in toString`() {
        val repo = DeviceRepository(store)
        val a = repo.add("lapis-zero", prod, "super-secret-token", "dev_a")
        assertFalse(a.toString().contains("super-secret-token"))
        assertFalse(repo.state.value.toString().contains("super-secret-token"))
    }

    @Test
    fun `a corrupt vault is treated as empty instead of crashing`() {
        val repo = DeviceRepository(MemorySecureStore("{not json"))
        assertTrue(repo.state.value.devices.isEmpty())
    }

    @Test(expected = IllegalArgumentException::class)
    fun `an empty token is refused`() {
        DeviceRepository(store).add("x", prod, "", "dev")
    }

    // ------------------------------------------------------------------ Lapis-Deck

    @Test
    fun `a vault written by the lapis-zero app keeps working, with no kind yet`() {
        // Exactly what the installed app stores: no `kind` key anywhere.
        val old = """{"devices":[{"id":"a1","name":"lapis-zero","host":"lapis-zero.tailnet.ts.net","tls":true,
            "token":"token-a","remote_id":"dev_a","added_at":1795541234,"timezone_synced":true}],"active_id":"a1"}"""
        val repo = DeviceRepository(MemorySecureStore(old))
        val device = repo.state.value.active!!
        assertEquals("token-a", device.token)
        assertEquals("", device.kind)
        assertFalse(device.isDeck)
        assertTrue(device.timezoneSynced)
    }

    @Test
    fun `the kind is stored with the pairing and updated only when it changes`() {
        val repo = DeviceRepository(store)
        val deck = repo.add("lapis-deck", prod, "token-d", "dev_d", kind = DeviceKind.DECK)
        assertTrue(repo.find(deck.id)!!.isDeck)
        assertTrue(DeviceRepository(store).find(deck.id)!!.isDeck)

        val writes = store.writes
        repo.setKind(deck.id, DeviceKind.DECK) // the same: no Keystore round trip
        repo.setKind(deck.id, "") // nothing known: ignored
        repo.setKind("no-such-id", DeviceKind.ZERO)
        assertEquals(writes, store.writes)

        repo.setKind(deck.id, DeviceKind.ZERO) // reflashed as a Lapis-Zero
        assertFalse(DeviceRepository(store).find(deck.id)!!.isDeck)
        assertEquals(writes + 1, store.writes)
    }

    @Test
    fun `bluetooth provisioning keeps the kind from the advertised name`() {
        val repo = DeviceRepository(store)
        val pending = repo.add("LapisDeck-7F3A", null, "ble-token", "dev_01", kind = BleUuids.kindOf("LapisDeck-7F3A"))
        val done = repo.completeProvisioning(pending.id, Endpoint("lapis-deck.tailnet.ts.net", tls = true), "lapis-deck")!!
        assertEquals("lapis-deck", done.name)
        assertTrue(done.isDeck)
    }
}
