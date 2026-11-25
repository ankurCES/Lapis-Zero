package dev.lapiszero.app.data.ble

import dev.lapiszero.app.data.ble.TestVectors.hex
import dev.lapiszero.app.data.ble.TestVectors.int
import dev.lapiszero.app.data.ble.TestVectors.str
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Byte-for-byte parity with `tests/vectors/ble_crypto_vectors.json`, the same file the Python
 * side (`daemon/tests/test_ble_crypto_vectors.py`) checks itself against.
 */
class BleEnvelopeCryptoVectorTest {
    private val v = TestVectors.crypto
    private val d = TestVectors.derived

    private val clientPriv = v.hex("client_priv_hex")
    private val devicePriv = v.hex("device_priv_hex")
    private val clientPub = v.hex("client_pub_hex")
    private val devicePub = v.hex("device_pub_hex")
    private val clientNonce = v.hex("client_nonce_hex")
    private val deviceNonce = v.hex("device_nonce_hex")
    private val pin = v.str("pin")
    private val seq = v.int("seq").toLong()
    private val dir = v.int("dir_byte")
    private val plaintext = v.str("plaintext_utf8").toByteArray(Charsets.UTF_8)

    private fun keys(pin: String = this.pin): BleEnvelopeCrypto.SessionKeys {
        val shared = BleEnvelopeCrypto.computeShared(clientPriv, devicePub)
        return BleEnvelopeCrypto.deriveKeys(shared, clientPub, devicePub, clientNonce, deviceNonce, pin)
    }

    @Test
    fun `vector file is the frozen version 1`() {
        assertEquals(1, TestVectors.root.int("version"))
        assertEquals(1, v.int("proto_ver"))
        assertEquals(BleEnvelopeCrypto.PROTO_VER.toInt(), v.int("proto_ver"))
    }

    @Test
    fun `public keys derive from the private keys`() {
        assertArrayEquals(clientPub, BleEnvelopeCrypto.publicKey(clientPriv))
        assertArrayEquals(devicePub, BleEnvelopeCrypto.publicKey(devicePriv))
    }

    @Test
    fun `shared secret matches from both sides`() {
        val expected = v.hex("shared_secret_hex")
        assertArrayEquals(expected, BleEnvelopeCrypto.computeShared(clientPriv, devicePub))
        assertArrayEquals(expected, BleEnvelopeCrypto.computeShared(devicePriv, clientPub))
    }

    @Test
    fun `salt binds transcript and pin`() {
        val transcript = BleEnvelopeCrypto.transcript(clientPub, devicePub, clientNonce, deviceNonce)
        assertEquals(97, transcript.size)
        assertArrayEquals(d.hex("salt_hex"), BleEnvelopeCrypto.salt(transcript, pin))
    }

    @Test
    fun `hkdf produces the three session keys`() {
        val keys = keys()
        assertArrayEquals(d.hex("k_c2d_hex"), keys.kC2d)
        assertArrayEquals(d.hex("k_d2c_hex"), keys.kD2c)
        assertArrayEquals(d.hex("k_pin_hex"), keys.kPin)
    }

    @Test
    fun `pin proofs match in both directions`() {
        val keys = keys()
        assertArrayEquals(d.hex("client_proof_hex"), BleEnvelopeCrypto.clientProof(keys))
        assertArrayEquals(d.hex("server_proof_hex"), BleEnvelopeCrypto.serverProof(keys))
        assertEquals(d.str("client_proof_hex"), BleEnvelopeCrypto.clientProof(keys).toHex())
    }

    @Test
    fun `nonce and aad layout`() {
        assertArrayEquals(d.hex("nonce_hex"), BleEnvelopeCrypto.nonce(dir, seq))
        assertArrayEquals(d.hex("aad_hex"), BleEnvelopeCrypto.aad(dir, seq))
        // dir_byte || 7 zero bytes || seq_be, also for a large counter.
        assertEquals("02000000000000000100ffff", BleEnvelopeCrypto.nonce(2, 0x0100FFFFL).toHex())
        assertEquals("0102ffffffff", BleEnvelopeCrypto.aad(2, 0xFFFFFFFFL).toHex())
    }

