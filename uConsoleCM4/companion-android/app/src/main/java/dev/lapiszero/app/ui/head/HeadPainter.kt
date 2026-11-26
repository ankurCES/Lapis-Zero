package dev.lapiszero.app.ui.head

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Canvas
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.LinearGradientShader
import androidx.compose.ui.graphics.Paint
import androidx.compose.ui.graphics.PaintingStyle
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.RadialGradientShader
import androidx.compose.ui.graphics.Shader
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import dev.lapiszero.app.domain.HeadState
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.sign
import kotlin.math.sin
import kotlin.math.sqrt

/** The colours the head is drawn in; all of them come from the theme (see `LapisHead`). */
internal data class HeadColors(
    /** The head's own light and dark: paper shell, ink glass, the same in both themes. */
    val hi: Color,
    val lo: Color,
    /** The room: contour, shadow and the monochrome props; key caps are cut in `bg`. */
    val fg: Color,
    val bg: Color,
    val blue: Color,
    val amber: Color,
)

/**
 * Draws the head of the head spec (the device's `ui/src/gfx/head.rs`) with the phone's means:
 * the same ellipsoid, visor, eye screen and ear pods, turned by the same matrix and lit by the
 * same lamp, but as outlines and gradients where the LCD has a per-pixel ray cast and a dither.
 *
 * Everything is drawn in head radii around the head's rest centre (the canvas is scaled), so
 * the paths and shaders made here once serve every size. A frame allocates nothing: the paths
 * are rewound and refilled, the paints reused.
 */
internal class HeadPainter(private val c: HeadColors) {
    private val fill = Paint()
    private val stroke = Paint().apply {
        style = PaintingStyle.Stroke
        strokeCap = StrokeCap.Round
        strokeJoin = StrokeJoin.Round
    }

    private val shell = Path()
    private val bezel = Path()
    private val glass = Path()
    private val pod = Path()
    private val scratch = Path()

    /** `?` for the approval bubble, around its own centre. A bitmap glyph on the device. */
    private val question = Path().apply {
        moveTo(-0.075f, -0.105f)
        cubicTo(-0.075f, -0.205f, 0.085f, -0.205f, 0.085f, -0.105f)
        cubicTo(0.085f, -0.035f, 0f, -0.045f, 0f, 0.04f)
    }

    private val shellShader = sphereShader(SHELL_LIGHT, SHELL_REACH) { shellTone(it) }
    private val podShader = sphereShader(0.6f, 1.6f) { max(0.30f + 0.75f * it, 0.26f) }
    private val glassShader: Shader = LinearGradientShader(
        Offset(-0.85f, -0.5f), Offset(0.45f, 0.45f), listOf(mix(c.lo, c.hi, GLASS_SHEEN), c.lo),
    )
    private val shadowShader: Shader = RadialGradientShader(
        Offset.Zero, 1f,
        // (1 - r^2), as the device's dither coverage falls off.
        listOf(1f, 0.9375f, 0.75f, 0.4375f, 0f).map { c.fg.copy(alpha = it) },
        listOf(0f, 0.25f, 0.5f, 0.75f, 1f),
    )
    private val blueBall = ballShader(c.blue)

    // The visor's rim, sampled once: a superellipse of exponent 4 in the shell's unit coordinates.
    private val sx = FloatArray(RIM) { squareRoot(cos(2 * PI * it / RIM)) }
    private val sy = FloatArray(RIM) { squareRoot(sin(2 * PI * it / RIM)) }
    private val ax = FloatArray(RIM)
    private val ay = FloatArray(RIM)
    private val av = FloatArray(RIM)

    // Head-to-view rotation, and its rows divided by the shell's axes (the device's ex, ey, d).
    private var m00 = 1f; private var m01 = 0f; private var m02 = 0f
    private var m10 = 0f; private var m11 = 1f; private var m12 = 0f
    private var m20 = 0f; private var m21 = 0f; private var m22 = 1f
    private var exx = 0f; private var exy = 0f; private var exz = 0f
    private var eyx = 0f; private var eyy = 0f; private var eyz = 0f
    private var dx = 0f; private var dy = 0f; private var dz = 0f
    // The silhouette as a quadratic form: u Q u = 1.
    private var q11 = 1f; private var q12 = 0f; private var q22 = 1f

    // The ear pods in view space, left then right: centre, the shell's normal there, lamp axis.
    private val podX = FloatArray(2)
    private val podY = FloatArray(2)
    private val podNx = FloatArray(2)
    private val podNy = FloatArray(2)
    private val podNz = FloatArray(2)
    private val lampX = FloatArray(2)
    private val lampY = FloatArray(2)
    private val lampZ = FloatArray(2)

    /** One real pixel, in head radii: nothing is drawn thinner. */
    private var px = 0f

    // Out parameters of [eyePoint], and the eye being drawn.
    private var ox = 0f
    private var oy = 0f
    private var eyeX = 0f
    private var eyeY = 0f
    private var eyeHalf = 0f

    /** `n` device pixels (at the device's head radius of 38) as head radii, never under a pixel. */
    private fun d(n: Float): Float = max(n / 38f, px)

