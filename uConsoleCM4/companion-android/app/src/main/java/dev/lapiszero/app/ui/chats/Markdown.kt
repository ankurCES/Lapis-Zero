package dev.lapiszero.app.ui.chats

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis

/**
 * The Markdown an agent answers in, as far as chat answers use it: headings, paragraphs, lists
 * (nested by indent), quotes, rules, fenced code; bold, italic, strike, inline code and links.
 * ponytail: no tables, images or HTML (they show as their text); add a library if answers need them.
 */
sealed interface MdBlock {
    data class Paragraph(val text: String) : MdBlock
    data class Heading(val level: Int, val text: String) : MdBlock
    data class Item(val depth: Int, val marker: String, val text: String) : MdBlock
    data class Quote(val text: String) : MdBlock
    data class Code(val language: String, val code: String) : MdBlock
    data object Rule : MdBlock
}

private val HEADING = Regex("""^(#{1,6})\s+(.*?)\s*#*\s*$""")
private val BULLET = Regex("""^(\s*)[-*+]\s+(.*)$""")
private val ORDERED = Regex("""^(\s*)(\d{1,9})[.)]\s+(.*)$""")
private val RULE = Regex("""^\s{0,3}([-*_])(\s*\1){2,}\s*$""")
private val FENCE = Regex("""^\s{0,3}(```+|~~~+)\s*([\w+.#-]*).*$""")

fun parseMarkdown(source: String): List<MdBlock> {
    val out = mutableListOf<MdBlock>()
    val paragraph = mutableListOf<String>()
    val quote = mutableListOf<String>()
    fun flush() {
        if (paragraph.isNotEmpty()) out += MdBlock.Paragraph(paragraph.joinToString("\n")).also { paragraph.clear() }
        if (quote.isNotEmpty()) out += MdBlock.Quote(quote.joinToString("\n")).also { quote.clear() }
    }
    val lines = source.replace("\r\n", "\n").split('\n')
    var i = 0
    while (i < lines.size) {
        val line = lines[i]
        val fence = FENCE.matchEntire(line)
        when {
            fence != null -> {
                flush()
                val mark = fence.groupValues[1]
                val code = mutableListOf<String>()
                i++
                // An unclosed fence runs to the end, as it does while an answer is still arriving.
                while (i < lines.size && !lines[i].trimStart().startsWith(mark)) code += lines[i++]
                out += MdBlock.Code(fence.groupValues[2], code.joinToString("\n"))
            }
            line.isBlank() -> flush()
            RULE.matches(line) -> { flush(); out += MdBlock.Rule }
            HEADING.matches(line) -> {
                flush()
                val m = HEADING.matchEntire(line)!!
                out += MdBlock.Heading(m.groupValues[1].length, m.groupValues[2])
            }
            line.trimStart().startsWith(">") -> {
                if (paragraph.isNotEmpty()) flush()
                quote += line.trimStart().removePrefix(">").removePrefix(" ")
            }
            BULLET.matches(line) || ORDERED.matches(line) -> {
                flush()
                val b = BULLET.matchEntire(line)
                out += if (b != null) {
                    MdBlock.Item(b.groupValues[1].length / 2, "•", b.groupValues[2])
                } else {
                    val o = ORDERED.matchEntire(line)!!
                    MdBlock.Item(o.groupValues[1].length / 2, o.groupValues[2] + ".", o.groupValues[3])
                }
            }
            // An indented line under a list item continues that item.
            out.lastOrNull() is MdBlock.Item && paragraph.isEmpty() && line.startsWith("  ") -> {
                val last = out.removeAt(out.lastIndex) as MdBlock.Item
                out += last.copy(text = last.text + " " + line.trim())
            }
            else -> {
                if (quote.isNotEmpty()) flush()
                paragraph += line
            }
        }
        i++
    }
    flush()
    return out
}

/** Colours of the inline marks, from the theme. */
data class MdStyle(val code: SpanStyle, val link: SpanStyle)

private val URL = Regex("""https?://[^\s<>()\[\]"']+[^\s<>()\[\]"'.,;:!?]""")

/** Inline marks into styled text. Only http(s) links become links; anything else stays text. */
fun parseInline(text: String, style: MdStyle): AnnotatedString = buildAnnotatedString { inline(text, style) }

private val PLAIN = MdStyle(SpanStyle(), SpanStyle())

/** An answer as one plain line, for the chat list: marks dropped, blocks run together. */
fun markdownPlain(source: String): String = parseMarkdown(source).mapNotNull {
    when (it) {
        is MdBlock.Paragraph -> it.text
        is MdBlock.Heading -> it.text
        is MdBlock.Item -> it.text
        is MdBlock.Quote -> it.text
        is MdBlock.Code -> it.code
        MdBlock.Rule -> null
    }
}.joinToString(" ") { parseInline(it, PLAIN).text.replace('\n', ' ') }

