package dev.lapiszero.app.data.ble

import com.google.crypto.tink.subtle.Hkdf
import com.google.crypto.tink.subtle.X25519
import java.nio.ByteBuffer
import java.security.GeneralSecurityException
import java.security.MessageDigest
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Bad hello, wrong protocol version, invalid peer key, or an all-zero shared secret. */
class HandshakeException(message: String, cause: Throwable? = null) : Exception(message, cause)

/** Replay, out-of-order sequence, or a GCM authentication failure. */
class EnvelopeException(message: String, cause: Throwable? = null) : Exception(message, cause)

/**
 * lapis-ble-v1 layer 2 (BLE_PROVISIONING_PROTOCOL.md section 3): X25519 ECDH, HKDF-SHA256
 * with a salt that binds both public keys, both nonces and the 6-digit PIN, one AES-256-GCM
 * key per direction and a PIN-proof HMAC key.
 *
 * Must stay byte-for-byte identical to `daemon/src/lapis_daemon/ble_prov/crypto.py`; both are
 * tested against `tests/vectors/ble_crypto_vectors.json`.
 */
object BleEnvelopeCrypto {
    const val PROTO_VER: Byte = 0x01
    const val HELLO_LEN = 49 // proto_ver (1) || public key (32) || nonce (16)
    const val KEY_LEN = 32
    const val NONCE_LEN = 16
    const val DIR_C2D = 0x01 // Android -> device, key k_c2d, CMD_TX
    const val DIR_D2C = 0x02 // device -> Android, key k_d2c, EVT_RX
    const val MAX_SEQ = 0xFFFF_FFFFL

    private const val TAG_BITS = 128
    private const val TAG_LEN = 16
    private val INFO_C2D = "lapis-ble-v1-c2d".toByteArray(Charsets.US_ASCII)
    private val INFO_D2C = "lapis-ble-v1-d2c".toByteArray(Charsets.US_ASCII)
    private val INFO_PIN = "lapis-ble-v1-pin-confirm".toByteArray(Charsets.US_ASCII)
    private val CLIENT_PROOF_LABEL = "client-pin-proof".toByteArray(Charsets.US_ASCII)
    private val SERVER_PROOF_LABEL = "server-pin-proof".toByteArray(Charsets.US_ASCII)
    private val PIN_RE = Regex("^[0-9]{6}$")

    class Hello(val publicKey: ByteArray, val nonce: ByteArray)

    class SessionKeys(
        val kC2d: ByteArray,
        val kD2c: ByteArray,
        val kPin: ByteArray,
        val transcript: ByteArray,
    ) {
        fun wipe() {
            kC2d.fill(0)
            kD2c.fill(0)
            kPin.fill(0)
        }

        override fun toString(): String = "SessionKeys(redacted)"
    }

    fun isValidPin(pin: String): Boolean = PIN_RE.matches(pin)

    fun generatePrivateKey(): ByteArray = X25519.generatePrivateKey()

    fun publicKey(privateKey: ByteArray): ByteArray = try {
        X25519.publicFromPrivate(privateKey)
    } catch (e: GeneralSecurityException) {
        throw HandshakeException("invalid private key", e)
    }

    fun buildHello(publicKey: ByteArray, nonce: ByteArray): ByteArray {
        if (publicKey.size != KEY_LEN || nonce.size != NONCE_LEN) {
            throw HandshakeException("hello needs a 32-byte key and a 16-byte nonce")
        }
        return byteArrayOf(PROTO_VER) + publicKey + nonce
    }

    /** (public key, nonce) from a ClientHello / ServerHello. */
    fun parseHello(data: ByteArray): Hello {
        if (data.size != HELLO_LEN) throw HandshakeException("hello must be $HELLO_LEN bytes")
        if (data[0] != PROTO_VER) {
            throw HandshakeException("unsupported protocol version ${data[0].toInt() and 0xFF}")
        }
        return Hello(data.copyOfRange(1, 33), data.copyOfRange(33, 49))
    }

    /** X25519 shared secret; aborts on the all-zero output (RFC 7748 section 6.1). */
    fun computeShared(privateKey: ByteArray, peerPublicKey: ByteArray): ByteArray {
        if (privateKey.size != KEY_LEN || peerPublicKey.size != KEY_LEN) {
            throw HandshakeException("X25519 keys must be 32 bytes")
        }
        val shared = try {
            X25519.computeSharedSecret(privateKey, peerPublicKey)
        } catch (e: GeneralSecurityException) { // Tink refuses the known low-order points itself
            throw HandshakeException("invalid peer public key", e)
        }
        requireNonZero(shared)
        return shared
    }

