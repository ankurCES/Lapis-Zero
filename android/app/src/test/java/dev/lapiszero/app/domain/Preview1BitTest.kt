package dev.lapiszero.app.domain

import java.util.Base64
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class Preview1BitTest {
    private fun packed(set: (x: Int, y: Int) -> Boolean): ByteArray {
        // Reference packer, written independently of the decoder: MSB-first, 15 bytes per row.
        val out = ByteArray(1020)
        for (y in 0 until 68) for (x in 0 until 120) {
            if (set(x, y)) out[y * 15 + x / 8] = (out[y * 15 + x / 8].toInt() or (0x80 ushr (x % 8))).toByte()
        }
        return out
    }

    @Test
    fun `geometry constants`() {
        assertEquals(120, Preview1Bit.WIDTH)
        assertEquals(68, Preview1Bit.HEIGHT)
        assertEquals(15, Preview1Bit.ROW_BYTES)
        assertEquals(1020, Preview1Bit.SIZE)
    }

    @Test
    fun `bits are msb first within each byte`() {
        val bytes = ByteArray(1020)
        bytes[0] = 0x80.toByte() // pixel (0, 0)
        bytes[1] = 0x01 // pixel (15, 0)
        bytes[15] = 0x40 // pixel (1, 1): second row starts at byte 15
        bytes[1019] = 0x01 // pixel (119, 67)
        val bits = Preview1Bit.decode(bytes)
        assertEquals(120 * 68, bits.size)
        val lit = bits.indices.filter { bits[it] }.map { it % 120 to it / 120 }
        assertEquals(listOf(0 to 0, 15 to 0, 1 to 1, 119 to 67), lit)
    }

    @Test
    fun `every pixel position round trips`() {
        val pattern = { x: Int, y: Int -> (x * 7 + y * 13) % 5 == 0 || x == y }
        val bits = Preview1Bit.decode(packed(pattern))
        for (y in 0 until 68) for (x in 0 until 120) {
            assertEquals("pixel ($x, $y)", pattern(x, y), bits[y * 120 + x])
        }
    }

    @Test
    fun `rows do not bleed into each other`() {
        val bits = Preview1Bit.decode(packed { _, y -> y == 33 })
        for (y in 0 until 68) for (x in 0 until 120) assertEquals(y == 33, bits[y * 120 + x])
    }

    @Test
    fun `base64 decoding and rejection of wrong sizes`() {
        val all = Base64.getEncoder().encodeToString(ByteArray(1020) { 0xFF.toByte() })
        assertEquals(1360, all.length) // what the daemon sends
        assertTrue(Preview1Bit.decodeBase64(all)!!.all { it })
        assertFalse(Preview1Bit.decodeBase64(Base64.getEncoder().encodeToString(ByteArray(1020)))!!.any { it })

        assertNull(Preview1Bit.decodeBase64(null))
        assertNull(Preview1Bit.decodeBase64(""))
        assertNull(Preview1Bit.decodeBase64("not base64 !!"))
        assertNull(Preview1Bit.decodeBase64(Base64.getEncoder().encodeToString(ByteArray(1019))))
        assertNull(Preview1Bit.decodeBase64(Base64.getEncoder().encodeToString(ByteArray(1021))))
        assertThrows(IllegalArgumentException::class.java) { Preview1Bit.decode(ByteArray(100)) }
    }

    @Test
    fun `argb mapping keeps set bits bright`() {
        val bits = Preview1Bit.decode(packed { x, y -> x == 0 && y == 0 })
        val paper = 0xFFF2F2EC.toInt()
        val ink = 0xFF0B0B0D.toInt()
        val argb = Preview1Bit.toArgb(bits, setColor = paper, clearColor = ink)
        assertEquals(120 * 68, argb.size)
        assertEquals(paper, argb[0])
        assertArrayEquals(IntArray(119) { ink }, argb.copyOfRange(1, 120))
        assertNotNull(argb)
    }
}