    /**
     * Draws the head into `width` x `height` pixels: the zone the device gives it, 1.9 radii to
     * either side of the rest centre, 1.3 up and 1.4 down, which is the room the props need.
     */
    fun draw(canvas: Canvas, width: Float, height: Float, m: HeadMotion) {
        val r = height / ZONE_HEIGHT
        if (r <= 0f) return
        px = 1f / r
        val pose = m.pose
        rotate(pose)
        canvas.save()
        canvas.translate(width / 2f, r * ZONE_UP)
        canvas.scale(r, r)
        // The keyboard takes the shadow's place on the ground.
        val keyboard = when {
            m.state == HeadState.WORKING -> m.strength
            m.prev == HeadState.WORKING -> m.leaving
            else -> 0f
        }
        shadow(canvas, pose.lift, keyboard)
        // The outgoing prop first in each pass; it is frozen at the end of its beat.
        prop(canvas, m.prev, BEAT_SECS, m.leaving, front = false, m)
        prop(canvas, m.state, m.tau, m.strength, front = false, m)
        head(canvas, m)
        prop(canvas, m.prev, BEAT_SECS, m.leaving, front = true, m)
        prop(canvas, m.state, m.tau, m.strength, front = true, m)
        canvas.restore()
    }

    // ------------------------------------------------------------------ paints

    private fun solid(color: Color, alpha: Float = 1f): Paint {
        fill.shader = null
        fill.color = color
        fill.alpha = color.alpha * alpha
        return fill
    }

    private fun shaded(shader: Shader, alpha: Float = 1f): Paint {
        fill.color = Color.Black
        fill.shader = shader
        fill.alpha = alpha
        return fill
    }

    private fun line(color: Color, width: Float, alpha: Float = 1f): Paint {
        stroke.color = color
        stroke.alpha = color.alpha * alpha
        stroke.strokeWidth = width
        return stroke
    }

    /** A tone 0..1 between the head's dark and light, as a dither of that coverage reads. */
    private fun shade(tone: Float): Color = mix(c.lo, c.hi, tone.coerceIn(0f, 1f).pow(TONE_GAMMA))

    /**
     * The shading of a ball under the room's lamp, as a radial gradient in the ball's own unit
     * space: its centre sits `light` of the way toward the lamp and it reaches `reach` across.
     * `tone` maps the cosine to the lamp to a tone.
     */
    // ponytail: the bands of equal light on a ball are ellipses and a gradient's rings are
    // circles; with these two numbers the tone is right along the lit diagonal and up to about
    // an eighth off where the shade meets the rim. The shell is also shaded as a ball, though
    // it is an ellipsoid (axes 1 : 0.9 : 0.94). Exact needs a per-pixel shader (AGSL, API 33).
    private fun sphereShader(light: Float, reach: Float, tone: (Float) -> Float): Shader {
        val steps = 24
        val colors = ArrayList<Color>(steps + 1)
        val stops = ArrayList<Float>(steps + 1)
        for (i in 0..steps) {
            // Walking the diameter that runs away from the lamp.
            val s = (reach * i / steps - light).coerceAtMost(1f)
            colors += shade(tone(LIGHT_Z * sqrt(1f - s * s) - LIGHT_XY * s))
            stops += i / steps.toFloat()
        }
        return RadialGradientShader(Offset(light * LIGHT_X / LIGHT_XY, light * LIGHT_Y / LIGHT_XY), reach, colors, stops)
    }

    /** The hands and the moon: a ball of the accent, lit by the same lamp. */
    private fun ballShader(accent: Color): Shader = RadialGradientShader(
        Offset(-0.36f, -0.34f), 1.42f,
        listOf(mix(accent, c.hi, 0.85f), mix(accent, c.hi, 0.3f), accent, mix(accent, c.lo, 0.3f), mix(accent, c.lo, 0.5f)),
        listOf(0f, 0.2f, 0.48f, 0.8f, 1f),
    )

    // ------------------------------------------------------------------ geometry

    /** `M = Rz(roll) * Rx(pitch) * Ry(yaw)`, then the silhouette of the turned shell. */
    private fun rotate(p: HeadPose) {
        val cosYaw = cos(p.yaw); val sinYaw = sin(p.yaw)
        val cp = cos(p.pitch); val sp = sin(p.pitch)
        val cr = cos(p.roll); val sr = sin(p.roll)
        // Rx * Ry
        val a00 = cosYaw; val a02 = sinYaw
        val a10 = -sp * sinYaw; val a11 = cp; val a12 = sp * cosYaw
        m00 = cr * a00 - sr * a10; m01 = -sr * a11; m02 = cr * a02 - sr * a12
        m10 = sr * a00 + cr * a10; m11 = cr * a11; m12 = sr * a02 + cr * a12
        m20 = -cp * sinYaw; m21 = -sp; m22 = cp * cosYaw
        exx = m00 / AX; exy = m01 / AY; exz = m02 / AZ
        eyx = m10 / AX; eyy = m11 / AY; eyz = m12 / AZ
        dx = m20 / AX; dy = m21 / AY; dz = m22 / AZ
        // A view ray at (u, v) just touches the shell where the device's discriminant is zero.
        val a = dx * dx + dy * dy + dz * dz
        val exd = exx * dx + exy * dy + exz * dz
        val eyd = eyx * dx + eyy * dy + eyz * dz
        q11 = exx * exx + exy * exy + exz * exz - exd * exd / a
        q12 = exx * eyx + exy * eyy + exz * eyz - exd * eyd / a
        q22 = eyx * eyx + eyy * eyy + eyz * eyz - eyd * eyd / a
        // Its principal axes, as a path of four arcs.
        val theta = 0.5f * atan2(2f * q12, q11 - q22)
        val ct = cos(theta); val st = sin(theta)
        val s1 = 1f / sqrt(q11 * ct * ct + 2f * q12 * ct * st + q22 * st * st)
        val s2 = 1f / sqrt(q11 * st * st - 2f * q12 * ct * st + q22 * ct * ct)
        oval(shell, s1 * ct, s1 * st, -s2 * st, s2 * ct, -s1 * ct, -s1 * st)
        for (i in 0..1) {
            val side = if (i == 0) -1f else 1f
            val cx = side * POD_X
            podX[i] = m00 * cx + m01 * POD_Y + m02 * POD_Z
            podY[i] = m10 * cx + m11 * POD_Y + m12 * POD_Z
            // The shell's outward normal where the pod sits: which way the pod's cut faces.
            val gx = cx / (AX * AX); val gy = POD_Y / (AY * AY); val gz = POD_Z / (AZ * AZ)
            val nx = m00 * gx + m01 * gy + m02 * gz
            val ny = m10 * gx + m11 * gy + m12 * gz
            val nz = m20 * gx + m21 * gy + m22 * gz
            val n = sqrt(nx * nx + ny * ny + nz * nz)
            podNx[i] = nx / n; podNy[i] = ny / n; podNz[i] = nz / n
            lampX[i] = m00 * side * LAMP_X + m02 * LAMP_Z
            lampY[i] = m10 * side * LAMP_X + m12 * LAMP_Z
            lampZ[i] = m20 * side * LAMP_X + m22 * LAMP_Z
        }
    }