    /** Constant-time check that the ECDH output is not all zero. */
    fun requireNonZero(shared: ByteArray) {
        var acc = 0
        for (b in shared) acc = acc or b.toInt()
        if (shared.size != KEY_LEN || acc == 0) throw HandshakeException("all-zero shared secret")
    }

    fun transcript(
        clientPub: ByteArray,
        devicePub: ByteArray,
        clientNonce: ByteArray,
        deviceNonce: ByteArray,
    ): ByteArray {
        if (clientPub.size != KEY_LEN || devicePub.size != KEY_LEN ||
            clientNonce.size != NONCE_LEN || deviceNonce.size != NONCE_LEN
        ) {
            throw HandshakeException("bad transcript input sizes")
        }
        return byteArrayOf(PROTO_VER) + clientPub + devicePub + clientNonce + deviceNonce // 97 bytes
    }

    /** salt = SHA256(transcript || ASCII(pin)). */
    fun salt(transcript: ByteArray, pin: String): ByteArray {
        if (!isValidPin(pin)) throw HandshakeException("PIN must be exactly 6 digits")
        return MessageDigest.getInstance("SHA-256").run {
            update(transcript)
            digest(pin.toByteArray(Charsets.US_ASCII))
        }
    }

    fun deriveKeys(
        shared: ByteArray,
        clientPub: ByteArray,
        devicePub: ByteArray,
        clientNonce: ByteArray,
        deviceNonce: ByteArray,
        pin: String,
    ): SessionKeys {
        requireNonZero(shared)
        val transcript = transcript(clientPub, devicePub, clientNonce, deviceNonce)
        val salt = salt(transcript, pin)
        fun expand(info: ByteArray): ByteArray =
            Hkdf.computeHkdf("HMACSHA256", shared, salt, info, KEY_LEN)
        return SessionKeys(expand(INFO_C2D), expand(INFO_D2C), expand(INFO_PIN), transcript)
    }

    fun clientProof(keys: SessionKeys): ByteArray = hmac(keys.kPin, CLIENT_PROOF_LABEL, keys.transcript)

    fun serverProof(keys: SessionKeys): ByteArray = hmac(keys.kPin, SERVER_PROOF_LABEL, keys.transcript)

    private fun hmac(key: ByteArray, label: ByteArray, transcript: ByteArray): ByteArray =
        Mac.getInstance("HmacSHA256").run {
            init(SecretKeySpec(key, "HmacSHA256"))
            update(label)
            doFinal(transcript)
        }

    fun constantTimeEquals(a: ByteArray, b: ByteArray): Boolean = MessageDigest.isEqual(a, b)

    /** nonce = dir_byte (1) || 0x00 * 7 || seq_be (4). */
    fun nonce(dirByte: Int, seq: Long): ByteArray =
        ByteBuffer.allocate(12).put(dirByte.toByte()).put(ByteArray(7)).putInt(seq.toInt()).array()

    /** aad = 0x01 (1) || dir_byte (1) || seq_be (4). */
    fun aad(dirByte: Int, seq: Long): ByteArray =
        ByteBuffer.allocate(6).put(PROTO_VER).put(dirByte.toByte()).putInt(seq.toInt()).array()

    /** `seq_be(4) || ciphertext || tag(16)`; the nonce is deterministic per (direction, seq). */
    fun seal(key: ByteArray, dirByte: Int, seq: Long, plaintext: ByteArray): ByteArray {
        if (seq < 1 || seq > MAX_SEQ) throw EnvelopeException("sequence counter exhausted")
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(TAG_BITS, nonce(dirByte, seq)),
        )
        cipher.updateAAD(aad(dirByte, seq))
        return ByteBuffer.allocate(4).putInt(seq.toInt()).array() + cipher.doFinal(plaintext)
    }

    /** Decrypts and authenticates; `seq` must be exactly the next expected one. */
    fun open(key: ByteArray, dirByte: Int, envelope: ByteArray, expectedSeq: Long): ByteArray {
        if (envelope.size < 4 + TAG_LEN) throw EnvelopeException("envelope too short")
        val seq = ByteBuffer.wrap(envelope, 0, 4).int.toLong() and MAX_SEQ
        if (seq == 0L || seq != expectedSeq) {
            throw EnvelopeException("unexpected sequence $seq (want $expectedSeq)")
        }
        return try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(
                Cipher.DECRYPT_MODE,
                SecretKeySpec(key, "AES"),
                GCMParameterSpec(TAG_BITS, nonce(dirByte, seq)),
            )
            cipher.updateAAD(aad(dirByte, seq))
            cipher.doFinal(envelope, 4, envelope.size - 4)
        } catch (e: AEADBadTagException) {
            throw EnvelopeException("authentication failed", e)
        } catch (e: GeneralSecurityException) {
            throw EnvelopeException("decryption failed", e)
        }
    }
}
