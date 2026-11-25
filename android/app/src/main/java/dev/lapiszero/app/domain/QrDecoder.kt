package dev.lapiszero.app.domain

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.LuminanceSource
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader

/**
 * Decodes a QR code from an 8-bit luminance plane (the Y plane of a camera frame).
 * Pure JVM, so it is unit-tested without a camera.
 */
object QrDecoder {
    private val hints = mapOf(
        DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
        DecodeHintType.TRY_HARDER to true,
        DecodeHintType.CHARACTER_SET to "UTF-8",
    )

    /**
     * @param luminance row-major bytes, `rowStride` bytes per row (>= `width`)
     * @return the decoded text, or null when no QR code is readable in this frame
     */
    fun decode(luminance: ByteArray, rowStride: Int, width: Int, height: Int): String? {
        if (width <= 0 || height <= 0 || rowStride < width) return null
        if (luminance.size < rowStride * (height - 1) + width) return null
        // The LCD is small in the frame: look at the centred square first, then everything.
        val side = minOf(width, height)
        val square = PlanarYUVLuminanceSource(
            luminance, rowStride, height, (width - side) / 2, (height - side) / 2, side, side, false,
        )
        return attempt(square) ?: attempt(square.invert()) // dark "Ink" theme: light-on-dark code
    }

    private fun attempt(source: LuminanceSource): String? = try {
        QRCodeReader().decode(BinaryBitmap(HybridBinarizer(source)), hints).text
    } catch (e: NotFoundException) {
        null
    } catch (e: ReaderException) {
        null // checksum or format error: wait for a sharper frame
    } catch (e: ArrayIndexOutOfBoundsException) {
        null // ZXing can trip over degenerate finder patterns
    }
}
