package dev.lapiszero.app.data.ble

import dev.lapiszero.app.data.ble.TestVectors.hex
import dev.lapiszero.app.data.ble.TestVectors.int
import dev.lapiszero.app.data.ble.TestVectors.strings
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ChunkFramingTest {
    private val v = TestVectors.framing

    private fun payload(size: Int): ByteArray = ByteArray(size) { (it * 31 + 7).toByte() }

    private fun reassemble(frames: List<ByteArray>, rx: Reassembler = Reassembler()): Reassembler.Result.Message {
        frames.dropLast(1).forEach { assertEquals(Reassembler.Result.Incomplete, rx.feed(it)) }
        return rx.feed(frames.last()) as Reassembler.Result.Message
    }

    @Test
    fun `d max follows the negotiated mtu`() {
        assertEquals(v.int("max_chunk_data"), ChunkFraming.maxChunkData(v.int("att_mtu")))
        assertEquals(16, ChunkFraming.maxChunkData(23))
        assertEquals(178, ChunkFraming.maxChunkData(185))
        assertEquals(510, ChunkFraming.maxChunkData(517))
    }

    @Test
    fun `vector frames are reproduced byte for byte`() {
        val frames = ChunkFraming.split(
            v.int("msg_id"), v.int("flags"), v.hex("payload_hex"), v.int("max_chunk_data"),
        )
        assertEquals(v.strings("expected_frames_hex"), frames.map { it.toHex() })
    }

    @Test
    fun `vector frames reassemble to the payload`() {
        val frames = v.strings("expected_frames_hex").map { it.hexToBytesOrNull()!! }
        val message = reassemble(frames)
        assertArrayEquals(v.hex("payload_hex"), message.payload)
        assertEquals(v.int("flags"), message.flags)
    }

    @Test
    fun `multi chunk split and reassembly at mtu 23`() {
        val data = payload(1000)
        val frames = ChunkFraming.split(200, ChunkFraming.FLAG_ENCRYPTED, data, ChunkFraming.maxChunkData(23))
        assertEquals(63, frames.size) // ceil(1000 / 16)
        assertTrue(frames.all { it.size <= 23 - 3 })
        frames.forEachIndexed { i, f ->
            assertEquals(200, f[0].toInt() and 0xFF)
            assertEquals(i, f[1].toInt() and 0xFF)
            assertEquals(63, f[2].toInt() and 0xFF)
            assertEquals(1, f[3].toInt())
        }
        assertEquals(4 + 1000 % 16, frames.last().size)
        assertArrayEquals(data, reassemble(frames).payload)
    }

    @Test
    fun `multi chunk split and reassembly at mtu 185`() {
        val data = payload(5000)
        val frames = ChunkFraming.split(255, ChunkFraming.FLAG_ENCRYPTED, data, ChunkFraming.maxChunkData(185))
        assertEquals(29, frames.size) // ceil(5000 / 178)
        assertTrue(frames.all { it.size <= 185 - 3 })
        assertEquals(182, frames.first().size)
        assertArrayEquals(data, reassemble(frames).payload)
    }

    @Test
    fun `a full 16 KiB message survives at mtu 185 and mtu 517`() {
        val data = payload(ChunkFraming.MAX_MESSAGE)
        for (mtu in listOf(185, 517)) {
            val frames = ChunkFraming.split(1, ChunkFraming.FLAG_ENCRYPTED, data, ChunkFraming.maxChunkData(mtu))
            assertArrayEquals("mtu $mtu", data, reassemble(frames).payload)
        }
    }

    @Test
    fun `single chunk message and msg id wrap`() {
        val frames = ChunkFraming.split(256 + 3, 0, byteArrayOf(9), 16)
        assertEquals(listOf("03000100" + "09"), frames.map { it.toHex() })
        assertArrayEquals(byteArrayOf(9), reassemble(frames).payload)
    }

    @Test
    fun `consecutive messages reuse one reassembler`() {
        val rx = Reassembler()
        val a = payload(40)
        val b = payload(41)
        assertArrayEquals(a, reassemble(ChunkFraming.split(0, 1, a, 16), rx).payload)
        assertArrayEquals(b, reassemble(ChunkFraming.split(1, 1, b, 16), rx).payload)
    }

    @Test
    fun `out of order chunk drops the buffer and asks for a reset`() {
        val frames = ChunkFraming.split(5, 1, payload(64), 16) // 4 chunks
        val rx = Reassembler()
        assertEquals(Reassembler.Result.Incomplete, rx.feed(frames[0]))
        assertTrue(rx.feed(frames[2]) is Reassembler.Result.Error) // skipped chunk 1
        // The buffer is gone: the next in-sequence chunk has nothing to attach to.
        assertTrue(rx.feed(frames[1]) is Reassembler.Result.Error)
        // A fresh message starting at chunk 0 works again.
        assertArrayEquals(payload(64), reassemble(frames, rx).payload)
    }

    @Test
    fun `duplicate chunk and mismatched header are rejected`() {
        val frames = ChunkFraming.split(5, 1, payload(64), 16)
        val rx = Reassembler()
        rx.feed(frames[0])
        assertTrue(rx.feed(frames[0].copyOf().also { it[1] = 1; it[2] = 9 }) is Reassembler.Result.Error) // total changed
        rx.feed(frames[0])
        assertTrue(rx.feed(frames[1].copyOf().also { it[0] = 6 }) is Reassembler.Result.Error) // msg_id changed
        rx.feed(frames[0])
        assertTrue(rx.feed(frames[1].copyOf().also { it[3] = 0 }) is Reassembler.Result.Error) // flags changed
        rx.feed(frames[0])
        rx.feed(frames[1])
        assertTrue(rx.feed(frames[1]) is Reassembler.Result.Error) // duplicate
    }

    @Test
    fun `invalid headers are rejected`() {
        val rx = Reassembler()
        assertTrue(rx.feed(byteArrayOf(1, 0, 1)) is Reassembler.Result.Error) // short
        assertTrue(rx.feed(byteArrayOf(1, 0, 1, 1)) is Reassembler.Result.Error) // no data
        assertTrue(rx.feed(byteArrayOf(1, 0, 0, 1, 7)) is Reassembler.Result.Error) // total = 0
        assertTrue(rx.feed(byteArrayOf(1, 2, 2, 1, 7)) is Reassembler.Result.Error) // idx >= total
        assertTrue(rx.feed(byteArrayOf(1, 0, 1, 0x02, 7)) is Reassembler.Result.Error) // reserved flag
        assertTrue(rx.feed(byteArrayOf(1, 1, 2, 1, 7)) is Reassembler.Result.Error) // starts mid-message
    }

    @Test
    fun `oversize message drops the buffer and asks for a reset`() {
        // 93 chunks of 178 bytes = 16554 bytes > 16384.
        val chunk = ByteArray(178) { 0x55 }
        val rx = Reassembler()
        var result: Reassembler.Result = Reassembler.Result.Incomplete
        var fed = 0
        for (idx in 0 until 93) {
            result = rx.feed(byteArrayOf(9, idx.toByte(), 93, 1) + chunk)
            fed++
            if (result !is Reassembler.Result.Incomplete) break
        }
        assertTrue(result is Reassembler.Result.Error)
        assertEquals(93, fed) // 92 * 178 = 16376 still fits; the 93rd chunk crosses the limit
        // Sender side refuses the same message outright.
        assertThrows(FrameException::class.java) {
            ChunkFraming.split(0, 1, ByteArray(ChunkFraming.MAX_MESSAGE + 1), 178)
        }
        assertThrows(FrameException::class.java) { ChunkFraming.split(0, 1, ByteArray(0), 16) }
        // 16 KiB does not fit in 255 chunks at the default MTU.
        assertThrows(FrameException::class.java) { ChunkFraming.split(0, 1, ByteArray(4081), 16) }
    }

    @Test
    fun `reset frame is four bytes and clears the peer buffer`() {
        assertEquals("00000180", ChunkFraming.resetFrame().toHex())
        assertEquals("2a000180", ChunkFraming.resetFrame(42).toHex())
        val frames = ChunkFraming.split(5, 1, payload(64), 16)
        val rx = Reassembler()
        rx.feed(frames[0])
        assertEquals(Reassembler.Result.PeerReset, rx.feed(ChunkFraming.resetFrame()))
        assertTrue(rx.feed(frames[1]) is Reassembler.Result.Error)
    }

    @Test
    fun `stale partial buffer is discarded after five seconds`() {
        var now = 0L
        val rx = Reassembler { now }
        val frames = ChunkFraming.split(5, 1, payload(64), 16)
        rx.feed(frames[0])
        now = 5_001
        // The continuation arrives too late: its buffer no longer exists.
        assertTrue(rx.feed(frames[1]) is Reassembler.Result.Error)
        now = 6_000
        assertArrayEquals(payload(64), reassemble(frames, rx).payload)
    }
}
