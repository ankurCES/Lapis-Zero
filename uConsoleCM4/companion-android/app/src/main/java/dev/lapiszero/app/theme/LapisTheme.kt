package dev.lapiszero.app.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The four identity colours shared with `lapis-ui` on the CardputerZero LCD. */
object LapisPalette {
    val Ink = Color(0xFF0B0B0D)
    val Paper = Color(0xFFF2F2EC)
    val Blue = Color(0xFF2D6BFF)
    val Amber = Color(0xFFFF6B2B)

    // Supporting tones, derived from Ink and Paper only.
    val InkRaised = Color(0xFF17171B)
    val InkLine = Color(0xFF3A3A42)
    val PaperSunken = Color(0xFFE3E3DB)
    val PaperLine = Color(0xFF8C8C84)
    val Danger = Color(0xFFD92D20)
    val DangerOnInk = Color(0xFFFF5A4D)
    val BlueOnInk = Color(0xFF6E9BFF)
}

enum class ThemeMode { SYSTEM, PAPER, INK }

/** Colours Material 3 has no role for. */
@Immutable
data class LapisColors(
    val ink: Color,
    val paper: Color,
    val blue: Color,
    val amber: Color,
    val danger: Color,
    val onAccent: Color,
    val muted: Color,
    val isInk: Boolean,
)

private val PaperExtras = LapisColors(
    ink = LapisPalette.Ink,
    paper = LapisPalette.Paper,
    blue = LapisPalette.Blue,
    amber = LapisPalette.Amber,
    danger = LapisPalette.Danger,
    onAccent = Color.White,
    muted = Color(0xFF55554F),
    isInk = false,
)

private val InkExtras = PaperExtras.copy(
    blue = LapisPalette.BlueOnInk,
    danger = LapisPalette.DangerOnInk,
    muted = Color(0xFFA9A9A2),
    isInk = true,
)

val LocalLapisColors = staticCompositionLocalOf { PaperExtras }

private val PaperScheme: ColorScheme = lightColorScheme(
    primary = LapisPalette.Blue,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFD9E4FF),
    onPrimaryContainer = LapisPalette.Ink,
    secondary = LapisPalette.Ink,
    onSecondary = LapisPalette.Paper,
    secondaryContainer = LapisPalette.PaperSunken,
    onSecondaryContainer = LapisPalette.Ink,
    tertiary = LapisPalette.Amber,
    onTertiary = LapisPalette.Ink,
    tertiaryContainer = Color(0xFFFFDCCB),
    onTertiaryContainer = LapisPalette.Ink,
    background = LapisPalette.Paper,
    onBackground = LapisPalette.Ink,
    surface = LapisPalette.Paper,
    onSurface = LapisPalette.Ink,
    surfaceVariant = LapisPalette.PaperSunken,
    onSurfaceVariant = Color(0xFF33332F),
    surfaceContainerLowest = Color.White,
    surfaceContainerLow = Color(0xFFF7F7F2),
    surfaceContainer = Color(0xFFECECE5),
    surfaceContainerHigh = LapisPalette.PaperSunken,
    surfaceContainerHighest = Color(0xFFDADAD2),
    outline = LapisPalette.Ink,
    outlineVariant = LapisPalette.PaperLine,
    error = LapisPalette.Danger,
    onError = Color.White,
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
    inverseSurface = LapisPalette.Ink,
    inverseOnSurface = LapisPalette.Paper,
    inversePrimary = LapisPalette.BlueOnInk,
    scrim = LapisPalette.Ink,
)

private val InkScheme: ColorScheme = darkColorScheme(
    primary = LapisPalette.Blue,
    onPrimary = Color.White,
    primaryContainer = Color(0xFF14285C),
    onPrimaryContainer = LapisPalette.Paper,
    secondary = LapisPalette.Paper,
    onSecondary = LapisPalette.Ink,
    secondaryContainer = LapisPalette.InkRaised,
    onSecondaryContainer = LapisPalette.Paper,
    tertiary = LapisPalette.Amber,
    onTertiary = LapisPalette.Ink,
    tertiaryContainer = Color(0xFF5A2410),
    onTertiaryContainer = LapisPalette.Paper,
    background = LapisPalette.Ink,
    onBackground = LapisPalette.Paper,
    surface = LapisPalette.Ink,
    onSurface = LapisPalette.Paper,
    surfaceVariant = LapisPalette.InkRaised,
    onSurfaceVariant = Color(0xFFC9C9C2),
    surfaceContainerLowest = Color(0xFF050506),
    surfaceContainerLow = Color(0xFF111114),
    surfaceContainer = LapisPalette.InkRaised,
    surfaceContainerHigh = Color(0xFF1F1F24),
    surfaceContainerHighest = Color(0xFF2A2A30),
    outline = LapisPalette.Paper,
    outlineVariant = LapisPalette.InkLine,
    error = LapisPalette.DangerOnInk,
    onError = LapisPalette.Ink,
    errorContainer = Color(0xFF5C1210),
    onErrorContainer = Color(0xFFFFDAD6),
    inverseSurface = LapisPalette.Paper,
    inverseOnSurface = LapisPalette.Ink,
    inversePrimary = LapisPalette.Blue,
    scrim = Color.Black,
)

/** Crisp, hardware-like corners. */
private val LapisShapes = Shapes(
    extraSmall = RoundedCornerShape(2.dp),
    small = RoundedCornerShape(4.dp),
    medium = RoundedCornerShape(6.dp),
    large = RoundedCornerShape(8.dp),
    extraLarge = RoundedCornerShape(12.dp),
)

object LapisType {
    /** Tool logs, coordinates, identifiers. */
    val Mono = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 13.sp, lineHeight = 18.sp)
    val MonoSmall = Mono.copy(fontSize = 11.sp, lineHeight = 15.sp)
    val MonoLabel = Mono.copy(fontSize = 11.sp, lineHeight = 14.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.5.sp)
    val MonoLarge = Mono.copy(fontSize = 28.sp, lineHeight = 34.sp, fontWeight = FontWeight.Bold, letterSpacing = 4.sp)
}

private val LapisTypography = Typography().run {
    copy(
        titleLarge = titleLarge.copy(fontWeight = FontWeight.Bold),
        titleMedium = titleMedium.copy(fontWeight = FontWeight.Bold),
        titleSmall = titleSmall.copy(fontWeight = FontWeight.Bold),
        labelLarge = labelLarge.copy(fontWeight = FontWeight.Bold),
    )
}

@Composable
fun LapisTheme(mode: ThemeMode = ThemeMode.SYSTEM, content: @Composable () -> Unit) {
    val ink = when (mode) {
        ThemeMode.SYSTEM -> isSystemInDarkTheme()
        ThemeMode.PAPER -> false
        ThemeMode.INK -> true
    }
    CompositionLocalProvider(LocalLapisColors provides if (ink) InkExtras else PaperExtras) {
        MaterialTheme(
            colorScheme = if (ink) InkScheme else PaperScheme,
            shapes = LapisShapes,
            typography = LapisTypography,
            content = content,
        )
    }
}

val lapis: LapisColors
    @Composable get() = LocalLapisColors.current
