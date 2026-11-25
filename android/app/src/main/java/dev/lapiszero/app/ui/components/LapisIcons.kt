package dev.lapiszero.app.ui.components

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathFillType
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.unit.dp

/** Blocky, pixel-ish glyphs for the bottom bar (the core icon set has no monitor or chip). */
object LapisIcons {
    private fun icon(name: String, block: ImageVector.Builder.() -> Unit): ImageVector =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f).apply(block).build()

    private fun ImageVector.Builder.shape(d: androidx.compose.ui.graphics.vector.PathBuilder.() -> Unit) {
        path(fill = SolidColor(Color.Black), pathFillType = PathFillType.EvenOdd, pathBuilder = d)
    }

    private fun androidx.compose.ui.graphics.vector.PathBuilder.rect(x: Float, y: Float, w: Float, h: Float) {
        moveTo(x, y)
        horizontalLineTo(x + w)
        verticalLineTo(y + h)
        horizontalLineTo(x)
        close()
    }

    /** Speech bubble with two text lines. */
    val Chat: ImageVector by lazy {
        icon("LapisChat") {
            shape {
                rect(3f, 4f, 18f, 13f)
                rect(5f, 6f, 14f, 9f)
                moveTo(6f, 17f)
                lineTo(11f, 17f)
                lineTo(6f, 21f)
                close()
            }
            shape {
                rect(7f, 8f, 10f, 2f)
                rect(7f, 11f, 6f, 2f)
            }
        }
    }

    /** Monitor with a cursor block. */
    val Monitor: ImageVector by lazy {
        icon("LapisMonitor") {
            shape {
                rect(2f, 4f, 20f, 13f)
                rect(4f, 6f, 16f, 9f)
            }
            shape {
                rect(10f, 17f, 4f, 2f)
                rect(7f, 19f, 10f, 2f)
                rect(12f, 9f, 5f, 3f)
            }
        }
    }

    /** The CardputerZero: a board with a screen and a keyboard row. */
    val Chip: ImageVector by lazy {
        icon("LapisChip") {
            shape {
                rect(3f, 5f, 18f, 14f)
                rect(5f, 7f, 14f, 6f)
            }
            shape {
                rect(6f, 15f, 2f, 2f)
                rect(9f, 15f, 2f, 2f)
                rect(12f, 15f, 2f, 2f)
                rect(15f, 15f, 3f, 2f)
            }
        }
    }
}
