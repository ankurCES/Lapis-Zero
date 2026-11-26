package dev.lapiszero.app.data.ble

import java.io.IOException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

/**
 * The CardputerZero side of `lapis-ble-v1`, ported line by line from
 * `daemon/src/lapis_daemon/ble_prov/session.py` (handshake, strict `seq == expected`, silent
 * failures, three-strike lockout with rotation, error events), so the phone-side client is
 * exercised against the device's real behaviour without Bluetooth.
 *
 * Paired-phone mode (protocol section 4.2): the first envelope is tried with every candidate key
 * set (the PIN's while [pinOnScreen], and one per token in [pairedTokens]); the first that opens
 * decides the session's mode. A token session may only scan, join Wi-Fi and finish.
 */
class FakeLapisDevice(var pin: String = "482917") {
    private var privateKey = BleEnvelopeCrypto.generatePrivateKey()
    var publicKey: ByteArray = BleEnvelopeCrypto.publicKey(privateKey)
        private set
    var deviceNonce = ByteArray(16) { (0xF0 + it).toByte() }
        private set

    private var keys: BleEnvelopeCrypto.SessionKeys? = null
    private var candidates: List<Pair<String, BleEnvelopeCrypto.SessionKeys>> = emptyList()
    private var helloSeen = false

    /** Tokens of the phones this device paired (the daemons keep only their SHA-256). */
    val pairedTokens = ArrayList<String>()

    /** A PIN is on screen, so PIN pairing is accepted next to paired phones. */
    var pinOnScreen = true

    /** "pin" or "token": decided by the key set that opened the first envelope. */
    var mode: String? = null
        private set
    private var rxSeq = 1L
    private var txSeq = 0L
    private var txMsg = 0
    private val rx = Reassembler { 0 }
    private var link: Connection? = null
    private var lastFrames: List<ByteArray> = emptyList()

    var verified = false
        private set
    var failures = 0
        private set
    var rotations = 0
        private set
    var handshakes = 0
        private set
    var registered = 0
        private set
    var finished = false
        private set

    /** Decrypted commands in arrival order. */
    val commands = ArrayList<JsonObject>()
    val savedSecrets = LinkedHashMap<String, String>()

    // ---- knobs
    var lieAboutProof = false

    /** Commands the device reads (and counts) but never answers. */
    val mute = HashSet<String>()
    var networks: JsonArray = buildJsonArray {
        add(buildJsonObject { put("ssid", "HomeWiFi"); put("rssi", -48); put("secured", true) })
        add(buildJsonObject { put("ssid", "café guest"); put("rssi", -71); put("secured", false) })
    }
    var wifiPassword = "correct horse"
    var wifiScanFails = false
    var authUrls = listOf("https://login.tailscale.com/a/abc123")
    var tailscaleError: String? = null
    var fqdn = "lapis-zero.tailnet.ts.net"

    inner class Connection(override val attMtu: Int) : BleTransport {
        private val out = Channel<ByteArray>(Channel.UNLIMITED)
        override val notifications: Flow<ByteArray> = out.receiveAsFlow()
        var open = true
            private set

        /** Raw frames the phone wrote to CMD_TX. */
        val writes = ArrayList<ByteArray>()

        /** Fail the Nth and later CMD_TX writes (a dying link). */
        var failWritesFrom: Int? = null

        override suspend fun writeHandshake(clientHello: ByteArray) {
            if (!open) throw IOException("GATT write failed: disconnected")
            handshakeWrite(clientHello)
        }

        override suspend fun readHandshake(): ByteArray {
            if (!open) throw IOException("GATT read failed: disconnected")
            return BleEnvelopeCrypto.buildHello(publicKey, deviceNonce)
        }

        override suspend fun writeCommand(frame: ByteArray) {
            if (!open) throw IOException("GATT write failed: disconnected")
            failWritesFrom?.let { if (writes.size >= it) throw IOException("GATT write failed") }
            check(frame.size <= attMtu - 3) { "frame of ${frame.size} bytes exceeds the ATT payload" }
            check(frame.size <= 512) { "frame of ${frame.size} bytes exceeds the 512-byte attribute limit" }
            writes += frame
            onFrame(frame)
        }

        fun notify(frame: ByteArray) {
            if (open) out.trySend(frame)
        }

        fun drop() {
            open = false
            out.close()
        }
    }

