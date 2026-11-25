package dev.lapiszero.app.data.ble

import java.security.SecureRandom
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.onSubscription
import kotlinx.coroutines.flow.transformWhile
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/** What the GATT link has to offer; implemented by the Nordic BLE manager and by test fakes. */
interface BleTransport {
    /** Negotiated ATT MTU (23 until the exchange completes). */
    val attMtu: Int

    suspend fun writeHandshake(clientHello: ByteArray)

    suspend fun readHandshake(): ByteArray

    /** One GATT write on CMD_TX: at most `attMtu - 3` bytes and never more than 512. */
    suspend fun writeCommand(frame: ByteArray)

    /** EVT_RX notifications, one frame each. Completes (or fails) when the link drops. */
    val notifications: Flow<ByteArray>
}

/** The device answered a command with `{"evt":"error",...}` or reset the transfer. */
class BleCommandException(val command: String, message: String) : Exception(message)

class BleTimeoutException(val command: String) : Exception("Timed out waiting for $command")

class BleLinkLostException : Exception("The Bluetooth connection to the device was lost")

sealed interface PinResult {
    class Verified(val deviceId: String, val apiToken: String) : PinResult {
        override fun toString(): String = "Verified(deviceId=$deviceId)"
    }

    /** The device did not accept the proof; `attemptsLeft` more tries before it locks. */
    data class Rejected(val attemptsLeft: Int) : PinResult

    /**
     * The link dropped while the PIN was being checked. That is what the device does on its
     * third failure (it rotates PIN, keypair and nonce and locks for five seconds), and the
     * recovery is the same whatever the cause: reconnect, redo the handshake, read the PIN
     * that is on the screen now.
     */
    data object LockedOut : PinResult
}

sealed interface TailscaleProgress {
    data class AuthUrl(val url: String) : TailscaleProgress
    data class Ready(val fqdn: String, val ip: String) : TailscaleProgress
}

/**
 * Phone side of `lapis-ble-v1` above the GATT link: handshake, PIN proof, sequenced AES-GCM
 * envelopes, chunking and reassembly, and the provisioning commands.
 *
 * Mirrors `daemon/src/lapis_daemon/ble_prov/session.py`:
 *  - a wrong PIN is met with silence (the device only sees a GCM failure), so a missing
 *    `pin_verified` within [pinTimeoutMs] counts as a rejected attempt;
 *  - a retry re-derives keys from the same hellos and sends `pin_verify` with `seq = 1` again;
 *  - after the third failure the device rotates everything and disconnects: redo [handshake].
 */
