package dev.lapiszero.app.ui.head

import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Paint
import android.graphics.Typeface
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.ui.graphics.Canvas
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.test.junit4.v2.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import dev.lapiszero.app.domain.HeadState
import dev.lapiszero.app.theme.LapisTheme
import dev.lapiszero.app.theme.ThemeMode
import java.io.File
import kotlin.math.abs
import kotlin.math.roundToInt
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/**
 * Not a test of anything: it renders the head to contact sheets, to be looked at next to the
 * device's own (`contact-states.png` and friends). Opt-in, so the normal run does not pay for it:
 *
 *     ./gradlew testDebugUnitTest --tests '*HeadSheetTest*' -Plapis.head.sheets=/some/directory
 *
 * The frames come from the painter and the motion model the composable uses, at chosen clock
 * times, in the colours the app's two themes give the head.
 */
@RunWith(RobolectricTestRunner::class)
class HeadSheetTest {
    @get:Rule
    val compose = createEmptyComposeRule()

    private val out: File? = System.getProperty("lapis.head.sheets")?.takeIf { it.isNotBlank() }?.let(::File)
    private val themes = LinkedHashMap<String, HeadColors>()

    @Before
    fun setUp() {
        assumeTrue("set -Plapis.head.sheets=<directory> to render the sheets", out != null)
        out!!.mkdirs()
        val context = ApplicationProvider.getApplicationContext<Context>()
        shadowOf(context.packageManager).addActivityIfNotPresent(ComponentName(context, ComponentActivity::class.java))
        // The colours are read out of the real themes, not copied here.
        for ((name, mode) in listOf("paper" to ThemeMode.PAPER, "ink" to ThemeMode.INK)) {
            ActivityScenario.launch(ComponentActivity::class.java).use { scenario ->
                scenario.onActivity { it.setContent { LapisTheme(mode) { themes[name] = headColors() } } }
                compose.waitUntil(5_000) { themes.containsKey(name) }
            }
        }
    }

    // ------------------------------------------------------------------ frames

    private fun seconds(t: Double): Long = (t * 1e9).toLong()

    /** `state` as if it had been on for a long while, at clock time `t`. */
    private fun looping(state: HeadState, t: Double) = HeadMotion().apply { advance(state, seconds(t)) }

    /** A beat (or any state) `tau` seconds after it began at clock time 6. */
    private fun beat(state: HeadState, tau: Double) = HeadMotion().apply {
        advance(state, seconds(6.0))
        advance(state, seconds(6.0 + tau))
    }

    /** `dt` seconds into the easing from `from` to `to`. */
    private fun easing(from: HeadState, to: HeadState, dt: Double) = HeadMotion().apply {
        advance(from, seconds(6.0))
        advance(from, seconds(8.0))
        advance(to, seconds(8.0))
        advance(to, seconds(8.0 + dt))
    }

    private fun still(state: HeadState) = HeadMotion().apply { still(state) }

    private fun render(colors: HeadColors, motion: HeadMotion, heightPx: Int): Bitmap {
        val width = (heightPx * HeadPainter.ZONE_WIDTH / HeadPainter.ZONE_HEIGHT).roundToInt()
        val image = ImageBitmap(width, heightPx)
        image.asAndroidBitmap().eraseColor(colors.bg.toArgb())
        HeadPainter(colors).draw(Canvas(image), width.toFloat(), heightPx.toFloat(), motion)
        return image.asAndroidBitmap()
    }

    // ------------------------------------------------------------------ sheets

    private class Cell(val bitmap: Bitmap, val caption: String)

