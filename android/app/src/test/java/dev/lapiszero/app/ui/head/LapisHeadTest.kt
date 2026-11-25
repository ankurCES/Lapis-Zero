package dev.lapiszero.app.ui.head

import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.MotionDurationScale
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.junit4.v2.createEmptyComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import dev.lapiszero.app.domain.HeadState
import dev.lapiszero.app.theme.LapisTheme
import dev.lapiszero.app.theme.ThemeMode
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/**
 * The composable itself, drawn for real (Robolectric's native graphics) under a frame clock the
 * test steps by hand: that it moves, that it stops, and that it names its state.
 */
@RunWith(RobolectricTestRunner::class)
class LapisHeadTest {
    /** What the system's "remove animations" setting reaches the composition as: 0 when it is on. */
    private val motionScale = object : MotionDurationScale {
        override var scaleFactor by mutableFloatStateOf(1f)
    }

    @get:Rule
    val compose = createEmptyComposeRule(effectContext = motionScale)

    private lateinit var scenario: ActivityScenario<ComponentActivity>
    private var state by mutableStateOf(HeadState.WORKING)
    private val motion = HeadMotion()

    @Before
    fun setUp() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        shadowOf(context.packageManager).addActivityIfNotPresent(ComponentName(context, ComponentActivity::class.java))
        scenario = ActivityScenario.launch(ComponentActivity::class.java)
        // By hand: with the clock advancing by itself the test rule cancels every endless animation.
        compose.mainClock.autoAdvance = false
    }

    @After
    fun tearDown() = scenario.close()

    /** The head at the top of a page long enough to scroll it out of the window. */
    private fun show() {
        scenario.onActivity { activity ->
            activity.setContent {
                LapisTheme(ThemeMode.PAPER) {
                    Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).verticalScroll(rememberScrollState())) {
                        LapisHead(state, size = 96.dp, motion = motion)
                        Spacer(Modifier.height(2000.dp))
                    }
                }
            }
        }
        compose.mainClock.advanceTimeBy(100)
    }

    private fun scrollBy(pixels: Float) {
        compose.onNode(hasScrollAction()).performSemanticsAction(SemanticsActions.ScrollBy) { it(0f, pixels) }
    }

    private fun head() = compose.onNodeWithContentDescription(headDescription(state))

    /** Lets what the test just changed reach the composition, then runs the clock for `ms`. */
    private fun run(ms: Long) {
        compose.waitForIdle()
        compose.mainClock.advanceTimeBy(ms)
    }

    /**
     * The window as it would be drawn now. (`captureToImage` waits for a frame from a display,
     * and Robolectric has none; drawing the view into a bitmap needs nothing but the view.)
     */
    private fun picture(save: String? = null): IntArray {
        lateinit var bitmap: Bitmap
        scenario.onActivity { activity ->
            val view = activity.window.decorView
            bitmap = Bitmap.createBitmap(view.width, view.height, Bitmap.Config.ARGB_8888)
            view.draw(android.graphics.Canvas(bitmap))
        }
        // With the contact sheets asked for, the composable's own picture goes next to them.
        val sheets = System.getProperty("lapis.head.sheets")
        if (save != null && !sheets.isNullOrBlank()) {
            File(sheets, save).outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
        }
        return IntArray(bitmap.width * bitmap.height).also { bitmap.getPixels(it, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height) }
    }

    @Test
    fun `it names its state`() {
        show()
        head().assertIsDisplayed()
        assertEquals("Lapis agent: working", headDescription(HeadState.WORKING))
        state = HeadState.APPROVAL
        run(50)
        compose.onNodeWithContentDescription("Lapis agent: waiting for approval").assertIsDisplayed()
        assertEquals(HeadState.entries.size, HeadState.entries.map(::headDescription).toSet().size)
    }

    @Test
    fun `its clock stops while it is scrolled out of the window`() {
        show()
        run(200)
        assertTrue(motion.live)
        val shown = motion.t
        run(200)
        assertEquals("the clock runs while the head is on screen", 0.2, motion.t - shown, 0.05)

        scrollBy(1500f)
        run(100) // the frame that finds it gone
        val gone = motion.t
        run(1000)
        assertEquals("no frame reaches a head nobody can see", gone, motion.t, 0.0)

        scrollBy(-1500f)
        run(300)
        assertTrue("the clock starts again when the head is back", motion.t - gone > 1.0)
        val back = motion.t
        run(200)
        assertEquals(0.2, motion.t - back, 0.05)
    }

    @Test
    fun `it moves with the frame clock and holds still when animations are removed`() {
        show()
        val first = picture(save = "app-head-composable.png")
        assertTrue("the head is drawn", first.toSet().size > 8)
        // A quarter of a second on: the other hand is on the keyboard.
        run(250)
        val later = picture()
        assertFalse("the head did not move with the clock", first.contentEquals(later))

        // "Remove animations" turned on while it runs: one still pose, whatever the clock does.
        motionScale.scaleFactor = 0f
        run(50)
        val still = picture()
        run(700)
        assertTrue("a still head moved", still.contentEquals(picture()))
        // Every state has its own still pose, and a change of state shows at once.
        state = HeadState.ERROR
        run(50)
        val error = picture()
        assertFalse("the new state did not show", still.contentEquals(error))
        run(700)
        assertTrue("a still head moved", error.contentEquals(picture()))

        // And off again: it moves.
        motionScale.scaleFactor = 1f
        state = HeadState.WORKING
        run(1200)
        val moving = picture()
        run(250)
        assertFalse("the head did not start moving again", moving.contentEquals(picture()))
    }
}