class ProvisioningClient(
    private val transport: BleTransport,
    private val random: SecureRandom = SecureRandom(),
    nowMs: () -> Long = { System.nanoTime() / 1_000_000 },
    private val newPrivateKey: () -> ByteArray = BleEnvelopeCrypto::generatePrivateKey,
    private val pinTimeoutMs: Long = PIN_TIMEOUT_MS,
) {
    private val _events = MutableSharedFlow<BleEvent>(extraBufferCapacity = 64)
    val events: SharedFlow<BleEvent> = _events.asSharedFlow()

    private val reassembler = Reassembler(nowMs)
    private val state = Mutex() // guards keys and counters
    private val sendLock = Mutex() // one logical message on the wire at a time
    private val commandLock = Mutex() // one request/response exchange at a time

    private var privateKey: ByteArray? = null
    private var clientPub = ByteArray(0)
    private var clientNonce = ByteArray(0)
    private var devicePub = ByteArray(0)
    private var deviceNonce = ByteArray(0)
    private var shared: ByteArray? = null
    private var keys: BleEnvelopeCrypto.SessionKeys? = null
    private var txSeq = 1L
    private var rxSeq = 1L
    private var txMsgId = 0
    @Volatile
    private var broken = false

    var pinFailures: Int = 0
        private set

    var isVerified: Boolean = false
        private set

    /**
     * Feeds EVT_RX notifications into the session until the link drops. Run it in its own
     * coroutine for as long as the connection lives.
     */
    suspend fun pump() {
        try {
            transport.notifications.collect { onFrame(it) }
        } finally {
            broken = true
            _events.tryEmit(BleEvent.Disconnected)
        }
    }

    /** ClientHello / ServerHello with a fresh ephemeral keypair and nonce. */
    suspend fun handshake() = commandLock.withLock {
        if (transport.attMtu < MIN_HANDSHAKE_MTU) {
            throw HandshakeException(
                "The connection negotiated an ATT MTU of ${transport.attMtu}; " +
                    "$MIN_HANDSHAKE_MTU or more is needed for the handshake",
            )
        }
        val priv = newPrivateKey()
        val pub = BleEnvelopeCrypto.publicKey(priv)
        val nonce = ByteArray(BleEnvelopeCrypto.NONCE_LEN).also(random::nextBytes)
        transport.writeHandshake(BleEnvelopeCrypto.buildHello(pub, nonce))
        val hello = BleEnvelopeCrypto.parseHello(transport.readHandshake())
        val secret = BleEnvelopeCrypto.computeShared(priv, hello.publicKey)
        state.withLock {
            wipeLocked()
            privateKey = priv
            clientPub = pub
            clientNonce = nonce
            devicePub = hello.publicKey
            deviceNonce = hello.nonce
            shared = secret
            reassembler.reset()
            broken = false
            isVerified = false
            pinFailures = 0
            txMsgId = 0
        }
    }

    /**
     * Proves knowledge of the PIN on the LCD and checks the device's proof in return.
     * Throws [HandshakeException] if the device's proof does not verify (never trust that
     * session), [BleLinkLostException] if the link drops for another reason.
     */
    suspend fun verifyPin(pin: String, deviceName: String, deviceModel: String): PinResult =
        commandLock.withLock {
            require(BleEnvelopeCrypto.isValidPin(pin)) { "PIN must be exactly 6 digits" }
            val sessionKeys = state.withLock {
                val secret = shared ?: throw HandshakeException("handshake has not completed")
                keys?.wipe()
                // Same hellos, new PIN: new keys, and both counters start over at 1.
                BleEnvelopeCrypto.deriveKeys(
                    secret, clientPub, devicePub, clientNonce, deviceNonce, pin,
                ).also {
                    keys = it
                    txSeq = 1
                    rxSeq = 1
                }
            }
            val command = buildJsonObject {
                put("cmd", "pin_verify")
                put("proof", BleEnvelopeCrypto.clientProof(sessionKeys).toHex())
                put("device_name", deviceName)
                put("device_model", deviceModel)
            }
            val reply = try {
                exchange("pin_verify", command, pinTimeoutMs) { it as? BleEvent.PinVerified }
            } catch (e: BleTimeoutException) {
                null
            } catch (e: BleLinkLostException) {
                pinFailures += 1
                return@withLock PinResult.LockedOut
            }
            if (reply == null) {
                pinFailures += 1
                return@withLock if (pinFailures >= MAX_PIN_FAILURES) {
                    PinResult.LockedOut
                } else {
                    PinResult.Rejected(MAX_PIN_FAILURES - pinFailures)
                }
            }
            val proof = reply.proofHex.hexToBytesOrNull()
            val expected = BleEnvelopeCrypto.serverProof(sessionKeys)
            if (proof == null || !BleEnvelopeCrypto.constantTimeEquals(expected, proof)) {
                state.withLock { broken = true }
                throw HandshakeException("The device failed to prove it knows the PIN")
            }
            if (reply.apiToken.isEmpty()) {
                state.withLock { broken = true }
                throw HandshakeException("The device did not issue a token")
            }
            state.withLock {
                isVerified = true
                pinFailures = 0
            }
            PinResult.Verified(reply.deviceId, reply.apiToken)
        }

    suspend fun wifiScan(timeoutMs: Long = 45_000): List<WifiNetwork> = verified {
        exchange("wifi_scan", command("wifi_scan"), timeoutMs) { (it as? BleEvent.WifiList)?.networks }
    }

    /** Waits for the terminal `wifi_status` ("connected" or "failed"); "connecting" is skipped. */
    suspend fun wifiConnect(ssid: String, psk: String?, timeoutMs: Long = 75_000): BleEvent.WifiStatus =
        verified {
            val cmd = buildJsonObject {
                put("cmd", "wifi_connect")
                put("ssid", ssid)
                put("psk", if (psk.isNullOrEmpty()) JsonNull else JsonPrimitive(psk))
            }
            exchange("wifi_connect", cmd, timeoutMs) { ev ->
                (ev as? BleEvent.WifiStatus)?.takeIf { it.state != "connecting" }
            }
        }

    /**
     * Starts `tailscale up`. Emits each distinct login URL, then [TailscaleProgress.Ready] and
     * completes. Fails with [BleCommandException] when the device reports an error (it does so
     * itself after 300 s), which is why the local timeout is a little longer.
     */
    fun tailscaleUp(authKey: String?, timeoutMs: Long = 330_000): Flow<TailscaleProgress> = flow {
        val cmd = buildJsonObject {
            put("cmd", "tailscale_up")
            put("authkey", if (authKey.isNullOrBlank()) JsonNull else JsonPrimitive(authKey.trim()))
        }
        requireVerified()
        try {
            withTimeout(timeoutMs) {
                events.onSubscription { send(cmd) }.transformWhile { ev ->
                    when (ev) {
                        is BleEvent.TailscaleAuth -> {
                            emit(TailscaleProgress.AuthUrl(ev.url))
                            true
                        }

                        is BleEvent.Ready -> {
                            emit(TailscaleProgress.Ready(ev.tailscaleFqdn, ev.tailscaleIp))
                            false
                        }

                        is BleEvent.Error ->
                            if (ev.cmd == "tailscale_up") {
                                throw BleCommandException(ev.cmd, ev.message)
                            } else {
                                true
                            }

                        BleEvent.Disconnected -> throw BleLinkLostException()
                        else -> true
                    }
                }.collect { emit(it) }
            }
        } catch (e: TimeoutCancellationException) {
            throw BleTimeoutException("tailscale_up")
        }
    }

    /** Values are sent encrypted and never echoed; the reply lists the saved names only. */
    suspend fun pushSecrets(secrets: Map<String, String>, timeoutMs: Long = 20_000): List<String> =
        verified {
            val cmd = buildJsonObject {
                put("cmd", "push_secrets")
                putJsonObject("secrets") { secrets.forEach { (k, v) -> put(k, v) } }
            }
            exchange("push_secrets", cmd, timeoutMs) { (it as? BleEvent.SecretsSaved)?.keys }
        }

    /** `finish_provisioning`: the device answers `bye` and shuts its BLE service down. */
    suspend fun finish(timeoutMs: Long = 8_000) {
        requireVerified()
        try {
            commandLock.withLock {
                exchange("finish_provisioning", command("finish_provisioning"), timeoutMs) { ev ->
                    (ev as? BleEvent.Bye)?.let { Unit }
                }
            }
        } catch (e: BleLinkLostException) {
            // The device tears the link down right after `bye`; losing it here is success.
        }
    }

    suspend fun close() = state.withLock { wipeLocked() }

    // ------------------------------------------------------------------ internals

    private fun command(name: String): JsonObject = buildJsonObject { put("cmd", name) }

    private fun requireVerified() {
        check(isVerified) { "PIN has not been verified" }
    }

    private suspend fun <T> verified(block: suspend () -> T): T {
        requireVerified()
        return commandLock.withLock { block() }
    }

    private suspend fun <T : Any> exchange(
        name: String,
        command: JsonObject,
        timeoutMs: Long,
        pick: (BleEvent) -> T?,
    ): T = try {
        withTimeout(timeoutMs) {
            // onSubscription runs after the subscription exists, so the reply cannot be missed.
            events.onSubscription { send(command) }.mapNotNull { ev ->
                when (ev) {
                    is BleEvent.Error ->
                        if (ev.cmd == name) throw BleCommandException(name, ev.message) else null

                    BleEvent.TransportReset ->
                        throw BleCommandException(name, "The device reset the transfer; try again")

                    BleEvent.Disconnected -> throw BleLinkLostException()
                    else -> pick(ev)
                }
            }.first()
        }
    } catch (e: TimeoutCancellationException) {
        throw BleTimeoutException(name)
    }

    private suspend fun send(command: JsonObject) = sendLock.withLock {
        val frames = state.withLock {
            if (broken) throw BleLinkLostException()
            val sessionKeys = keys ?: throw HandshakeException("no session keys")
            // Compact UTF-8 JSON, as the protocol requires.
            val plaintext = command.toString().toByteArray(Charsets.UTF_8)
            val envelope = BleEnvelopeCrypto.seal(
                sessionKeys.kC2d, BleEnvelopeCrypto.DIR_C2D, txSeq, plaintext,
            )
            // A (key, seq) pair is spent the moment it is sealed, even if the write fails:
            // reusing it for other plaintext would repeat a GCM nonce.
            txSeq += 1
            val id = txMsgId
            txMsgId = (txMsgId + 1) and 0xFF
            ChunkFraming.split(
                id, ChunkFraming.FLAG_ENCRYPTED, envelope,
                ChunkFraming.writeChunkData(transport.attMtu),
            )
        }
        try {
            for (frame in frames) transport.writeCommand(frame)
        } catch (e: kotlinx.coroutines.CancellationException) {
            state.withLock { broken = true }
            throw e
        } catch (e: Exception) {
            // The device now holds a partial message and our counter moved on: the session
            // cannot continue. A failed GATT write practically always means the link is gone.
            state.withLock { broken = true }
            throw BleLinkLostException()
        }
    }

    private suspend fun onFrame(frame: ByteArray) {
        when (val result = reassembler.feed(frame)) {
            Reassembler.Result.Incomplete -> Unit
            Reassembler.Result.PeerReset -> _events.emit(BleEvent.TransportReset)
            is Reassembler.Result.Error ->
                runCatching { transport.writeCommand(ChunkFraming.resetFrame()) }

            is Reassembler.Result.Message -> {
                // Plaintext frames are never accepted: everything after the hellos is sealed.
                if (result.flags and ChunkFraming.FLAG_ENCRYPTED == 0) return
                val plaintext = state.withLock {
                    val sessionKeys = keys ?: return
                    try {
                        BleEnvelopeCrypto.open(
                            sessionKeys.kD2c, BleEnvelopeCrypto.DIR_D2C, result.payload, rxSeq,
                        ).also { rxSeq += 1 }
                    } catch (e: EnvelopeException) {
                        return // replayed, out of order or tampered: dropped, counter unchanged
                    }
                }
                BleEvent.parse(plaintext)?.let { _events.emit(it) }
            }
        }
    }

    private fun wipeLocked() {
        privateKey?.fill(0)
        shared?.fill(0)
        keys?.wipe()
        privateKey = null
        shared = null
        keys = null
        isVerified = false
    }

    companion object {
        const val MAX_PIN_FAILURES = 3
        const val PIN_TIMEOUT_MS = 8_000L
        const val LOCKOUT_COOLDOWN_MS = 5_000L

        /** A 49-byte hello must fit one ATT write (MTU - 3): the device does not assemble long writes. */
        const val MIN_HANDSHAKE_MTU = BleEnvelopeCrypto.HELLO_LEN + 3
    }
}