    /**
     * A closed curve through `u`, `w`, `v`, `-w` around the origin, each quarter an elliptic arc:
     * the full ellipse when `v` is `-u`, a half disc joined to a half ellipse otherwise.
     */
    private fun oval(path: Path, ux: Float, uy: Float, wx: Float, wy: Float, vx: Float, vy: Float) {
        val k = 0.5523f
        path.rewind()
        path.moveTo(wx, wy)
        path.cubicTo(wx + k * ux, wy + k * uy, ux + k * wx, uy + k * wy, ux, uy)
        path.cubicTo(ux - k * wx, uy - k * wy, -wx + k * ux, -wy + k * uy, -wx, -wy)
        path.cubicTo(-wx + k * vx, -wy + k * vy, vx - k * wx, vy - k * wy, vx, vy)
        path.cubicTo(vx + k * wx, vy + k * wy, wx + k * vx, wy + k * vy, wx, wy)
        path.close()
    }

    /**
     * The visor's rim scaled by `k` (1 is the edge of the glass), projected into [ax], [ay].
     * A point the turn has carried over the horizon is held on the silhouette, so the visor
     * wraps out of sight instead of folding back. With `light`, [av] gets each point's cosine
     * to the lamp, which is what the glint follows.
     */
    private fun rim(k: Float, light: Boolean) {
        for (i in 0 until RIM) {
            val x = VISOR_X * k * sx[i]
            val y = VISOR_Y0 - VISOR_BOW * x * x + VISOR_Y * k * sy[i]
            val z = sqrt(max(1f - x * x - y * y, 0f))
            var u = m00 * x * AX + m01 * y * AY + m02 * z * AZ
            var v = m10 * x * AX + m11 * y * AY + m12 * z * AZ
            val nz = dx * x + dy * y + dz * z
            if (nz < 0f) {
                val q = sqrt(q11 * u * u + 2f * q12 * u * v + q22 * v * v)
                if (q > 1e-4f) { u /= q; v /= q }
            }
            ax[i] = u
            ay[i] = v
            if (light) {
                val nx = exx * x + exy * y + exz * z
                val ny = eyx * x + eyy * y + eyz * z
                av[i] = (nx * LIGHT_X + ny * LIGHT_Y + nz * LIGHT_Z) / sqrt(nx * nx + ny * ny + nz * nz)
            }
        }
    }

    /** A smooth closed curve through the first `n` points of [ax], [ay] (taking every `step`th). */
    private fun loop(path: Path, n: Int, step: Int = 1) {
        path.rewind()
        val last = (n - 1) * step
        path.moveTo((ax[last] + ax[0]) / 2f, (ay[last] + ay[0]) / 2f)
        for (i in 0 until n) {
            val a = i * step
            val b = if (i == n - 1) 0 else a + step
            path.quadraticTo(ax[a], ay[a], (ax[a] + ax[b]) / 2f, (ay[a] + ay[b]) / 2f)
        }
        path.close()
    }

    /**
     * Adds to [scratch] the stretches of the rim, between points `first` and `last`, where the
     * cosine to the lamp is in `lo..hi`. The ends are interpolated, so the glint slides along
     * the rim as the head turns instead of jumping from sample to sample.
     */
    private fun lit(lo: Float, hi: Float, first: Int, last: Int) {
        var down = false
        for (i in first until last) {
            val a = i % RIM
            val b = (i + 1) % RIM
            val v0 = av[a]
            val v1 = av[b]
            if ((v0 < lo && v1 < lo) || (v0 >= hi && v1 >= hi)) {
                down = false
                continue
            }
            val dv = v1 - v0
            val t0 = if (v0 < lo) (lo - v0) / dv else if (v0 >= hi) (hi - v0) / dv else 0f
            val t1 = if (v1 < lo) (lo - v0) / dv else if (v1 >= hi) (hi - v0) / dv else 1f
            if (t1 <= t0) {
                down = false
                continue
            }
            if (!down || t0 > 0f) scratch.moveTo(ax[a] + (ax[b] - ax[a]) * t0, ay[a] + (ay[b] - ay[a]) * t0)
            scratch.lineTo(ax[a] + (ax[b] - ax[a]) * t1, ay[a] + (ay[b] - ay[a]) * t1)
            down = t1 >= 1f
        }
    }