    fun connect(attMtu: Int = 185): Connection = Connection(attMtu).also { link = it }

    /** Fresh PIN, keypair and nonce: first use and after a lockout. */
    fun rotate(newPin: String = "135790") {
        pin = newPin
        privateKey = BleEnvelopeCrypto.generatePrivateKey()
        publicKey = BleEnvelopeCrypto.publicKey(privateKey)
        deviceNonce = ByteArray(16) { (0xA0 + rotations + it).toByte() }
        keys = null
        candidates = emptyList()
        helloSeen = false
        mode = null
        verified = false
        failures = 0
        rx.reset()
        rotations++
    }

    private fun handshakeWrite(data: ByteArray) {
        val hello = BleEnvelopeCrypto.parseHello(data)
        val shared = BleEnvelopeCrypto.computeShared(privateKey, hello.publicKey)
        candidates = buildList {
            if (pinOnScreen) add("pin" to BleEnvelopeCrypto.deriveKeys(shared, hello.publicKey, publicKey, hello.nonce, deviceNonce, pin))
            pairedTokens.forEach {
                val secret = BleEnvelopeCrypto.pairedSecret(it)
                add("token" to BleEnvelopeCrypto.deriveKeysWithToken(shared, hello.publicKey, publicKey, hello.nonce, deviceNonce, secret))
            }
        }
        keys = null
        mode = null
        helloSeen = true
        rxSeq = 1
        txSeq = 0
        txMsg = 0
        verified = false
        rx.reset()
        handshakes++
    }

    private fun onFrame(frame: ByteArray) {
        val message = when (val result = rx.feed(frame)) {
            is Reassembler.Result.Message -> result
            is Reassembler.Result.Error -> {
                link?.notify(ChunkFraming.resetFrame())
                return
            }

            else -> return
        }
        val decided = keys
        if (message.flags and ChunkFraming.FLAG_ENCRYPTED == 0 || !helloSeen) {
            link?.notify(ChunkFraming.resetFrame())
            return
        }
        fun tryOpen(k: BleEnvelopeCrypto.SessionKeys): ByteArray? = try {
            BleEnvelopeCrypto.open(k.kC2d, BleEnvelopeCrypto.DIR_C2D, message.payload, rxSeq)
        } catch (e: EnvelopeException) {
            null
        }
        var plain = decided?.let(::tryOpen)
        if (decided == null) {
            for ((m, k) in candidates) {
                plain = tryOpen(k) ?: continue
                keys = k
                mode = m
                break
            }
        }
        if (plain == null) {
            failure() // a wrong PIN (or an unknown token) derives the wrong key, so it shows up right here
            return
        }
        val command = try {
            Json.parseToJsonElement(plain.toString(Charsets.UTF_8)).jsonObject
        } catch (e: IllegalArgumentException) {
            failure()
            return
        }
        rxSeq += 1
        commands += command
        dispatch(command)
    }

    private fun failure() {
        if (verified) return // tampering after verification is dropped, never counted
        failures += 1
        if (failures < 3) return
        rotate()
        link?.drop()
    }

    /** Sends one encrypted event, exactly like `_send` in session.py. */
    fun send(event: JsonObject) {
        val sessionKeys = keys ?: error("no keys")
        txSeq += 1
        val envelope = BleEnvelopeCrypto.seal(
            sessionKeys.kD2c, BleEnvelopeCrypto.DIR_D2C, txSeq, event.toString().toByteArray(Charsets.UTF_8),
        )
        val frames = ChunkFraming.split(
            txMsg, ChunkFraming.FLAG_ENCRYPTED, envelope, ChunkFraming.maxChunkData(link?.attMtu ?: 23),
        )
        txMsg = (txMsg + 1) % 256
        lastFrames = frames
        frames.forEach { link?.notify(it) }
    }

    /** An attacker (or a glitch) repeats the last notification frames verbatim. */
    fun replayLastMessage() = lastFrames.forEach { link?.notify(it) }

    /** Pushes arbitrary bytes to the phone on EVT_RX. */
    fun inject(frame: ByteArray) {
        link?.notify(frame)
    }

    private fun error(cmd: String, message: String) =
        send(buildJsonObject { put("evt", "error"); put("cmd", cmd); put("message", message) })

