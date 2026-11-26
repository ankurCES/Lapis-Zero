package dev.lapiszero.app.ui.components

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.lapiszero.app.theme.LapisPalette
import kotlin.math.cos
import kotlin.math.sin

/**
 * The mark of who answers in a chat: the device's own agent (`lapis`) or an agent tool found on
 * the host (`claude`, `opencode`, `codex`, `gemini`, `cursor`, `freya`; ids as in the daemon's
 * `agent/harness.py`). Anything else, a custom tool, gets its initial.
 *
 * ponytail: simple drawn marks in each tool's colours, not the vendors' artwork; the official
 * SVGs can replace a branch here one by one if the project gets them.
 * Decorative: the tool's name is always written next to it.
 */
@Composable
fun AgentLogo(agent: String, modifier: Modifier = Modifier, size: Dp = 20.dp) {
    val ink = MaterialTheme.colorScheme.onSurface
    val paper = MaterialTheme.colorScheme.surface
    val text = rememberTextMeasurer()
    Canvas(modifier.size(size)) {
        when (agent) {
            "lapis" -> lapis()
            "claude" -> claude()
            "gemini" -> gemini()
            "codex" -> codex(ink)
            "cursor" -> cursor(ink)
            "opencode" -> opencode(ink, paper)
            "freya" -> freya()
            else -> initial(agent, ink, text)
        }
    }
}

private val ClaudeClay = Color(0xFFD97757)

/** Lapis lazuli: a cut stone in the project's blue, one facet lit. */
private fun DrawScope.lapis() {
    val s = size.minDimension
    val c = center
    val stone = Path().apply {
        moveTo(c.x, c.y - s * 0.46f)
        lineTo(c.x + s * 0.40f, c.y - s * 0.08f)
        lineTo(c.x, c.y + s * 0.46f)
        lineTo(c.x - s * 0.40f, c.y - s * 0.08f)
        close()
    }
    drawPath(stone, LapisPalette.Blue)
    val facet = Path().apply {
        moveTo(c.x, c.y - s * 0.46f)
        lineTo(c.x + s * 0.40f, c.y - s * 0.08f)
        lineTo(c.x, c.y + s * 0.02f)
        lineTo(c.x - s * 0.40f, c.y - s * 0.08f)
        close()
    }
    drawPath(facet, Color.White.copy(alpha = 0.28f))
}

/** A burst of rounded rays of uneven length, in Claude's clay. */
private fun DrawScope.claude() {
    val s = size.minDimension
    val lengths = floatArrayOf(0.47f, 0.36f, 0.44f, 0.33f, 0.46f, 0.38f, 0.45f, 0.34f, 0.47f, 0.37f, 0.43f, 0.35f)
    lengths.forEachIndexed { i, len ->
        val a = Math.toRadians(i * 30.0 - 90.0)
        val end = Offset(center.x + (cos(a) * len * s).toFloat(), center.y + (sin(a) * len * s).toFloat())
        drawLine(ClaudeClay, center, end, strokeWidth = s * 0.11f, cap = StrokeCap.Round)
    }
}

/** Four-pointed sparkle with curved sides, blue to violet to rose. */
private fun DrawScope.gemini() {
    val s = size.minDimension
    val c = center
    val r = s * 0.48f
    val k = s * 0.06f // how far the sides bow out from the centre
    val star = Path().apply {
        moveTo(c.x, c.y - r)
        quadraticTo(c.x + k, c.y - k, c.x + r, c.y)
        quadraticTo(c.x + k, c.y + k, c.x, c.y + r)
        quadraticTo(c.x - k, c.y + k, c.x - r, c.y)
        quadraticTo(c.x - k, c.y - k, c.x, c.y - r)
        close()
    }
    drawPath(
        star,
        Brush.linearGradient(
            listOf(Color(0xFF4796E3), Color(0xFF9177C7), Color(0xFFCA6673)),
            start = Offset(c.x - r, c.y - r),
            end = Offset(c.x + r, c.y + r),
        ),
    )
}

/** Six interlaced links turning about the centre, in the theme's ink. */
private fun DrawScope.codex(ink: Color) {
    val s = size.minDimension
    val w = s * 0.24f
    val h = s * 0.50f
    repeat(6) { i ->
        rotate(i * 60f) {
            drawRoundRect(
                ink,
                topLeft = Offset(center.x - w * 0.15f, center.y - h * 0.86f),
                size = Size(w, h),
                cornerRadius = CornerRadius(w / 2),
                style = Stroke(width = s * 0.065f),
            )
        }
    }
}

/** A cube seen corner on: three faces in three tones of ink. */
private fun DrawScope.cursor(ink: Color) {
    val s = size.minDimension
    val c = center
    val r = s * 0.47f
    fun corner(deg: Double) = Offset(c.x + (cos(Math.toRadians(deg)) * r).toFloat(), c.y + (sin(Math.toRadians(deg)) * r).toFloat())
    val top = corner(-90.0)
    val upperRight = corner(-30.0)
    val lowerRight = corner(30.0)
    val bottom = corner(90.0)
    val lowerLeft = corner(150.0)
    val upperLeft = corner(210.0)
    fun face(p: List<Offset>) = Path().apply {
        moveTo(p[0].x, p[0].y)
        p.drop(1).forEach { lineTo(it.x, it.y) }
        close()
    }
    drawPath(face(listOf(top, upperRight, c, upperLeft)), ink.copy(alpha = 0.55f))
    drawPath(face(listOf(upperRight, lowerRight, bottom, c)), ink)
    drawPath(face(listOf(upperLeft, c, bottom, lowerLeft)), ink.copy(alpha = 0.8f))
}

/** A square within a square, block-cut like a terminal glyph. */
private fun DrawScope.opencode(ink: Color, paper: Color) {
    val s = size.minDimension
    val o = Offset(center.x - s * 0.44f, center.y - s * 0.44f)
    drawRoundRect(ink, o, Size(s * 0.88f, s * 0.88f), CornerRadius(s * 0.12f))
    drawRect(paper, Offset(o.x + s * 0.18f, o.y + s * 0.18f), Size(s * 0.52f, s * 0.52f))
    drawRect(ink.copy(alpha = 0.55f), Offset(o.x + s * 0.30f, o.y + s * 0.40f), Size(s * 0.28f, s * 0.18f))
}

/** A gold crescent and a star on a night-blue disc. */
private fun DrawScope.freya() {
    val s = size.minDimension
    val night = Color(0xFF1B2A4E)
    val gold = Color(0xFFF2C94C)
    drawCircle(night, s * 0.48f)
    drawCircle(gold, s * 0.27f, Offset(center.x - s * 0.04f, center.y + s * 0.02f))
    drawCircle(night, s * 0.24f, Offset(center.x + s * 0.08f, center.y - s * 0.06f))
    drawCircle(gold, s * 0.045f, Offset(center.x + s * 0.20f, center.y + s * 0.14f))
}

/** A custom tool: its initial in a ring. */
private fun DrawScope.initial(agent: String, ink: Color, text: TextMeasurer) {
    val s = size.minDimension
    drawCircle(ink, s * 0.44f, style = Stroke(width = s * 0.08f))
    val letter = agent.firstOrNull { it.isLetterOrDigit() }?.uppercase() ?: "?"
    val layout = text.measure(
        letter,
        TextStyle(color = ink, fontSize = (s * 0.5f / density / fontScale).sp, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold),
    )
    drawText(layout, topLeft = Offset(center.x - layout.size.width / 2f, center.y - layout.size.height / 2f))
}