    /**
     * A point of the eye being drawn, `x`, `y` eye half-widths from its centre, onto the screen
     * (into [ox], [oy]). The eyes live on a second ellipsoid behind the glass, so they shift
     * against the visor's rim when the head turns.
     */
    private fun eyePoint(x: Float, y: Float) {
        val ex = eyeX + x * eyeHalf
        val ey = eyeY + y * eyeHalf
        val ez = sqrt(max(1f - ex * ex - ey * ey, 0f))
        val hx = SCREEN * AX * ex; val hy = SCREEN * AY * ey; val hz = SCREEN * AZ * ez
        ox = m00 * hx + m01 * hy + m02 * hz
        oy = m10 * hx + m11 * hy + m12 * hz
    }

    // ------------------------------------------------------------------ the head

    private fun head(canvas: Canvas, m: HeadMotion) {
        val pose = m.pose
        val accent = if (m.accent == Accent.AMBER) c.amber else c.blue
        val glow = if (m.accent == Accent.NONE) 0f else pose.glow
        canvas.save()
        canvas.translate(0f, -pose.lift)
        // A pod on the far side of its cut is behind the shell; one turned toward the viewer
        // is in front of it.
        for (i in 0..1) if (podNz[i] <= 0f) pod(canvas, i, accent, glow)
        canvas.drawPath(shell, shaded(shellShader))
        visor(canvas, m, accent, glow)
        canvas.drawPath(shell, line(c.fg, d(CONTOUR)))
        for (i in 0..1) if (podNz[i] > 0f) pod(canvas, i, accent, glow)
        canvas.restore()
    }

    private fun visor(canvas: Canvas, m: HeadMotion, accent: Color, glow: Float) {
        canvas.save()
        canvas.clipPath(shell)
        // The gasket: light like the shell, so a groove keeps it apart from the lit side.
        rim(BEZEL, light = false)
        loop(bezel, RIM)
        canvas.drawPath(bezel, solid(c.hi))
        canvas.drawPath(bezel, line(c.lo, d(1f), GROOVE))
        rim(1f, light = false)
        loop(glass, RIM)
        canvas.drawPath(glass, shaded(glassShader))
        canvas.clipPath(glass)
        // The glint, just inside the rim: a stroke where the glass faces the lamp and, after a
        // gap, a short second streak along the top edge.
        rim(GLINT, light = true)
        scratch.rewind()
        lit(0.80f, Float.MAX_VALUE, 0, RIM)
        lit(0.66f, 0.74f, RIM * 5 / 8, RIM * 7 / 8)
        canvas.drawPath(scratch, line(c.hi, d(2f)))
        val pose = m.pose
        eyeHalf = EYE_HALF * pose.wide
        eyeY = EYE_Y + pose.lookY
        for (i in 0..1) {
            eyeX = (2 * i - 1) * EYE_X + pose.lookX
            eye(canvas, m.eyes, left = i == 0, m.cursor, pose.open, accent, glow)
        }
        canvas.restore()
    }

    private fun eye(canvas: Canvas, eyes: Eyes, left: Boolean, cursor: Boolean, open: Float, accent: Color, glow: Float) {
        // Every shape but the open bars is flattened onto the eye's centre line as it shuts.
        val squash = open.coerceIn(0.12f, 1f)
        when (eyes) {
            Eyes.OPEN -> bar(canvas, open, accent, glow)
            // The bottom of a circle: a sagging arc.
            Eyes.ASLEEP -> {
                scratch.rewind()
                for (i in 0..ARC) {
                    val a = 1.12f * (2f * i / ARC - 1f)
                    eyePoint(sin(a), (-0.62f + cos(a)) * squash)
                    if (i == 0) scratch.moveTo(ox, oy) else scratch.lineTo(ox, oy)
                }
                // No halo of its own: asleep, the eyes do not glow through the glass.
                trace(canvas, 0.20f, mono = false, accent, glow)
            }
            // The top of a circle: an upturned arc.
            Eyes.HAPPY -> {
                scratch.rewind()
                for (i in 0..ARC) {
                    val a = 1.16f * (2f * i / ARC - 1f)
                    eyePoint(1.25f * sin(a), (0.8f - 1.25f * cos(a)) * squash)
                    if (i == 0) scratch.moveTo(ox, oy) else scratch.lineTo(ox, oy)
                }
                trace(canvas, 0.40f, mono = true, accent, glow)
            }

            Eyes.CROSS -> {
                scratch.rewind()
                eyePoint(-0.9f, -0.9f * squash); scratch.moveTo(ox, oy)
                eyePoint(0.9f, 0.9f * squash); scratch.lineTo(ox, oy)
                eyePoint(-0.9f, 0.9f * squash); scratch.moveTo(ox, oy)
                eyePoint(0.9f, -0.9f * squash); scratch.lineTo(ox, oy)
                trace(canvas, 0.255f, mono = true, accent, glow)
            }
            // A terminal: `>` in the left eye, the cursor in the right one.
            Eyes.PROMPT -> if (left) {
                scratch.rewind()
                eyePoint(-0.35f, -1.1f * squash); scratch.moveTo(ox, oy)
                eyePoint(0.75f, 0f); scratch.lineTo(ox, oy)
                eyePoint(-0.35f, 1.1f * squash); scratch.lineTo(ox, oy)
                trace(canvas, 0.283f, mono = true, accent, glow)
            } else {
                // A block while the cursor is on, an underline while it is off: never one-eyed.
                val top = if (cursor) 0.55f else 1.02f
                scratch.rewind()
                eyePoint(-0.95f, top * squash); scratch.moveTo(ox, oy)
                eyePoint(0.95f, top * squash); scratch.lineTo(ox, oy)
                eyePoint(0.95f, 1.25f * squash); scratch.lineTo(ox, oy)
                eyePoint(-0.95f, 1.25f * squash); scratch.lineTo(ox, oy)
                scratch.close()
                canvas.drawPath(scratch, solid(c.hi))
            }
        }
        // Nearly shut, every shape is the line that shut open eyes are: the change of shape
        // happens behind it.
        if (eyes != Eyes.OPEN && open < 0.35f) bar(canvas, open, accent, glow)
    }