    @Test
    fun `envelope is byte identical to the python reference`() {
        val envelope = BleEnvelopeCrypto.seal(keys().kC2d, dir, seq, plaintext)
        assertEquals(d.str("envelope_hex"), envelope.toHex())
        assertEquals(4 + plaintext.size + 16, envelope.size)
    }

    @Test
    fun `reference envelope decrypts to the plaintext`() {
        val opened = BleEnvelopeCrypto.open(keys().kC2d, dir, d.hex("envelope_hex"), seq)
        assertEquals(v.str("plaintext_utf8"), opened.toString(Charsets.UTF_8))
    }

    @Test
    fun `reference envelope splits into the reference frames shape`() {
        // The crypto envelope travels through layer 1 unchanged.
        val envelope = d.hex("envelope_hex")
        val frames = ChunkFraming.split(0, ChunkFraming.FLAG_ENCRYPTED, envelope, ChunkFraming.maxChunkData(23))
        val rx = Reassembler()
        val results = frames.map { rx.feed(it) }
        val message = results.last() as Reassembler.Result.Message
        assertArrayEquals(envelope, message.payload)
        assertEquals(ChunkFraming.FLAG_ENCRYPTED, message.flags)
    }

    @Test
    fun `round trip in both directions with independent keys`() {
        val keys = keys()
        val event = """{"evt":"wifi_list","networks":[{"ssid":"café","rssi":-58,"secured":true}]}"""
            .toByteArray(Charsets.UTF_8)
        val c2d = BleEnvelopeCrypto.seal(keys.kC2d, BleEnvelopeCrypto.DIR_C2D, 2, plaintext)
        val d2c = BleEnvelopeCrypto.seal(keys.kD2c, BleEnvelopeCrypto.DIR_D2C, 2, event)
        assertArrayEquals(plaintext, BleEnvelopeCrypto.open(keys.kC2d, BleEnvelopeCrypto.DIR_C2D, c2d, 2))
        assertArrayEquals(event, BleEnvelopeCrypto.open(keys.kD2c, BleEnvelopeCrypto.DIR_D2C, d2c, 2))
        // Directional keys and the direction byte both separate the two streams.
        assertThrows(EnvelopeException::class.java) {
            BleEnvelopeCrypto.open(keys.kD2c, BleEnvelopeCrypto.DIR_C2D, c2d, 2)
        }
        assertThrows(EnvelopeException::class.java) {
            BleEnvelopeCrypto.open(keys.kC2d, BleEnvelopeCrypto.DIR_D2C, c2d, 2)
        }
    }

    @Test
    fun `replayed and out of order sequence numbers are rejected`() {
        val key = keys().kC2d
        val first = BleEnvelopeCrypto.seal(key, dir, 1, plaintext)
        val second = BleEnvelopeCrypto.seal(key, dir, 2, plaintext)
        val third = BleEnvelopeCrypto.seal(key, dir, 3, plaintext)
        var expected = 1L
        BleEnvelopeCrypto.open(key, dir, first, expected++)
        // Replay of seq 1 while 2 is expected.
        assertThrows(EnvelopeException::class.java) { BleEnvelopeCrypto.open(key, dir, first, expected) }
        // Skipping ahead to seq 3 while 2 is expected.
        assertThrows(EnvelopeException::class.java) { BleEnvelopeCrypto.open(key, dir, third, expected) }
        BleEnvelopeCrypto.open(key, dir, second, expected++)
        BleEnvelopeCrypto.open(key, dir, third, expected)
    }

    @Test
    fun `sequence zero and wrap around are rejected`() {
        val key = keys().kC2d
        assertThrows(EnvelopeException::class.java) { BleEnvelopeCrypto.seal(key, dir, 0, plaintext) }
        assertThrows(EnvelopeException::class.java) {
            BleEnvelopeCrypto.seal(key, dir, BleEnvelopeCrypto.MAX_SEQ + 1, plaintext)
        }
        val forged = ByteArray(4) + BleEnvelopeCrypto.seal(key, dir, 1, plaintext).copyOfRange(4, 39)
        assertThrows(EnvelopeException::class.java) { BleEnvelopeCrypto.open(key, dir, forged, 0) }
        assertThrows(EnvelopeException::class.java) { BleEnvelopeCrypto.open(key, dir, ByteArray(19), 1) }
    }

