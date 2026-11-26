package dev.lapiszero.app.domain

import java.util.Base64

/**
 * The approval preview: 120x68 pixels, 1 bit each, MSB-first, 15 bytes per row, 1020 bytes
 * in total (spec 5.4). A set bit is a bright pixel (the daemon thresholds luma at 50%).
 */
object Preview1Bit {
    const val WIDTH = 120
    const val HEIGHT = 68
    const val ROW_BYTES = 15
    const val SIZE = ROW_BYTES * HEIGHT // 1020

    /** Null when the field is absent or is not exactly 1020 bytes of base64. */
    fun decodeBase64(b64: String?): BooleanArray? {
        if (b64.isNullOrEmpty()) return null
        val bytes = try {
            Base64.getDecoder().decode(b64)
        } catch (e: IllegalArgumentException) {
            return null
        }
        return if (bytes.size == SIZE) decode(bytes) else null
    }

    /** Row-major pixels; `true` = bit set. */
    fun decode(packed: ByteArray): BooleanArray {
        require(packed.size == SIZE) { "expected $SIZE bytes, got ${packed.size}" }
        val out = BooleanArray(WIDTH * HEIGHT)
        for (y in 0 until HEIGHT) {
            for (x in 0 until WIDTH) {
                val byte = packed[y * ROW_BYTES + x / 8].toInt() and 0xFF
                out[y * WIDTH + x] = (byte shr (7 - x % 8)) and 1 == 1
            }
        }
        return out
    }

    /** ARGB pixels for a 120x68 bitmap; scale it with nearest-neighbour to keep it crisp. */
    fun toArgb(bits: BooleanArray, setColor: Int, clearColor: Int): IntArray =
        IntArray(bits.size) { if (bits[it]) setColor else clearColor }
}