    private val label = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = 0xFFFFFFFF.toInt()
        textSize = 20f
        typeface = Typeface.MONOSPACE
    }

    /** Rows of cells on a grey board, each row with a name on the left. */
    private fun sheet(name: String, rows: List<Pair<String, List<Cell>>>) {
        val gap = 6
        val caption = 26
        val margin = 190
        val width = margin + rows.maxOf { (_, cells) -> cells.sumOf { it.bitmap.width + gap } } + gap
        val height = rows.sumOf { (_, cells) -> cells.maxOf { it.bitmap.height } + caption + gap } + gap
        val board = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        board.eraseColor(0xFF77777D.toInt())
        val canvas = android.graphics.Canvas(board)
        var y = gap
        for ((title, cells) in rows) {
            val rowHeight = cells.maxOf { it.bitmap.height }
            canvas.drawText(title, 10f, y + rowHeight / 2f, label)
            var x = margin
            for (cell in cells) {
                canvas.drawBitmap(cell.bitmap, x.toFloat(), (y + rowHeight - cell.bitmap.height).toFloat(), null)
                canvas.drawText(cell.caption, x + 2f, y + rowHeight + 20f, label)
                x += cell.bitmap.width + gap
            }
            y += rowHeight + caption + gap
        }
        File(out, name).outputStream().use { board.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    /** The moments of each state's row: the device's own sheet samples the same ones. */
    private fun moments(state: HeadState): List<Pair<String, HeadMotion>> = when (state) {
        HeadState.IDLE -> {
            // Named moments, found on the schedule: the sway at each end, a blink, a glance each way.
            fun first(from: Double, test: (Double) -> Boolean): Double =
                generateSequence(from) { it + 0.01 }.first(test)
            val blinkAt = first(6.0) { blink(it) < 0.6f }
            val shutAt = first(blinkAt) { blink(it) == 0f }
            val left = first(0.0) { glance(it) < -0.99f }
            val right = first(0.0) { glance(it) > 0.99f }
            val turning = first(0.0) { abs(glance(it)) in 0.4f..0.6f }
            val returning = first(right + 1.0) { glance(it) < 0.6f }
            listOf(
                // (15.75 s and 11.25 s: the ends of the 9 s sway, clear of any blink or glance)
                "sway left" to 15.75, "sway right" to 11.25, "blink" to blinkAt, "shut" to shutAt,
                "turning" to turning, "glance L" to left, "glance R" to right, "returning" to returning,
            ).map { (name, t) -> name to looping(state, t) }
        }

        HeadState.DONE -> listOf(0, 100, 210, 320, 480, 620, 900, 1400).map { "$it ms" to beat(state, it / 1000.0) }
        HeadState.ERROR -> listOf(0, 75, 150, 225, 300, 450, 700, 1200).map { "$it ms" to beat(state, it / 1000.0) }
        else -> {
            val step = when (state) {
                HeadState.THINKING -> 300
                HeadState.WORKING -> 125
                HeadState.FETCHING -> 100
                HeadState.COMMAND -> 140
                HeadState.LISTENING -> 150
                else -> 200
            }
            (0 until 8).map { "+${it * step} ms" to looping(state, 6.0 + it * step / 1000.0) }
        }
    }

    @Test
    fun `render the contact sheets`() {
        for ((theme, colors) in themes) {
            // Every state across its loop: the hero size, then the small one at phone pixels.
            sheet(
                if (theme == "paper") "app-head-states.png" else "app-head-states-$theme.png",
                HeadState.entries.flatMap { state ->
                    val frames = moments(state)
                    listOf(
                        "${state.name} 160dp" to frames.map { (caption, motion) -> Cell(render(colors, motion, 320), caption) },
                        "${state.name} 48dp" to frames.map { (caption, motion) -> Cell(render(colors, motion, 144), caption) },
                    )
                },
            )
        }
        // Every state at its still pose (what "remove animations" shows), in both themes.
        sheet(
            "app-head-themes.png",
            themes.flatMap { (theme, colors) ->
                listOf(
                    "$theme 160dp" to HeadState.entries.map { Cell(render(colors, still(it), 320), it.name.lowercase()) },
                    "$theme 48dp" to HeadState.entries.map { Cell(render(colors, still(it), 144), it.name.lowercase()) },
                    "$theme 28dp" to HeadState.entries.map { Cell(render(colors, still(it), 84), it.name.lowercase()) },
                )
            },
        )
        // The easing between states, at the times the device's sheet shows.
        val pairs = listOf(
            HeadState.IDLE to HeadState.THINKING, HeadState.THINKING to HeadState.WORKING,
            HeadState.WORKING to HeadState.FETCHING, HeadState.FETCHING to HeadState.COMMAND,
            HeadState.COMMAND to HeadState.APPROVAL, HeadState.APPROVAL to HeadState.PAUSED,
            HeadState.THINKING to HeadState.DONE, HeadState.WORKING to HeadState.ERROR,
        )
        sheet(
            "app-head-transitions.png",
            pairs.map { (from, to) ->
                "${from.name.take(5)}>${to.name.take(5)}" to listOf(0, 50, 100, 150, 200, 300, 450, 700).map {
                    Cell(render(themes.getValue("paper"), easing(from, to, it / 1000.0), 240), "$it ms")
                }
            },
        )
        // One of each, large, for a close look at the drawing itself.
        for ((theme, colors) in themes) {
            sheet(
                "app-head-close-$theme.png",
                HeadState.entries.chunked(5).map { row -> "" to row.map { Cell(render(colors, still(it), 480), it.name.lowercase()) } },
            )
        }
    }
}
