package dev.lapiszero.app.data.ble

/**
 * lapis-ble-v1 layer 1: the 4-byte chunk header `msg_id | chunk_idx | total_chunks | flags`
 * (BLE_PROVISIONING_PROTOCOL.md section 2). Mirrors `daemon/.../ble_prov/crypto.py`.
 */
object ChunkFraming {
    const val HEADER_LEN = 4
    const val FLAG_ENCRYPTED = 0x01
    const val FLAG_RESET = 0x80
    const val MAX_MESSAGE = 16_384
    const val DEFAULT_ATT_MTU = 23
    const val REASSEMBLY_TIMEOUT_MS = 5_000L

    /**
     * The Bluetooth Core Specification caps an attribute value at 512 bytes, whatever the MTU.
     * At the MTU this protocol asks for (517) the formula below allows 514-byte frames, which
     * Android refuses to write, so the phone never sends a frame longer than this. Shorter
     * chunks are valid on the wire: the protocol allows 1..D_max data bytes per frame.
     */
    const val MAX_ATT_VALUE = 512

    /** D_max = ATT_MTU - 7 (3 bytes of ATT header plus the 4-byte chunk header). */
    fun maxChunkData(attMtu: Int): Int = maxOf(1, attMtu - 3 - HEADER_LEN)

    /** Chunk size for frames this phone writes: D_max, limited so a frame fits one attribute. */
    fun writeChunkData(attMtu: Int): Int = minOf(maxChunkData(attMtu), MAX_ATT_VALUE - HEADER_LEN)

    /** Splits one logical message into GATT-sized frames. */
    fun split(msgId: Int, flags: Int, payload: ByteArray, chunk: Int): List<ByteArray> {
        if (payload.isEmpty()) throw FrameException("empty payload")
        if (payload.size > MAX_MESSAGE) throw FrameException("message exceeds 16 KiB")
        require(chunk >= 1) { "chunk size must be positive" }
        val total = (payload.size + chunk - 1) / chunk
        if (total > 255) throw FrameException("message needs more than 255 chunks at this MTU")
        return List(total) { idx ->
            val from = idx * chunk
            val to = minOf(payload.size, from + chunk)
            ByteArray(HEADER_LEN + (to - from)).also { frame ->
                frame[0] = (msgId and 0xFF).toByte()
                frame[1] = idx.toByte()
                frame[2] = total.toByte()
                frame[3] = flags.toByte()
                payload.copyInto(frame, HEADER_LEN, from, to)
            }
        }
    }

    /** Transport abort: `msg_id 00 01 80`, no body. The peer drops its partial buffer. */
    fun resetFrame(msgId: Int = 0): ByteArray =
        byteArrayOf((msgId and 0xFF).toByte(), 0, 1, FLAG_RESET.toByte())
}

class FrameException(message: String) : Exception(message)

/** Rebuilds logical messages from in-order chunks; one message at a time. */
class Reassembler(private val nowMs: () -> Long = { System.nanoTime() / 1_000_000 }) {
    sealed interface Result {
        /** More chunks are needed. */
        data object Incomplete : Result

        /** A whole message. */
        class Message(val flags: Int, val payload: ByteArray) : Result

        /** The peer sent a reset frame; the partial buffer is gone. */
        data object PeerReset : Result

        /** Malformed, out-of-order or oversized: buffer dropped, answer with ONE reset frame. */
        data class Error(val reason: String) : Result
    }

    private var msgId = -1
    private var total = 0
    private var flags = 0
    private var next = 0
    private var startedAt = 0L
    private var buffer = java.io.ByteArrayOutputStream()

    fun reset() {
        msgId = -1
        next = 0
        buffer = java.io.ByteArrayOutputStream()
    }

    fun feed(frame: ByteArray): Result {
        val now = nowMs()
        if (msgId >= 0 && now - startedAt > ChunkFraming.REASSEMBLY_TIMEOUT_MS) reset()
        if (frame.size < ChunkFraming.HEADER_LEN) return fail("short frame")
        val id = frame[0].toInt() and 0xFF
        val idx = frame[1].toInt() and 0xFF
        val count = frame[2].toInt() and 0xFF
        val frameFlags = frame[3].toInt() and 0xFF
        val dataLen = frame.size - ChunkFraming.HEADER_LEN
        if (frameFlags and ChunkFraming.FLAG_RESET != 0) {
            reset()
            return Result.PeerReset
        }
        val reserved = frameFlags and ChunkFraming.FLAG_ENCRYPTED.inv()
        if (count < 1 || idx >= count || dataLen == 0 || reserved != 0) {
            return fail("invalid chunk header")
        }
        if (idx == 0) {
            reset()
            msgId = id
            total = count
            flags = frameFlags
            startedAt = now
        } else if (msgId != id || total != count || flags != frameFlags || idx != next) {
            return fail("chunk out of order")
        }
        buffer.write(frame, ChunkFraming.HEADER_LEN, dataLen)
        next = idx + 1
        if (buffer.size() > ChunkFraming.MAX_MESSAGE) return fail("message exceeds 16 KiB")
        if (next < total) return Result.Incomplete
        val message = Result.Message(flags, buffer.toByteArray())
        reset()
        return message
    }

    private fun fail(reason: String): Result {
        reset()
        return Result.Error(reason)
    }
}