    /** The open eye: a rounded bar with a bright core, a rim and a halo that take the accent. */
    private fun bar(canvas: Canvas, open: Float, accent: Color, glow: Float) {
        val h = max(1.613f * open, 0.24f)
        // The halo in two steps, so it falls off: the eyes glow a little through the glass
        // even without an accent.
        ring(HALO, h)
        canvas.drawPath(scratch, solid(c.hi, 0.09f))
        if (glow > 0f) canvas.drawPath(scratch, solid(accent, 0.24f * glow))
        ring(HALO_INNER, h)
        canvas.drawPath(scratch, solid(c.hi, 0.13f))
        if (glow > 0f) canvas.drawPath(scratch, solid(accent, 0.34f * glow))
        ring(1f, h)
        canvas.drawPath(scratch, solid(mix(c.hi, accent, glow)))
        ring(CORE, h)
        canvas.drawPath(scratch, solid(c.hi))
    }

    /** The bar's outline at `k` of its size into [scratch]: `x^4 + (y / h)^4 = k^4`. */
    private fun ring(k: Float, h: Float) {
        val n = RIM / EYE_STEP
        for (i in 0 until n) {
            eyePoint(k * sx[i * EYE_STEP], k * h * sy[i * EYE_STEP])
            ax[i] = ox
            ay[i] = oy
        }
        loop(scratch, n)
    }

    /**
     * Strokes the centre line in [scratch] as an eye shape whose rim is `half` eye half-widths
     * to either side: halo, rim, core, as the bar has them.
     */
    // ponytail: one stroke width for the whole line, so an eye turned away keeps its thickness
    // where the device's thins with the foreshortening (at most a seventh, at Fetching's 30
    // degrees). The line itself is foreshortened as it should be.
    private fun trace(canvas: Canvas, half: Float, mono: Boolean, accent: Color, glow: Float) {
        val w = 2f * half * eyeHalf * SCREEN
        if (mono) canvas.drawPath(scratch, line(c.hi, w * 1.612f, 0.22f))
        if (glow > 0f) canvas.drawPath(scratch, line(accent, w * 1.612f, 0.5f * glow))
        canvas.drawPath(scratch, line(mix(c.hi, accent, glow), w))
        canvas.drawPath(scratch, line(c.hi, w * 0.648f))
    }

    /**
     * An ear pod: a rubber ball sunk half into the shell, with a lamp on the cap that faces
     * outward and forward. What shows of it is the half outside the shell: a half disc, plus
     * the cut where it meets the shell once that cut has turned toward the viewer.
     */
    // ponytail: the cut is taken as flat (the shell's tangent plane at the pod), and a lamp
    // that reaches round the pod's edge is clipped there instead of wrapping. Both are right to
    // a fraction of a pixel at these sizes; the device finds the true edges ray by ray.
    private fun pod(canvas: Canvas, i: Int, accent: Color, glow: Float) {
        canvas.save()
        canvas.translate(podX[i], podY[i])
        canvas.scale(POD_R, POD_R)
        val nz = podNz[i]
        val flat = sqrt(podNx[i] * podNx[i] + podNy[i] * podNy[i])
        if (nz <= 0f || flat < 1e-3f) {
            oval(pod, 1f, 0f, 0f, 1f, -1f, 0f)
        } else {
            val ux = podNx[i] / flat
            val uy = podNy[i] / flat
            oval(pod, ux, uy, -uy, ux, -nz * ux, -nz * uy)
        }
        canvas.drawPath(pod, shaded(podShader))
        val lx = lampX[i]; val ly = lampY[i]; val lz = lampZ[i]
        if (lz > 0.02f) {
            canvas.save()
            canvas.clipPath(pod)
            // The lamp's cap and its seat are circles on the ball: ellipses from here.
            val reach = sqrt(lx * lx + ly * ly)
            canvas.rotate(atan2(ly, lx) * DEGREES)
            canvas.drawOval(SEAT_COS * reach - SEAT_SIN * lz, -SEAT_SIN, SEAT_COS * reach + SEAT_SIN * lz, SEAT_SIN, solid(c.lo))
            // Dark glass until the accent lights it; the lamp facing the room's light is paler.
            val cosine = lx * LIGHT_X + ly * LIGHT_Y + lz * LIGHT_Z
            val lens = mix(shade(0.25f + 0.75f * max(cosine, 0f)), accent, glow)
            canvas.drawOval(LENS_COS * reach - LENS_SIN * lz, -LENS_SIN, LENS_COS * reach + LENS_SIN * lz, LENS_SIN, solid(lens))
            canvas.restore()
            // One highlight, a little way round from the lamp's axis toward the light.
            val hx = lx + 0.5f * (LIGHT_X - cosine * lx)
            val hy = ly + 0.5f * (LIGHT_Y - cosine * ly)
            val hz = lz + 0.5f * (LIGHT_Z - cosine * lz)
            if (hz > 0f) {
                val h = sqrt(hx * hx + hy * hy + hz * hz)
                canvas.drawCircle(Offset(hx / h, hy / h), d(1.1f) / POD_R, solid(c.hi))
            }
        }
        canvas.drawPath(pod, line(c.fg, d(CONTOUR) / POD_R))
        canvas.restore()
    }

