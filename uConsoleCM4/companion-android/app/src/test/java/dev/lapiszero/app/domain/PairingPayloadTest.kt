package dev.lapiszero.app.domain

import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter
import dev.lapiszero.app.domain.PairingPayload.ParseResult
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PairingPayloadTest {
    private val nonce = "GneYMjL9l4v_GZvPP4A8TukmQs978gPNuFg-W3Q3ZGc" // 43 chars, base64url
    private val prod = "lapis://pair?host=lapis-zero.tailnet.ts.net&nonce=$nonce&pin=482917"
    private val dev = "lapis://pair?host=127.0.0.1:18431&nonce=$nonce&pin=114773&tls=0"

    private fun ok(raw: String): PairingPayload = (PairingPayload.parse(raw) as ParseResult.Ok).payload

    private fun invalid(raw: String): String = (PairingPayload.parse(raw) as ParseResult.Invalid).reason

    @Test
    fun `production payload uses https to the fqdn`() {
        val p = ok(prod)
        assertEquals("lapis-zero.tailnet.ts.net", p.host)
        assertEquals(nonce, p.nonce)
        assertEquals("482917", p.pin)
        assertTrue(p.tls)
        assertEquals("https://lapis-zero.tailnet.ts.net", p.endpoint.baseUrl)
        assertEquals("wss://lapis-zero.tailnet.ts.net/v1/ws", p.endpoint.wsUrl)
    }

    @Test
    fun `development payload with tls 0 uses plain http to host and port`() {
        val p = ok(dev)
        assertEquals("127.0.0.1:18431", p.host)
        assertFalse(p.tls)
        assertEquals("http://127.0.0.1:18431", p.endpoint.baseUrl)
        assertEquals("ws://127.0.0.1:18431/v1/ws", p.endpoint.wsUrl)
        assertEquals("10.0.2.2:8421", ok("lapis://pair?host=10.0.2.2:8421&nonce=$nonce&pin=000000&tls=0").host)
        assertEquals("localhost:8421", ok("lapis://pair?host=localhost:8421&nonce=$nonce&pin=000000&tls=0").host)
    }

    @Test
    fun `field order whitespace and scheme case do not matter`() {
        assertEquals(ok(prod), ok("  LAPIS://pair?pin=482917&nonce=$nonce&host=lapis-zero.tailnet.ts.net\n"))
        assertEquals(ok(prod), ok("$prod&tls=1"))
        assertEquals(ok(prod), ok("$prod&future=1"))
        assertEquals("127.0.0.1:18431", ok(dev.replace("127.0.0.1:18431", "127.0.0.1%3A18431")).host)
    }

    @Test
    fun `cleartext is refused for anything but a loopback development daemon`() {
        assertTrue(invalid("lapis://pair?host=lapis-zero.tailnet.ts.net&nonce=$nonce&pin=482917&tls=0").contains("Development"))
        assertTrue(invalid("lapis://pair?host=192.168.1.50:8421&nonce=$nonce&pin=482917&tls=0").contains("127.0.0.1"))
        assertTrue(invalid("lapis://pair?host=evil.example:80&nonce=$nonce&pin=482917&tls=0").isNotEmpty())
    }

    @Test
    fun `malformed input is rejected with a reason`() {
        val cases = listOf(
            "",
            "hello",
            "https://example.com/?host=a&nonce=$nonce&pin=482917",
            "lapis://other?host=a.ts.net&nonce=$nonce&pin=482917",
            "lapis://pair",
            "lapis://pair?",
            "lapis://pair?nonce=$nonce&pin=482917", // no host
            "lapis://pair?host=&nonce=$nonce&pin=482917",
            "lapis://pair?host=a.ts.net&pin=482917", // no nonce
            "lapis://pair?host=a.ts.net&nonce=short&pin=482917",
            "lapis://pair?host=a.ts.net&nonce=${nonce}A&pin=482917", // 44 chars
            "lapis://pair?host=a.ts.net&nonce=${nonce.dropLast(1)}%2B&pin=482917", // '+' is not base64url
            "lapis://pair?host=a.ts.net&nonce=$nonce", // no pin
            "lapis://pair?host=a.ts.net&nonce=$nonce&pin=12345",
            "lapis://pair?host=a.ts.net&nonce=$nonce&pin=12345a",
            "lapis://pair?host=a.ts.net&nonce=$nonce&pin=1234567",
            "lapis://pair?host=a.ts.net&nonce=$nonce&pin=482917&tls=2",
            "lapis://pair?host=a.ts.net&host=b.ts.net&nonce=$nonce&pin=482917", // duplicate field
            "lapis://pair?host=a.ts.net:8443&nonce=$nonce&pin=482917", // port on a production code
            "lapis://pair?host=a.ts.net%2Fevil&nonce=$nonce&pin=482917",
            "lapis://pair?host=user%40a.ts.net&nonce=$nonce&pin=482917",
            "lapis://pair?host=a.ts.net%23frag&nonce=$nonce&pin=482917",
            "lapis://pair?host=a%20b.ts.net&nonce=$nonce&pin=482917",
            "lapis://pair?host=127.0.0.1:99999&nonce=$nonce&pin=482917&tls=0",
            "lapis://pair?host=%zz&nonce=$nonce&pin=482917",
        )
        for (raw in cases) {
            val result = PairingPayload.parse(raw)
            assertTrue("should be invalid: $raw", result is ParseResult.Invalid)
            assertTrue((result as ParseResult.Invalid).reason.isNotBlank())
        }
    }

    @Test
    fun `nonce never appears in toString`() {
        assertFalse(ok(prod).toString().contains(nonce))
    }

    @Test
    fun `qr decoder reads a rendered payload both dark on light and inverted`() {
        for (payload in listOf(prod, dev)) {
            val (luma, size) = render(payload, scale = 4, quiet = 4, inverted = false)
            assertEquals(payload, QrDecoder.decode(luma, size, size, size))
            // The device's dark "Ink" theme draws light modules on a dark screen.
            val (inv, invSize) = render(payload, scale = 4, quiet = 4, inverted = true)
            assertEquals(payload, QrDecoder.decode(inv, invSize, invSize, invSize))
        }
    }

    @Test
    fun `qr decoder copes with row padding and wide frames and returns null on noise`() {
        val (luma, size) = render(prod, scale = 3, quiet = 4, inverted = false)
        // Embed the code in the middle of a wider 16:9-ish frame whose rows are padded.
        val width = size * 2
        val stride = width + 24
        val frame = ByteArray(stride * size) { 0xFF.toByte() }
        for (y in 0 until size) System.arraycopy(luma, y * size, frame, y * stride + size / 2, size)
        assertEquals(prod, QrDecoder.decode(frame, stride, width, size))

        assertNull(QrDecoder.decode(ByteArray(200 * 200) { (it * 31 % 251).toByte() }, 200, 200, 200))
        assertNull(QrDecoder.decode(ByteArray(10), 200, 200, 200)) // buffer too small
        assertNull(QrDecoder.decode(ByteArray(0), 0, 0, 0))
    }

    /** Renders a QR code into an 8-bit luminance square, like a camera's Y plane would hold it. */
    private fun render(text: String, scale: Int, quiet: Int, inverted: Boolean): Pair<ByteArray, Int> {
        val matrix = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 0, 0, mapOf(com.google.zxing.EncodeHintType.MARGIN to 0))
        val modules = matrix.width
        val size = (modules + 2 * quiet) * scale
        val dark = if (inverted) 0xF0 else 0x10
        val light = if (inverted) 0x10 else 0xF0
        val out = ByteArray(size * size) { light.toByte() }
        for (my in 0 until modules) for (mx in 0 until modules) {
            if (!matrix.get(mx, my)) continue
            for (dy in 0 until scale) for (dx in 0 until scale) {
                out[((my + quiet) * scale + dy) * size + (mx + quiet) * scale + dx] = dark.toByte()
            }
        }
        return out to size
    }
}
