package dev.lapiszero.app.ui.components

import android.graphics.Bitmap
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.domain.Preview1Bit
import dev.lapiszero.app.domain.ScreenshotStore
import dev.lapiszero.app.domain.Shot
import dev.lapiszero.app.theme.LapisPalette
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.kvm.ClickMarker
import java.io.IOException

sealed interface ShotUi {
    data object Loading : ShotUi
    class Ready(val shot: Shot) : ShotUi
    data class Failed(val message: String) : ShotUi
}

/** Loads `/v1/screenshots/{id}.jpg` through the session's cache. */
@Composable
fun rememberShot(store: ScreenshotStore, id: Long?): ShotUi? {
    if (id == null) return null
    val state by produceState<ShotUi>(store.peek(id)?.let { ShotUi.Ready(it) } ?: ShotUi.Loading, store, id) {
        value = try {
            ShotUi.Ready(store.load(id))
        } catch (e: IOException) {
            ShotUi.Failed(ChatRepository.describe(e))
        }
    }
    return state
}

/** Small tappable preview of a target-screen frame inside a tool-call card. */
@Composable
fun ScreenshotThumb(store: ScreenshotStore, id: Long, modifier: Modifier = Modifier, onClick: () -> Unit) {
    val shape = MaterialTheme.shapes.small
    Box(
        modifier
            .aspectRatio(16f / 9f)
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, shape)
            .background(MaterialTheme.colorScheme.surfaceContainerHigh, shape)
            .clickable(onClickLabel = "Open screenshot", onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        when (val shot = rememberShot(store, id)) {
            is ShotUi.Ready -> Image(
                bitmap = shot.shot.bitmap.asImageBitmap(),
                contentDescription = "Target screen at this step",
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )

            is ShotUi.Failed -> Text(
                "frame unavailable",
                style = LapisType.MonoSmall,
                color = lapis.muted,
                modifier = Modifier.padding(4.dp),
            )

            else -> CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
        }
    }
}

/**
 * A target-screen frame with overlays in screenshot pixel coordinates: numbered click markers,
 * an optional crosshair, pinch-zoom / pan, and an optional tap callback (manual assist).
 */
@Composable
fun ScreenshotCanvas(
    shot: Shot,
    modifier: Modifier = Modifier,
    markers: List<ClickMarker> = emptyList(),
    crosshair: Pair<Int, Int>? = null,
    zoomable: Boolean = false,
    onTap: ((x: Int, y: Int) -> Unit)? = null,
) {
    val bitmap = shot.bitmap
    val image = remember(bitmap) { bitmap.asImageBitmap() }
    var scale by remember(bitmap) { mutableFloatStateOf(1f) }
    var offset by remember(bitmap) { mutableStateOf(Offset.Zero) }
    val measurer = rememberTextMeasurer()
    val amber = lapis.amber
    val blue = LapisPalette.Blue

    var box by remember { mutableStateOf(IntSize.Zero) }
    val transform = rememberTransformableState { _, zoom, pan, _ ->
        val next = (scale * zoom).coerceIn(1f, 6f)
        val maxX = box.width * (next - 1) / 2
        val maxY = box.height * (next - 1) / 2
        scale = next
        offset = Offset(
            (offset.x + pan.x).coerceIn(-maxX, maxX),
            (offset.y + pan.y).coerceIn(-maxY, maxY),
        )
    }

    Box(
        modifier
            .fillMaxWidth()
            .aspectRatio(bitmap.width.toFloat() / bitmap.height.coerceAtLeast(1))
            .clipToBounds()
            .background(Color.Black)
            .onSizeChanged { box = it }
            .then(
                if (zoomable) {
                    // One-finger drags pan only when zoomed in; otherwise the page keeps scrolling.
                    Modifier.transformable(transform, canPan = { scale > 1f })
                } else {
                    Modifier
                },
            ),
    ) {
        Box(
            Modifier
                .fillMaxSize()
                .graphicsLayer {
                    scaleX = scale
                    scaleY = scale
                    translationX = offset.x
                    translationY = offset.y
                }
                .then(
                    if (onTap != null || zoomable) {
                        // Inside the transformed layer positions are already in unzoomed space.
                        Modifier.pointerInput(bitmap, onTap) {
                            detectTapGestures(
                                onDoubleTap = {
                                    if (zoomable) {
                                        scale = if (scale > 1f) 1f else 2.5f
                                        offset = Offset.Zero
                                    }
                                },
                                onTap = { at ->
                                    onTap?.invoke(
                                        (at.x * bitmap.width / size.width).toInt().coerceIn(0, bitmap.width - 1),
                                        (at.y * bitmap.height / size.height).toInt().coerceIn(0, bitmap.height - 1),
                                    )
                                },
                            )
                        }
                    } else {
                        Modifier
                    },
                ),
        ) {
            Image(
                bitmap = image,
                contentDescription = "Target screen",
                contentScale = ContentScale.FillBounds,
                modifier = Modifier.fillMaxSize(),
            )
            Canvas(Modifier.fillMaxSize()) {
                val sx = size.width / bitmap.width
                val sy = size.height / bitmap.height
                markers.forEach { m ->
                    drawMarker(Offset(m.x * sx, m.y * sy), m.label, if (m.pending) amber else blue, measurer)
                }
                crosshair?.let { (x, y) -> drawCrosshair(Offset(x * sx, y * sy), amber) }
            }
        }
    }
}