    // ------------------------------------------------------------------ the stage

    /** On the ground under the rest centre; it narrows and thins as the head rises. */
    private fun shadow(canvas: Canvas, lift: Float, keyboard: Float) {
        val rise = max(lift, -0.1f)
        val cover = (0.55f - 0.9f * rise) * (1f - keyboard)
        val half = 0.80f - 0.9f * rise
        if (cover <= 0.02f || half <= 0f) return
        canvas.save()
        canvas.translate(0.05f, 1.20f)
        canvas.scale(half, 0.15f)
        canvas.drawCircle(Offset.Zero, 1f, shaded(shadowShader, min(cover * SHADOW_INK, 1f)))
        canvas.restore()
    }

    private fun ball(canvas: Canvas, x: Float, y: Float, r: Float) {
        if (r < 0.5f * px) return
        canvas.save()
        canvas.translate(x, y)
        canvas.scale(r, r)
        canvas.drawCircle(Offset.Zero, 1f, shaded(blueBall))
        // A contour, on balls big enough to carry one.
        val rim = d(1.3f) / r
        if (r >= d(3f)) canvas.drawCircle(Offset.Zero, 1f - rim / 2f, line(c.lo, rim, 0.85f))
        canvas.restore()
    }

    private fun block(canvas: Canvas, left: Float, top: Float, right: Float, bottom: Float, color: Color, alpha: Float = 1f) {
        if (right <= left || bottom <= top) return
        val round = min(min(right - left, bottom - top) * 0.22f, d(1f))
        canvas.drawRoundRect(left, top, right, bottom, round, round, solid(color, alpha))
    }

