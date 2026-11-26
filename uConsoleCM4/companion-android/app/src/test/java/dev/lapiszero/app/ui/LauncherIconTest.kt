package dev.lapiszero.app.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.PorterDuff
import android.graphics.PorterDuffColorFilter
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Drawable
import androidx.test.core.app.ApplicationProvider
import dev.lapiszero.app.R
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The launcher icon is the adaptive Lapis head on lapis lazuli, with a monochrome layer for themed
 * icons, and it is what the manifest uses. With `-Plapis.icon.png=<file>` it is also rendered to a
 * sheet for a look: as a launcher masks it, the full 108 dp layers with the safe zone, and themed.
 */
@RunWith(RobolectricTestRunner::class)
class LauncherIconTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val size = 432 // 108 dp at 4x for the layers; the masked icon shows the inner 72 dp

    private fun Drawable.render(px: Int, background: Int = Color.TRANSPARENT): Bitmap {
        val bitmap = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        canvas.drawColor(background)
        setBounds(0, 0, px, px)
        draw(canvas)
        return bitmap
    }

    /** Pixel of a masked icon at layer coordinates (dp in the 108 dp viewport). */
    private fun Bitmap.atLayer(x: Float, y: Float): Int = getPixel(((x - 18) / 72 * width).toInt(), ((y - 18) / 72 * height).toInt())

    @Test
    fun `the launcher icon is the adaptive lapis head with a monochrome layer`() {
        assertEquals("Lapis Zero", context.getString(R.string.app_name))
        val info = context.packageManager.getApplicationInfo(context.packageName, 0)
        assertEquals(R.mipmap.ic_launcher, info.icon)
        val icon = context.getDrawable(R.mipmap.ic_launcher) as AdaptiveIconDrawable
        assertNotNull("themed icons need the monochrome layer", icon.monochrome)

        val masked = icon.render(size)
        fun Int.isStoneBlue() = Color.blue(this) > Color.red(this) + 40 && Color.blue(this) > Color.green(this) + 20
        fun Int.isPaper() = Color.red(this) > 170 && Color.green(this) > 170 && Color.blue(this) > 160
        fun Int.isInk() = Color.red(this) < 70 && Color.green(this) < 70 && Color.blue(this) < 80
        assertTrue("lapis blue around the head", masked.atLayer(40f, 26f).isStoneBlue())
        assertTrue("paper shell under the visor", masked.atLayer(54f, 72f).isPaper())
        assertTrue("ink visor between the eyes", masked.atLayer(54f, 60f).isInk())
        val eye = masked.atLayer(45f, 56f)
        assertTrue("a blue eye", Color.blue(eye) > 200 && Color.red(eye) < 160)

        val out = System.getProperty("lapis.icon.png")?.takeIf { it.isNotBlank() }?.let(::File) ?: return
        val sheet = Bitmap.createBitmap(size * 3 + 64, size + 32, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(sheet)
        canvas.drawColor(Color.rgb(0x3A, 0x3A, 0x40))
        // 1: as a launcher shows it (this platform's default mask).
        canvas.drawBitmap(masked, 16f, 16f, null)
        // 2: the full layers, with the 72 dp visible circle and the 66 dp safe zone.
        val layers = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        Canvas(layers).apply {
            icon.background.apply { setBounds(0, 0, size, size) }.draw(this)
            icon.foreground.apply { setBounds(0, 0, size, size) }.draw(this)
            val guide = Paint(Paint.ANTI_ALIAS_FLAG).apply {
                style = Paint.Style.STROKE
                strokeWidth = 2f
                color = Color.argb(160, 255, 255, 255)
                pathEffect = DashPathEffect(floatArrayOf(8f, 8f), 0f)
            }
            drawCircle(size / 2f, size / 2f, size * 36f / 108, guide)
            drawCircle(size / 2f, size / 2f, size * 33f / 108, guide.apply { color = Color.argb(160, 255, 107, 43) })
        }
        canvas.drawBitmap(layers, 32f + size, 16f, null)
        // 3: themed, as Android 13+ tints the monochrome layer.
        val themed = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        Canvas(themed).apply {
            drawCircle(size / 2f, size / 2f, size / 2f, Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(0xD9, 0xE4, 0xFF) })
            val mono = icon.monochrome!!.constantState!!.newDrawable().mutate()
            mono.colorFilter = PorterDuffColorFilter(Color.rgb(0x1B, 0x2E, 0x8F), PorterDuff.Mode.SRC_IN)
            // The layer is 108 dp; the visible 72 dp fill the circle.
            mono.setBounds(-size / 4, -size / 4, size + size / 4, size + size / 4)
            mono.draw(this)
        }
        canvas.drawBitmap(themed, 48f + 2 * size, 16f, null)
        out.parentFile?.mkdirs()
        out.outputStream().use { sheet.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
}