private fun DrawScope.drawMarker(at: Offset, label: String, color: Color, measurer: TextMeasurer) {
    val r = 9.dp.toPx()
    drawCircle(Color.Black.copy(alpha = 0.55f), r + 1.dp.toPx(), at)
    drawCircle(color, r, at, style = Stroke(2.dp.toPx()))
    drawCircle(color, 2.dp.toPx(), at)
    val text = measurer.measure(label, LapisType.MonoLabel.copy(color = Color.White))
    val corner = Offset(
        (at.x + r).coerceAtMost(size.width - text.size.width).coerceAtLeast(0f),
        (at.y - r - text.size.height).coerceAtLeast(0f),
    )
    drawRect(Color.Black.copy(alpha = 0.7f), corner, androidx.compose.ui.geometry.Size(text.size.width.toFloat(), text.size.height.toFloat()))
    drawText(text, topLeft = corner)
}

private fun DrawScope.drawCrosshair(at: Offset, color: Color) {
    val stroke = 2.dp.toPx()
    val gap = 6.dp.toPx()
    val halo = Color.Black.copy(alpha = 0.6f)
    fun line(from: Offset, to: Offset) {
        drawLine(halo, from, to, stroke * 2.2f)
        drawLine(color, from, to, stroke)
    }
    line(Offset(0f, at.y), Offset((at.x - gap).coerceAtLeast(0f), at.y))
    line(Offset((at.x + gap).coerceAtMost(size.width), at.y), Offset(size.width, at.y))
    line(Offset(at.x, 0f), Offset(at.x, (at.y - gap).coerceAtLeast(0f)))
    line(Offset(at.x, (at.y + gap).coerceAtMost(size.height)), Offset(at.x, size.height))
    drawCircle(halo, gap + stroke, at, style = Stroke(stroke * 2.2f))
    drawCircle(color, gap + stroke, at, style = Stroke(stroke))
}

/** Full-screen, zoomable view of one frame. */
@Composable
fun ScreenshotDialog(
    store: ScreenshotStore,
    id: Long,
    onDismiss: () -> Unit,
    crosshair: Pair<Int, Int>? = null,
) {
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            when (val shot = rememberShot(store, id)) {
                is ShotUi.Ready -> ScreenshotCanvas(
                    shot = shot.shot,
                    // Fall back to the click the daemon recorded for this frame.
                    crosshair = crosshair ?: shot.shot.clickX?.let { x -> shot.shot.clickY?.let { y -> x to y } },
                    zoomable = true,
                    modifier = Modifier.align(Alignment.Center),
                )

                is ShotUi.Failed -> Text(
                    shot.message,
                    color = Color.White,
                    modifier = Modifier.align(Alignment.Center).padding(24.dp),
                )

                else -> CircularProgressIndicator(Modifier.align(Alignment.Center))
            }
            TextButton(onClick = onDismiss, modifier = Modifier.align(Alignment.TopEnd).padding(8.dp)) {
                Text("CLOSE", style = LapisType.MonoLabel, color = Color.White)
            }
            Text(
                "frame #$id  ·  pinch to zoom, double-tap to reset",
                style = LapisType.MonoSmall,
                color = Color.White.copy(alpha = 0.7f),
                modifier = Modifier.align(Alignment.BottomCenter).padding(12.dp),
            )
        }
    }
}

/**
 * The 120x68 1-bit approval preview, scaled with nearest-neighbour so every source pixel stays
 * a crisp square (exactly what the CardputerZero LCD shows).
 */
@Composable
fun Preview1BitImage(base64: String?, modifier: Modifier = Modifier) {
    val bits = remember(base64) { Preview1Bit.decodeBase64(base64) } ?: return
    val bitmap = remember(bits) {
        val pixels = Preview1Bit.toArgb(bits, LapisPalette.Paper.toArgb(), LapisPalette.Ink.toArgb())
        Bitmap.createBitmap(pixels, Preview1Bit.WIDTH, Preview1Bit.HEIGHT, Bitmap.Config.ARGB_8888).asImageBitmap()
    }
    Image(
        bitmap = bitmap,
        contentDescription = "1-bit preview of the click target",
        filterQuality = FilterQuality.None,
        contentScale = ContentScale.FillBounds,
        modifier = modifier
            .aspectRatio(Preview1Bit.WIDTH.toFloat() / Preview1Bit.HEIGHT)
            .border(1.dp, MaterialTheme.colorScheme.outline),
    )
}