    /**
     * `state`'s prop at strength `w` (it grows in and shrinks out with the easing): the part
     * behind the head or the part in front of it. Positions are from the head's rest centre,
     * so the props do not ride the hops; only the sound arcs follow the ear pods.
     */
    private fun prop(canvas: Canvas, state: HeadState, tau: Float, w: Float, front: Boolean, m: HeadMotion) {
        if (w <= 0.02f) return
        val t = m.t
        when (state) {
            // Sound arriving: arcs close in on both ear pods and fade as they reach them.
            // Louder brings more of them, and bolder.
            HeadState.LISTENING -> if (front) {
                val s = phase(t, 0.6)
                val loud = m.rms * 3.2f
                val half = 0.62f * w * DEGREES
                for (i in 0..1) {
                    val cx = podX[i]
                    val cy = podY[i] - m.pose.lift
                    for (k in 0..2) {
                        val pos = k + 1 - s
                        val cover = min(min((loud + 1f - pos) / 0.4f, pos / 0.5f), 1f)
                        if (cover <= 0f) break
                        val radius = 0.34f + 0.155f * pos
                        canvas.drawArc(
                            cx - radius, cy - radius, cx + radius, cy + radius,
                            if (i == 0) 180f - half else -half, 2f * half, false,
                            line(c.fg, d(if (pos < loud) 2f else 1f), cover),
                        )
                    }
                }
            }
            // A moon on a tilted orbit: big across the brow in front, small and high over the
            // crown behind, like a moon setting. Three smaller ones trail it.
            HeadState.THINKING -> {
                var moonX = 0f; var moonY = 0f; var moonR = 0f
                for (k in 0..3) {
                    val a = TAU * phase(t, 2.4) - 0.30f * k
                    val z = sin(a)
                    val x = 1.40f * cos(a)
                    val y = -0.84f + 0.14f * z - 0.12f * cos(a)
                    val size = (0.16f + 0.05f * z) * TRAIL[k] * w
                    if (k == 0) { moonX = x; moonY = y; moonR = size }
                    // Where the orbit doubles back the tail goes behind the moon, not across it.
                    val clear = moonR + size
                    val hidden = k > 0 && (x - moonX) * (x - moonX) + (y - moonY) * (y - moonY) < clear * clear
                    if ((z >= 0f) == front && !hidden) ball(canvas, x, y, size)
                }
            }
            // A keyboard on the ground and two hands over it, striking in turn: the key that
            // was hit lights up, and each strike throws off a pair of ticks.
            HeadState.WORKING -> if (front) {
                val leftStrikes = floor(t / BEAT_STEP).toLong() % 2 == 0L
                val b = phase(t, BEAT_STEP)
                val key = max(KEY_PITCH, 3f * px)
                val gap = d(1f)
                // It grows from the middle, a key at a time; a stub of one or two is not drawn.
                val keys = floor(2f * KEYS_HALF * w / key).toInt().let { if (it < 3) 0 else it }
                val kx = -(keys * key + gap) / 2f
                if (keys > 0) {
                    canvas.drawRoundRect(kx, KEYS_Y, kx + keys * key + gap, KEYS_Y + 2f * key + gap, gap, gap, solid(c.fg))
                    // Two rows of caps, the lower one set half a key along and cut off at the case.
                    for (i in 0 until keys) {
                        val x = kx + gap + i * key
                        block(canvas, x, KEYS_Y + gap, x + key - gap, KEYS_Y + key, c.bg)
                        val low = x + key / 2f
                        block(canvas, low, KEYS_Y + gap + key, low + min(key - gap, kx + keys * key - low), KEYS_Y + 2f * key, c.bg)
                    }
                }
                for (h in 0..1) {
                    val striking = (h == 0) == leftStrikes
                    val down = if (striking) (1f - b) * (1f - b) else 0f
                    val x = (2 * h - 1) * 0.46f
                    val y = HAND_Y + HAND_TRAVEL * down
                    val hand = 0.17f * w
                    val i = floor((x - kx) / key).toInt()
                    if (striking && b < 0.4f && i in 0 until keys) {
                        val cap = kx + gap + i * key
                        block(canvas, cap, KEYS_Y + gap, cap + key - gap, KEYS_Y + key, c.blue)
                    }
                    ball(canvas, x, y, hand)
                    if (striking && b < 0.2f && w > 0.5f) {
                        for (o in 0..1) {
                            val out = 2 * o - 1
                            val near = hand + d(2f)
                            val far = near + d(2f) * w
                            canvas.drawLine(
                                Offset(x + near * 0.82f * out, y - near * 0.57f),
                                Offset(x + far * 0.82f * out, y - far * 0.57f),
                                line(c.blue, d(2f)),
                            )
                        }
                    }
                }
            }
            // Packets fly in from the right, growing; each shrinks away just short of the
            // head, which swallows it (see the gulp).
            HeadState.FETCHING -> if (front) {
                for (k in 0..2) {
                    val p = phase(t + k * PACKET_SECS, 3 * PACKET_SECS)
                    val e = p * p
                    val x = 1.80f - 0.56f * e
                    val y = -0.34f + 0.28f * e
                    val half = min(0.045f + 0.085f * e, 0.10f) * min((1f - p) / 0.15f, 1f) * w
                    if (half > 0.25f * px) block(canvas, x - half, y - half, x + half, y + half, c.blue)
                }
            }
            // Output scrolling up beside the head; the newest line is being typed, in blue.
            HeadState.COMMAND -> if (front) {
                val n = floor(t / LINE_SECS).toInt()
                val f = phase(t, LINE_SECS)
                val thick = d(2f)
                for (row in 0..4) {
                    val y = -0.52f + 0.25f * (row - f)
                    val len = (0.16f + 0.28f * hash01(n + row)) * w
                    when (row) {
                        0 -> block(canvas, 1.44f, y, 1.44f + len, y + thick, c.fg, 1f - f) // dissolves as it leaves
                        4 -> block(canvas, 1.44f, y, 1.44f + len * f, y + thick, c.blue)
                        else -> block(canvas, 1.44f, y, 1.44f + len, y + thick, c.fg)
                    }
                }
            }
            // An amber speech bubble with a question mark, bobbing; its tail points at the head.
            HeadState.APPROVAL -> if (front) {
                val x = 1.42f
                val y = -0.80f + 0.04f * wave(t, 0.8)
                val hw = 0.36f * w
                val hh = 0.30f * w
                val round = d(3f) * w
                canvas.drawRoundRect(x - hw, y - hh, x + hw, y + hh, round, round, solid(c.amber))
                scratch.rewind()
                scratch.moveTo(x - hw + d(3f) * w, y + hh - round)
                scratch.lineTo(x - hw + d(3f) * w + hh / 2f + d(1f), y + hh - d(0.5f))
                scratch.lineTo(x - hw + d(3f) * w - hh / 4f, y + hh + hh / 2f)
                scratch.close()
                canvas.drawPath(scratch, solid(c.amber))
                if (w > 0.8f) {
                    // Dark on amber in both themes.
                    canvas.save()
                    canvas.translate(x, y)
                    canvas.drawPath(question, line(c.lo, max(0.062f, 1.5f * px)))
                    canvas.drawCircle(Offset(0f, 0.135f), max(0.04f, px), solid(c.lo))
                    canvas.restore()
                }
            }
            // A pause mark that never leaves: solid, then hollow for a moment, the way a paused
            // tape deck blinks.
            HeadState.PAUSED -> if (front) {
                val bw = 0.10f + d(1f)
                val bh = 0.34f * w
                val hollow = phase(t, 1.6) >= 0.7f
                val thin = d(1f)
                for (k in 0..1) {
                    val x = 1.18f + 2f * bw * k
                    val y = -0.98f - bh / 2f
                    if (hollow) {
                        canvas.drawRect(x + thin / 2f, y + thin / 2f, x + bw - thin / 2f, y + bh - thin / 2f, line(c.fg, thin))
                    } else {
                        block(canvas, x, y, x + bw, y + bh, c.fg)
                    }
                }
            }
            // Six rays bursting outward for 0.7 s. None points straight up, where the hop goes.
            HeadState.DONE -> if (front) {
                val e = min(tau / 0.7f, 1f)
                if (e < 1f) {
                    val out = 1f - (1f - e) * (1f - e)
                    val r1 = 1.24f + 0.30f * out
                    val r2 = r1 + 0.24f * (1f - e) * w
                    for (turn in RAYS) {
                        val a = PI.toFloat() * turn
                        canvas.drawLine(
                            Offset(r1 * cos(a), r1 * sin(a) * 0.9f),
                            Offset(r2 * cos(a), r2 * sin(a) * 0.9f),
                            line(c.blue, d(2f)),
                        )
                    }
                }
            }
            // An amber `!` that shakes with the head.
            HeadState.ERROR -> if (front) {
                val fade = max(1f - tau / 0.9f, 0f)
                val x = 1.22f + 0.06f * fade * sin(TAU * tau / 0.15f)
                val bw = 0.11f + d(1f)
                val bh = 0.36f * w
                block(canvas, x, -0.96f - bh / 2f, x + bw, -0.96f + bh / 2f, c.amber)
                block(canvas, x, -0.96f + bh / 2f + d(2f), x + bw, -0.96f + bh / 2f + d(2f) + bw, c.amber)
            }

            HeadState.IDLE -> Unit
        }
    }