    @Test
    fun `tampering with tag ciphertext or sequence fails authentication`() {
        val key = keys().kC2d
        val envelope = d.hex("envelope_hex")
        for (index in listOf(envelope.size - 1, envelope.size - 16, 4, 10)) {
            val bad = envelope.copyOf().also { it[index] = (it[index].toInt() xor 0x01).toByte() }
            assertThrows("byte $index", EnvelopeException::class.java) {
                BleEnvelopeCrypto.open(key, dir, bad, seq)
            }
        }
        // Rewriting the sequence header (to slip past a replay check) breaks nonce and AAD.
        val renumbered = envelope.copyOf().also { it[3] = 2 }
        assertThrows(EnvelopeException::class.java) { BleEnvelopeCrypto.open(key, dir, renumbered, 2) }
        // Truncated tag.
        assertThrows(EnvelopeException::class.java) {
            BleEnvelopeCrypto.open(key, dir, envelope.copyOf(envelope.size - 1), seq)
        }
    }

    @Test
    fun `a wrong pin derives keys that cannot open the envelope`() {
        val wrong = keys(pin = "482918")
        assertFalse(wrong.kC2d.contentEquals(d.hex("k_c2d_hex")))
        assertFalse(BleEnvelopeCrypto.clientProof(wrong).contentEquals(d.hex("client_proof_hex")))
        assertThrows(EnvelopeException::class.java) {
            BleEnvelopeCrypto.open(wrong.kC2d, dir, d.hex("envelope_hex"), seq)
        }
    }

    @Test
    fun `all zero shared secret aborts the handshake`() {
        // Low-order points force X25519 to the all-zero output (RFC 7748 section 6.1).
        val lowOrder = listOf(
            ByteArray(32),
            ByteArray(32).also { it[0] = 1 },
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800".hexToBytesOrNull()!!,
        )
        for (point in lowOrder) {
            assertThrows(HandshakeException::class.java) { BleEnvelopeCrypto.computeShared(clientPriv, point) }
        }
        assertThrows(HandshakeException::class.java) { BleEnvelopeCrypto.requireNonZero(ByteArray(32)) }
        assertThrows(HandshakeException::class.java) {
            BleEnvelopeCrypto.deriveKeys(ByteArray(32), clientPub, devicePub, clientNonce, deviceNonce, pin)
        }
        BleEnvelopeCrypto.requireNonZero(v.hex("shared_secret_hex")) // a real secret passes
    }

    @Test
    fun `hello is 49 bytes and rejects other versions and sizes`() {
        val hello = BleEnvelopeCrypto.buildHello(clientPub, clientNonce)
        assertEquals(49, hello.size)
        assertEquals("01" + v.str("client_pub_hex") + v.str("client_nonce_hex"), hello.toHex())
        val parsed = BleEnvelopeCrypto.parseHello(hello)
        assertArrayEquals(clientPub, parsed.publicKey)
        assertArrayEquals(clientNonce, parsed.nonce)
        assertThrows(HandshakeException::class.java) {
            BleEnvelopeCrypto.parseHello(hello.copyOf().also { it[0] = 2 })
        }
        assertThrows(HandshakeException::class.java) { BleEnvelopeCrypto.parseHello(hello.copyOf(48)) }
        assertThrows(HandshakeException::class.java) { BleEnvelopeCrypto.buildHello(clientPub, ByteArray(15)) }
    }

    @Test
    fun `pin must be six ascii digits`() {
        assertTrue(BleEnvelopeCrypto.isValidPin("000000"))
        for (bad in listOf("", "12345", "1234567", "12345a", "12 456", "١٢٣٤٥٦")) {
            assertFalse(bad, BleEnvelopeCrypto.isValidPin(bad))
            assertThrows(HandshakeException::class.java) {
                BleEnvelopeCrypto.deriveKeys(
                    v.hex("shared_secret_hex"), clientPub, devicePub, clientNonce, deviceNonce, bad,
                )
            }
        }
    }

    @Test
    fun `fresh keypairs agree on a secret`() {
        val a = BleEnvelopeCrypto.generatePrivateKey()
        val b = BleEnvelopeCrypto.generatePrivateKey()
        assertArrayEquals(
            BleEnvelopeCrypto.computeShared(a, BleEnvelopeCrypto.publicKey(b)),
            BleEnvelopeCrypto.computeShared(b, BleEnvelopeCrypto.publicKey(a)),
        )
    }
}