private fun AnnotatedString.Builder.inline(text: String, style: MdStyle) {
    var i = 0
    fun closes(mark: String, from: Int): Int {
        val end = text.indexOf(mark, from)
        return if (end > from && !text[end - 1].isWhitespace()) end else -1
    }
    while (i < text.length) {
        val c = text[i]
        val rest = text.length - i
        when {
            c == '\\' && rest > 1 && text[i + 1] in "\\`*_[]~#>-" -> { append(text[i + 1]); i += 2 }
            c == '`' -> {
                val end = text.indexOf('`', i + 1)
                if (end < 0) { append(c); i++ } else {
                    withStyle(style.code) { append(text.substring(i + 1, end)) }
                    i = end + 1
                }
            }
            (text.startsWith("**", i) || text.startsWith("__", i)) && rest > 4 && !text[i + 2].isWhitespace() -> {
                val end = closes(text.substring(i, i + 2), i + 3)
                if (end < 0) { append(text.substring(i, i + 2)); i += 2 } else {
                    withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { inline(text.substring(i + 2, end), style) }
                    i = end + 2
                }
            }
            text.startsWith("~~", i) && rest > 4 -> {
                val end = closes("~~", i + 3)
                if (end < 0) { append("~~"); i += 2 } else {
                    withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) { inline(text.substring(i + 2, end), style) }
                    i = end + 2
                }
            }
            // `_` only at a word's edge: snake_case names stay as they are.
            (c == '*' || (c == '_' && (i == 0 || !text[i - 1].isLetterOrDigit()))) && rest > 2 && !text[i + 1].isWhitespace() -> {
                val end = closes(c.toString(), i + 2)
                val atEdge = end > 0 && (c == '*' || end + 1 >= text.length || !text[end + 1].isLetterOrDigit())
                if (!atEdge) { append(c); i++ } else {
                    withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { inline(text.substring(i + 1, end), style) }
                    i = end + 1
                }
            }
            c == '[' -> {
                val close = text.indexOf("](", i)
                val end = if (close > i) text.indexOf(')', close) else -1
                val url = if (end > 0) text.substring(close + 2, end).trim() else ""
                if (end < 0 || !(url.startsWith("https://") || url.startsWith("http://"))) { append(c); i++ } else {
                    withLink(LinkAnnotation.Url(url, TextLinkStyles(style.link))) { inline(text.substring(i + 1, close), style) }
                    i = end + 1
                }
            }
            c == 'h' && (text.startsWith("http://", i) || text.startsWith("https://", i)) -> {
                val url = URL.matchAt(text, i)?.value
                if (url == null) { append(c); i++ } else {
                    withLink(LinkAnnotation.Url(url, TextLinkStyles(style.link))) { append(url) }
                    i += url.length
                }
            }
            else -> { append(c); i++ }
        }
    }
}

/** An agent's answer, rendered; the whole of it can be selected and copied. */
@Composable
fun MarkdownText(text: String, modifier: Modifier = Modifier) {
    val blocks = remember(text) { parseMarkdown(text) }
    val code = MaterialTheme.colorScheme.surfaceContainerHighest
    val style = MdStyle(
        code = SpanStyle(fontFamily = FontFamily.Monospace, background = code),
        link = SpanStyle(color = lapis.blue, textDecoration = TextDecoration.Underline),
    )
    val body = MaterialTheme.typography.bodyLarge
    SelectionContainer(modifier) {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            blocks.forEach { block ->
                when (block) {
                    is MdBlock.Paragraph -> Text(parseInline(block.text, style), style = body)
                    is MdBlock.Heading -> Text(
                        parseInline(block.text, style),
                        style = when (block.level) {
                            1 -> MaterialTheme.typography.titleLarge
                            2 -> MaterialTheme.typography.titleMedium
                            else -> MaterialTheme.typography.titleSmall
                        },
                        modifier = Modifier.padding(top = 4.dp),
                    )
                    is MdBlock.Item -> Row(Modifier.padding(start = (block.depth * 16).dp)) {
                        Text(block.marker, style = body, color = lapis.muted, modifier = Modifier.width(if (block.marker == "•") 16.dp else 24.dp))
                        Text(parseInline(block.text, style), style = body)
                    }
                    is MdBlock.Quote -> Row(Modifier.height(IntrinsicSize.Min)) {
                        Box(Modifier.width(3.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                        Spacer(Modifier.width(10.dp))
                        Text(parseInline(block.text, style), style = body, color = lapis.muted)
                    }
                    is MdBlock.Code -> CodeBlock(block)
                    MdBlock.Rule -> HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
            }
        }
    }
}

@Composable
private fun CodeBlock(block: MdBlock.Code) {
    val context = LocalContext.current
    var copied by remember(block) { mutableStateOf(false) }
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Row(Modifier.fillMaxWidth().padding(start = 12.dp), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Text(block.language.ifEmpty { "code" }, style = LapisType.MonoSmall, color = lapis.muted, modifier = Modifier.weight(1f))
                TextButton(
                    onClick = {
                        context.getSystemService(ClipboardManager::class.java)
                            ?.setPrimaryClip(ClipData.newPlainText("Code", block.code))
                        copied = true
                    },
                ) { Text(if (copied) "Copied" else "Copy", style = LapisType.MonoSmall) }
            }
            Text(
                block.code,
                style = LapisType.Mono,
                color = MaterialTheme.colorScheme.onSurface,
                softWrap = false,
                modifier = Modifier
                    .horizontalScroll(rememberScrollState())
                    .padding(start = 12.dp, end = 12.dp, bottom = 12.dp),
            )
        }
    }
}