    companion object {
        /** The head's zone in head radii: 1.9 to either side of the rest centre, 1.3 up, 1.4 down. */
        const val ZONE_WIDTH = 3.8f
        const val ZONE_HEIGHT = 2.7f
        private const val ZONE_UP = 1.3f

        // Shell: an ellipsoid. Eye screen: a second one inside it, behind the glass.
        private const val AX = 1.00f
        private const val AY = 0.90f
        private const val AZ = 0.94f
        private const val SCREEN = 0.84f

        // Visor: (x / 0.76)^4 + ((y - 0.03 + 0.14 x^2) / 0.47)^4 < 1 on the shell's unit point.
        // The x^2 term bows it, so the glass wraps round the ball even seen dead-on.
        private const val VISOR_X = 0.76f
        private const val VISOR_Y = 0.47f
        private const val VISOR_Y0 = 0.03f
        private const val VISOR_BOW = 0.14f
        private const val BEZEL = 1.0916f // the gasket ends at f = 1.42: the fourth root
        private const val GLINT = 0.853f // the middle of the band f = 0.42..0.66
        private const val RIM = 96

        // Eyes, on the eye screen's unit point.
        private const val EYE_X = 0.37f
        private const val EYE_Y = 0.05f
        private const val EYE_HALF = 0.155f
        private const val CORE = 0.805f // m < 0.42
        private const val HALO = 1.27f // m < 2.6
        private const val HALO_INNER = 1.13f
        private const val EYE_STEP = 3
        private const val ARC = 14

        // Ear pods: balls on the shell's sides, a lamp on the cap 38.7 degrees round its axis,
        // seated in a ring out to 48.7.
        private const val POD_X = 1.00f
        private const val POD_Y = 0.06f
        private const val POD_Z = -0.04f
        private const val POD_R = 0.26f
        private const val LAMP_X = 0.74f
        private const val LAMP_Z = 0.673f
        private const val LENS_COS = 0.78f
        private const val LENS_SIN = 0.626f
        private const val SEAT_COS = 0.66f
        private const val SEAT_SIN = 0.751f

        // One lamp, fixed to the room: up, left and in front. A unit vector toward it.
        private const val LIGHT_X = -0.55f
        private const val LIGHT_Y = -0.52f
        private const val LIGHT_Z = 0.653f
        private const val LIGHT_XY = 0.7569f

        // The phone's own choices: how the device's dither coverage becomes a smooth tone.
        private const val TONE_GAMMA = 0.62f
        private const val SHELL_LIGHT = 0.55f
        private const val SHELL_REACH = 1.56f
        private const val GLASS_SHEEN = 0.13f
        private const val GROOVE = 0.16f
        private const val SHADOW_INK = 0.6f
        private const val CONTOUR = 1.1f // device pixels; 1.3 there, where it also has to carry the edge of a dither

        private const val HAND_Y = 0.76f
        private const val HAND_TRAVEL = 0.24f
        private const val KEYS_Y = 1.14f
        private const val KEYS_HALF = 0.80f
        private const val KEY_PITCH = 0.105f

        private const val TAU = (2 * PI).toFloat()
        private const val DEGREES = (180 / PI).toFloat()
        private val TRAIL = floatArrayOf(1f, 0.5f, 0.35f, 0.2f)
        private val RAYS = floatArrayOf(0.03f, 0.97f, 1.14f, 1.31f, 1.69f, 1.86f)

        /** White through the lit half, a fifth in shade, a little light bounced onto the far rim. */
        private fun shellTone(cosine: Float): Float =
            0.18f + 0.82f * smooth((cosine + 0.304f) / 0.656f) + 0.5f * max(-cosine - 0.55f, 0f)

        private fun squareRoot(x: Double): Float = (sign(x) * sqrt(abs(x))).toFloat()

        /** Plain sRGB mixing: no colour-space round trip, so it is safe in a frame. */
        internal fun mix(a: Color, b: Color, t: Float): Color = Color(
            red = a.red + (b.red - a.red) * t,
            green = a.green + (b.green - a.green) * t,
            blue = a.blue + (b.blue - a.blue) * t,
            alpha = a.alpha + (b.alpha - a.alpha) * t,
        )
    }
}
