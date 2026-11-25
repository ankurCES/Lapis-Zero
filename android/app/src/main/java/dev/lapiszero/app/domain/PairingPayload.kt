package dev.lapiszero.app.domain

import dev.lapiszero.app.data.remote.Endpoint
import java.net.URLDecoder

/**
 * The QR code on the CardputerZero LCD (spec 5.4):
 * `lapis://pair?host=<fqdn>&nonce=<43-char base64url>&pin=<6 digits>` and, from a development
 * daemon only, a trailing `&tls=0` (then `host` is `HOST:PORT` and the exchange uses http).
 */
data class PairingPayload(
    val host: String,
    val nonce: String,
    val pin: String,
    val tls: Boolean,
) {
    val endpoint: Endpoint get() = Endpoint(host, tls)

    override fun toString(): String = "PairingPayload(host=$host, tls=$tls)" // nonce stays out of logs

    sealed interface ParseResult {
        data class Ok(val payload: PairingPayload) : ParseResult
        data class Invalid(val reason: String) : ParseResult
    }

    companion object {
        private const val PREFIX = "lapis://pair?"
        private val NONCE_RE = Regex("^[A-Za-z0-9_-]{43}$")
        private val PIN_RE = Regex("^[0-9]{6}$")

        fun parse(raw: String): ParseResult {
            val text = raw.trim()
            if (!text.startsWith(PREFIX, ignoreCase = true)) {
                return ParseResult.Invalid("Not a Lapis pairing code")
            }
            val params = HashMap<String, String>()
            for (part in text.substring(PREFIX.length).split('&')) {
                if (part.isEmpty()) continue
                val key = part.substringBefore('=')
                val value = part.substringAfter('=', "")
                if (key in params) return ParseResult.Invalid("Duplicate field: $key")
                params[key] = try {
                    // base64url never contains '+', but keep it literal rather than a space.
                    URLDecoder.decode(value.replace("+", "%2B"), "UTF-8")
                } catch (e: IllegalArgumentException) {
                    return ParseResult.Invalid("Malformed pairing code")
                }
            }
            val host = params["host"].orEmpty()
            val nonce = params["nonce"].orEmpty()
            val pin = params["pin"].orEmpty()
            val tls = when (params["tls"]) {
                null, "1" -> true
                "0" -> false
                else -> return ParseResult.Invalid("Unknown tls value")
            }
            if (!Endpoint.isValidHost(host)) return ParseResult.Invalid("Pairing code has no valid host")
            if (!NONCE_RE.matches(nonce)) return ParseResult.Invalid("Pairing code has no valid nonce")
            if (!PIN_RE.matches(pin)) return ParseResult.Invalid("Pairing code has no valid PIN")
            if (tls && host.contains(':')) {
                return ParseResult.Invalid("A production pairing code must not carry a port")
            }
            if (!tls && !Endpoint.isCleartextAllowed(host)) {
                return ParseResult.Invalid(
                    "Development (plain HTTP) pairing codes only work for 127.0.0.1, localhost " +
                        "or 10.0.2.2. Use `adb reverse` to reach a daemon on your computer.",
                )
            }
            return ParseResult.Ok(PairingPayload(host, nonce, pin, tls))
        }
    }
}
