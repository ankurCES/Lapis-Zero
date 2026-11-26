package dev.lapiszero.app.ui

import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.test.junit4.v2.createEmptyComposeRule
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import dev.lapiszero.app.data.remote.Message
import dev.lapiszero.app.theme.LapisTheme
import dev.lapiszero.app.theme.ThemeMode
import dev.lapiszero.app.ui.chats.MarkdownText
import dev.lapiszero.app.ui.chats.MdBlock
import dev.lapiszero.app.ui.chats.MdStyle
import dev.lapiszero.app.ui.chats.ThreadItem
import dev.lapiszero.app.ui.chats.groupThread
import dev.lapiszero.app.ui.chats.markdownPlain
import dev.lapiszero.app.ui.chats.parseInline
import dev.lapiszero.app.ui.chats.parseMarkdown
import dev.lapiszero.app.ui.components.AgentLogo
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/** How answers are read (Markdown), how a thread folds its tool steps, and a look at both. */
@RunWith(RobolectricTestRunner::class)
class ChatLookTest {
    @get:Rule
    val compose = createEmptyComposeRule()

    private val style = MdStyle(code = SpanStyle(), link = SpanStyle())

    @Test
    fun `blocks of an answer are told apart`() {
        val blocks = parseMarkdown(
            """
            # Done
            I changed **two** files:

            - `ui/src/main.rs`
              and its test
            - daemon/core.py
              1. nested
            > careful
            ---
            ```rust
            fn main() {}
            ```
            """.trimIndent(),
        )
        assertEquals(
            listOf(
                MdBlock.Heading(1, "Done"),
                MdBlock.Paragraph("I changed **two** files:"),
                MdBlock.Item(0, "•", "`ui/src/main.rs` and its test"),
                MdBlock.Item(0, "•", "daemon/core.py"),
                MdBlock.Item(1, "1.", "nested"),
                MdBlock.Quote("careful"),
                MdBlock.Rule,
                MdBlock.Code("rust", "fn main() {}"),
            ),
            blocks,
        )
        // An answer still arriving: the open fence runs to the end.
        assertEquals(listOf(MdBlock.Code("", "half")), parseMarkdown("```\nhalf"))
        // The chat list's one-line preview carries no marks.
        assertEquals(
            "Done I changed two files: ui/src/main.rs and its test",
            markdownPlain("# Done\nI changed **two** files:\n\n- `ui/src/main.rs`\n  and its test\n---"),
        )
    }

    @Test
    fun `inline marks are styled and only web links are links`() {
        val text = parseInline("a **bold** and *it* `code_x` my_var_name [site](https://example.com) [bad](javascript:alert(1)) https://lapis.dev/x.", style)
        assertEquals("a bold and it code_x my_var_name site [bad](javascript:alert(1)) https://lapis.dev/x.", text.text)
        assertTrue(text.spanStyles.any { it.item.fontWeight == FontWeight.Bold && text.text.substring(it.start, it.end) == "bold" })
        val links = text.getLinkAnnotations(0, text.length).map { (it.item as LinkAnnotation.Url).url }
        assertEquals(listOf("https://example.com", "https://lapis.dev/x"), links)
    }

    @Test
    fun `consecutive tool steps fold into one block`() {
        fun m(id: Long, role: String) = Message(id = id, role = role, content = "m$id")
        val items = groupThread(
            listOf(m(1, Message.ROLE_USER), m(2, Message.ROLE_TOOL), m(3, Message.ROLE_TOOL), m(4, Message.ROLE_ASSISTANT), m(5, Message.ROLE_TOOL)),
        )
        assertEquals(listOf("1", "steps-2", "4", "steps-5"), items.map { it.key.toString() })
        assertEquals(2, (items[1] as ThreadItem.Steps).steps.size)
    }

    /** Not a check: the logos and a rendered answer, for looking at (`-Plapis.head.sheets=<dir>`). */
    @Test
    fun `render the logos and an answer`() {
        val out = System.getProperty("lapis.head.sheets")?.takeIf { it.isNotBlank() }?.let(::File)
        assumeTrue("set -Plapis.head.sheets=<directory> to render", out != null)
        val context = ApplicationProvider.getApplicationContext<Context>()
        shadowOf(context.packageManager).addActivityIfNotPresent(ComponentName(context, ComponentActivity::class.java))
        for (mode in listOf(ThemeMode.PAPER, ThemeMode.INK)) {
            ActivityScenario.launch(ComponentActivity::class.java).use { scenario ->
                scenario.onActivity { activity ->
                    activity.setContent {
                        LapisTheme(mode) {
                            // A Surface, as in the app: it gives text the theme's colour.
                            Surface(Modifier.fillMaxSize()) {
                            Column(
                                Modifier.fillMaxSize().padding(16.dp),
                                verticalArrangement = Arrangement.spacedBy(12.dp),
                            ) {
                                for (size in listOf(16, 22, 48)) {
                                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                                        listOf("lapis", "claude", "opencode", "codex", "gemini", "cursor", "freya", "mytool").forEach {
                                            AgentLogo(it, size = size.dp)
                                        }
                                    }
                                }
                                Text("lapis · claude · opencode · codex · gemini · cursor · freya · custom", style = MaterialTheme.typography.labelSmall)
                                MarkdownText(SAMPLE)
                            }
                            }
                        }
                    }
                }
                compose.waitForIdle()
                scenario.onActivity { activity ->
                    val view = activity.window.decorView
                    val bitmap = Bitmap.createBitmap(view.width, view.height, Bitmap.Config.ARGB_8888)
                    view.draw(android.graphics.Canvas(bitmap))
                    File(out, "chat-look-${mode.name.lowercase()}.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
                }
            }
        }
    }

    private companion object {
        val SAMPLE = """
            ## Build fixed
            The failing test was **`test_vnc_settle`**: the frame arrived *before* the timeout. See [the log](https://example.com/log).

            1. Raised `FRAME_SETTLE_S` to 0.5
            2. Re-ran the suite: 283 passed
               - one xfail, as before

            ```python
            FRAME_SETTLE_S = 0.5
            assert frame.covered >= 0.9
            ```
            > Nothing was pushed.
        """.trimIndent()
    }
}