    private fun str(command: JsonObject, key: String): String =
        (command[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull.orEmpty()

    private fun dispatch(command: JsonObject) {
        val cmd = str(command, "cmd")
        if (cmd == "pin_verify" || cmd == "token_verify") {
            // pin_verify in a token-keyed session, or token_verify in a PIN-keyed one: a failed attempt.
            when {
                (cmd == "pin_verify") != (mode == "pin") -> failure()
                cmd == "pin_verify" -> pinVerify(command)
                else -> tokenVerify(command)
            }
            return
        }
        if (!verified) {
            failure()
            return
        }
        if (cmd in mute) return
        if (mode == "token" && cmd !in PAIRED_COMMANDS) {
            error(cmd, "not allowed for a paired phone")
            return
        }
        when (cmd) {
            "wifi_scan" ->
                if (wifiScanFails) {
                    error("wifi_scan", "RuntimeError: nmcli not found")
                } else {
                    send(buildJsonObject { put("evt", "wifi_list"); put("networks", networks) })
                }

            "wifi_connect" -> {
                val ssid = str(command, "ssid")
                if (ssid.isEmpty()) {
                    error("wifi_connect", "ssid is required")
                    return
                }
                send(buildJsonObject { put("evt", "wifi_status"); put("state", "connecting"); put("local_ip", "") })
                val ok = str(command, "psk") == wifiPassword || ssid == "café guest"
                send(
                    buildJsonObject {
                        put("evt", "wifi_status")
                        put("state", if (ok) "connected" else "failed")
                        put("local_ip", if (ok) "192.168.1.42" else "")
                    },
                )
            }

            "tailscale_up" -> {
                val authKey = str(command, "authkey")
                if (authKey.isEmpty()) {
                    authUrls.forEach { url -> send(buildJsonObject { put("evt", "tailscale_auth"); put("url", url) }) }
                }
                val failure = tailscaleError
                if (failure != null) {
                    error("tailscale_up", failure)
                } else {
                    send(buildJsonObject { put("evt", "ready"); put("tailscale_fqdn", fqdn); put("tailscale_ip", "100.91.12.4") })
                }
            }

            "push_secrets" -> {
                val values = command["secrets"] as? JsonObject
                if (values == null || values.isEmpty()) {
                    error("push_secrets", "secrets must be a non-empty object")
                    return
                }
                values.forEach { (k, v) -> savedSecrets[k] = (v as JsonPrimitive).content }
                send(buildJsonObject { put("evt", "secrets_saved"); put("keys", buildJsonArray { values.keys.sorted().forEach { add(JsonPrimitive(it)) } }) })
            }

            "finish_provisioning" -> {
                send(buildJsonObject { put("evt", "bye") })
                finished = true
                link?.drop() // the service exits half a second after `bye`
            }

            else -> error(cmd, "unknown command")
        }
    }

    private fun tokenVerify(command: JsonObject) {
        val sessionKeys = keys ?: return
        val proof = str(command, "proof").hexToBytesOrNull() ?: ByteArray(0)
        if (!BleEnvelopeCrypto.constantTimeEquals(proof, BleEnvelopeCrypto.clientProof(sessionKeys))) {
            failure()
            return
        }
        verified = true
        failures = 0
        val serverProof = BleEnvelopeCrypto.serverProof(sessionKeys).also { if (lieAboutProof) it[0] = (it[0].toInt() xor 1).toByte() }
        send(buildJsonObject { put("evt", "token_verified"); put("proof", serverProof.toHex()); put("device_id", "dev_01") })
    }

    private companion object {
        val PAIRED_COMMANDS = setOf("wifi_scan", "wifi_connect", "finish_provisioning")
    }

    private fun pinVerify(command: JsonObject) {
        val sessionKeys = keys ?: return
        val proof = str(command, "proof").hexToBytesOrNull() ?: ByteArray(0)
        if (!BleEnvelopeCrypto.constantTimeEquals(proof, BleEnvelopeCrypto.clientProof(sessionKeys))) {
            failure()
            return
        }
        if (!verified) {
            verified = true
            failures = 0
            registered++
        }
        val serverProof = BleEnvelopeCrypto.serverProof(sessionKeys).also { if (lieAboutProof) it[0] = (it[0].toInt() xor 1).toByte() }
        send(
            buildJsonObject {
                put("evt", "pin_verified")
                put("proof", serverProof.toHex())
                put("device_id", "dev_01")
                put("api_token", "tok-from-ble-0123456789")
            },
        )
    }
}
