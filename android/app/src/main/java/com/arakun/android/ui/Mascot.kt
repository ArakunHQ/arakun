// Arakun's raccoon, alive: the desktop's mascot (core/src/mascot.rs) cut down
// for the phone, as the iOS app has it. The same drawing
// (assets/brand/mascot.json, copied in at build time) on springs, with eyes
// that blink and look around, and one pose and one symbol per mood, so each
// state reads at a glance. With animations turned off it holds still.
package com.arakun.android.ui

import android.content.Context
import android.provider.Settings
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathFillType
import androidx.compose.ui.graphics.PathMeasure
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.arakun.kit.array
import com.arakun.kit.double
import com.arakun.kit.get
import com.arakun.kit.parseJson
import com.arakun.kit.string
import kotlinx.serialization.json.JsonElement
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sin
import kotlin.random.Random

/** What the agent behind the raccoon is doing. */
enum class Mood(val word: String) {
    IDLE("Idle"), THINKING("Thinking"), WORKING("Working"), WAITING("Waiting for you"),
    DONE("Done"), ERROR("Error"), SLEEPING("Asleep"),
}

/** The drawing's parts, read once from the app's assets. */
private class MascotArt(json: JsonElement, small: Boolean) {
    val ink: Path
    /** The head with the ink parts as holes, for an even-odd fill. */
    val body: Path
    val eyes: List<Rect>

    init {
        fun paths(key: String): List<String> {
            val v = json[key]
            return v.array.mapNotNull { it["d"].string }.ifEmpty { listOfNotNull(v["d"].string) }
        }
        fun parse(d: String): Path = PathParser().parsePathString(d).toPath()
        val suffix = if (small) "_small" else ""
        val head = Path()
        for (d in paths(if (small) "head_small" else "head")) head.addPath(parse(d))
        // The dark of the mask, the ear insides and the forehead stripe.
        val ink = Path()
        for (key in listOf("band", "ear_in", "stripe")) {
            for (d in paths(key + suffix).ifEmpty { paths(key) }) ink.addPath(parse(d))
        }
        ink.addPath(nose)
        this.ink = ink
        body = Path().apply {
            addPath(head)
            addPath(ink)
            fillType = PathFillType.EvenOdd
        }
        eyes = json["eyes"].array.map {
            val cx = it["cx"].double ?: 0.0
            val cy = it["cy"].double ?: 0.0
            val w = it["w"].double ?: 0.0
            val h = it["h"].double ?: 0.0
            Rect((cx - w / 2).toFloat(), (cy - h / 2).toFloat(), (cx + w / 2).toFloat(), (cy + h / 2).toFloat())
        }
    }

    companion object {
        private var full: MascotArt? = null
        private var little: MascotArt? = null

        fun get(context: Context, small: Boolean): MascotArt {
            (if (small) little else full)?.let { return it }
            val json = context.assets.open("mascot.json").use { parseJson(it.readBytes()) } ?: kotlinx.serialization.json.JsonNull
            val art = MascotArt(json, small)
            if (small) little = art else full = art
            return art
        }

        /**
         * The nose, which mascot.json leaves out: the brand SVG's, moved by the
         * same (10.73, 10.68) as the rest.
         */
        private val nose: Path
            get() = Path().apply {
                moveTo(99.89f, 96.42f)
                cubicTo(99.89f, 99.69f, 95.14f, 105.06f, 89.27f, 105.06f)
                cubicTo(83.40f, 105.06f, 78.65f, 99.70f, 78.65f, 96.42f)
                cubicTo(78.65f, 93.14f, 83.40f, 90.49f, 89.27f, 90.49f)
                cubicTo(95.14f, 90.49f, 99.89f, 93.14f, 99.89f, 96.42f)
                close()
                translate(Offset(10.73f, 10.68f))
            }
    }
}

/** A value that follows its target like a spring. */
private class Spring(var x: Float) {
    var v = 0f

    fun step(target: Float, dt: Float, stiffness: Float = 170f, damping: Float = 16f) {
        v += (stiffness * (target - x) - damping * v) * dt
        x += v * dt
    }

    fun reset(to: Float) {
        x = to
        v = 0f
    }
}

private enum class Eyes { OPEN, HAPPY, CROSS }
private enum class Prop { DOTS, ASK, BANG, CHECK, ZZZ }

/** One frame's intent, as on the desktop. */
private class Pose {
    /** x, y, rotation in degrees, scale x, scale y. */
    val head = floatArrayOf(0f, 0f, 0f, 1f, 1f)
    var look = floatArrayOf(0f, 0f)

    /** Eye width and height factors, and tilt in degrees (mirrored per eye). */
    var eye = floatArrayOf(1f, 1f, 0f)
    var eyes = Eyes.OPEN
    var blink = true
    var prop: Prop? = null
}

/** The springs and clocks behind one raccoon, kept across frames. Times in seconds. */
private class MascotState {
    val head = Array(5) { Spring(if (it >= 3) 1f else 0f) }
    val look = Array(2) { Spring(0f) }
    val eye = arrayOf(Spring(1f), Spring(1f), Spring(0f))
    var propAlpha = 0f
    var prop: Prop? = null
    var mood = Mood.IDLE

    /** The mood last asked for, apart from `mood`, which plays `done` in between. */
    var requested = Mood.IDLE
    var moodStart = 0.0
    var last: Double? = null
    var nextBlink = 1.5
    var blinkStart: Double? = null
    var nextGlance = 2.0
    var glance = floatArrayOf(0f, 0f)
    var hopStart: Double? = null

    /** A mood that was busy and turns idle plays `done` first. */
    fun enter(m: Mood, now: Double) {
        if (m == requested) return
        requested = m
        val busy = mood == Mood.WORKING || mood == Mood.THINKING || mood == Mood.WAITING
        mood = if (m == Mood.IDLE && busy) Mood.DONE else m
        moodStart = now
    }
}

/**
 * The box in mascot.json's units, which put the brand SVG's head (178.54
 * wide) at (10.73, 10.68): room for hops, and props float up to about 50
 * units above it.
 */
private val BOX = Rect(5.73f, 2.68f, 5.73f + 188.54f, 2.68f + 152f)
private const val PROP_ROOM = 52f

/** The head's centre line and chin; it squashes and turns about the chin. */
private const val CX = 100f
private const val FLOOR = 147.84f

/** Idle this long and it falls asleep, until a tap or a new mood. */
private const val DOZE_AFTER = 120.0

private fun now(): Double = System.nanoTime() / 1e9

/** Whether the system asks for no animation (Settings › Accessibility › Remove animations). */
private fun animationsOff(context: Context): Boolean =
    Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f

/** The raccoon, `width` wide, in a mood. A tap makes it hop. */
@Composable
fun Mascot(mood: Mood, width: Dp, modifier: Modifier = Modifier, props: Boolean = true) {
    val context = LocalContext.current
    val small = width < 48.dp
    val top = if (props && !small) PROP_ROOM else 0f
    val height = width * ((BOX.height + top) / BOX.width)
    val state = remember { MascotState() }
    val art = remember(small) { MascotArt.get(context, small) }
    val still = remember { animationsOff(context) }
    val c = Palette.current
    // The symbols over the head: in the text color, so they read on white too.
    val propColor = if (c.dark) Brand.fur else Brand.furInk
    val askColor = c.amberInk
    var frame by remember { mutableLongStateOf(0L) }
    LaunchedEffect(still) {
        if (still) return@LaunchedEffect
        while (true) withFrameNanos { frame = it }
    }
    Canvas(
        modifier
            .size(width, height)
            .semantics {
                contentDescription = "Arakun"
                stateDescription = mood.word
            }
            .pointerInput(Unit) {
                detectTapGestures {
                    val t = now()
                    state.hopStart = t
                    if (state.mood == Mood.IDLE) state.moodStart = t
                }
            },
    ) {
        // Read so each frame redraws.
        frame
        draw(state, art, mood, still, small, top, propColor, askColor)
    }
}

private fun pose(s: MascotState, now: Double): Pose {
    val p = Pose()
    val t = now - s.moodStart
    when (s.mood) {
        Mood.IDLE -> if (t > DOZE_AFTER && s.hopStart == null) {
            // Nothing has happened for a while: it dozes off.
            val b = sin(t * 1.3).toFloat()
            p.eye = floatArrayOf(1.2f, 0.13f, 0f)
            p.head[1] = 7 + 1.5f * b
            p.head[2] = -3f
            p.look = floatArrayOf(0f, 0.5f)
            p.blink = false
            p.prop = Prop.ZZZ
        } else {
            if (now > s.nextGlance) {
                s.glance = if (Random.nextBoolean()) {
                    floatArrayOf(Random.nextDouble(-0.8, 0.8).toFloat(), Random.nextDouble(-0.5, 0.4).toFloat())
                } else {
                    floatArrayOf(0f, 0f)
                }
                s.nextGlance = now + Random.nextDouble(1.6, 4.2)
            }
            p.look = s.glance
            p.head[1] = 1.2f * sin(now * 1.6).toFloat()
        }
        Mood.THINKING -> {
            val side = if ((t / 3.2).toInt() % 2 == 1) -1f else 1f
            p.head[2] = 8 * side
            p.head[1] = -1f
            p.eye[1] = 0.92f
            p.look = floatArrayOf(0.75f * side + 0.1f * sin(now * 1.3).toFloat(), -0.95f)
            p.prop = Prop.DOTS
        }
        Mood.WORKING -> {
            // Head down, narrowed eyes reading line after line.
            val line = (t % 1.6) / 1.6
            val lx = (-0.75 + 1.5 * min(1.0, line / 0.85)).toFloat()
            if ((t / 1.6).toInt() % 5 == 4 && line > 0.2) {
                p.look = floatArrayOf(0f, -0.1f)
            } else {
                p.look = floatArrayOf(lx, 0.85f)
                p.eye = floatArrayOf(1f, 0.55f, -9f)
                p.head[1] = 4f
                p.head[2] = lx * 1.5f
            }
        }
        Mood.WAITING -> {
            // Big eyes on you and a hop that keeps coming back.
            val c = t % 2
            p.head[2] = if ((t / 2).toInt() % 2 == 1) -5f else 5f
            when {
                c < 0.12 -> { p.head[1] = 2f; p.head[3] = 1.05f; p.head[4] = 0.92f }
                c < 0.45 -> { p.head[1] = -12f; p.head[3] = 0.97f; p.head[4] = 1.05f }
                c < 0.58 -> { p.head[1] = 1f; p.head[3] = 1.04f; p.head[4] = 0.94f }
            }
            p.eye = floatArrayOf(1.12f, 1.15f, 0f)
            p.prop = Prop.ASK
        }
        Mood.DONE -> {
            if (t > 2.8) {
                s.mood = Mood.IDLE
                s.moodStart = now
                return pose(s, now)
            }
            p.prop = Prop.CHECK
            when {
                t < 0.18 -> { p.head[1] = 3f; p.head[3] = 1.07f; p.head[4] = 0.88f; p.eye = floatArrayOf(1.15f, 1.2f, 0f) }
                t < 0.55 -> { p.head[1] = -16f; p.head[3] = 0.96f; p.head[4] = 1.06f; p.eyes = Eyes.HAPPY }
                t < 0.7 -> { p.head[1] = 1f; p.head[3] = 1.05f; p.head[4] = 0.93f; p.eyes = Eyes.HAPPY }
                t < 2.5 -> { p.eyes = Eyes.HAPPY; p.head[2] = 5 * sin((t - 0.7) * 6).toFloat() }
            }
        }
        Mood.ERROR -> {
            // A shake with crossed eyes, then a sad look down.
            p.prop = Prop.BANG
            if (t < 0.8) {
                p.head[0] = (8 * sin(t * 42) * exp(-3.5 * t)).toFloat()
                p.eyes = Eyes.CROSS
            } else {
                p.eye = floatArrayOf(1f, 0.72f, 17f)
                p.head[1] = 6f
                p.look = floatArrayOf(0f, 0.75f)
            }
        }
        Mood.SLEEPING -> {
            val b = sin(t * 1.3).toFloat()
            p.eye = floatArrayOf(1.2f, 0.13f, 0f)
            p.head[1] = 7 + 1.5f * b
            p.head[2] = -3f
            p.head[4] = 1 + 0.03f * b
            p.look = floatArrayOf(0f, 0.5f)
            p.blink = false
            p.prop = Prop.ZZZ
        }
    }
    // A tap: one hop with happy eyes.
    s.hopStart?.let { h ->
        val u = now - h
        when {
            u > 0.75 -> s.hopStart = null
            u < 0.12 -> { p.head[1] = 3f; p.head[3] = 1.06f; p.head[4] = 0.9f }
            u < 0.45 -> { p.head[1] = -14f; p.head[3] = 0.97f; p.head[4] = 1.05f; p.eyes = Eyes.HAPPY }
            else -> p.eyes = Eyes.HAPPY
        }
    }
    return p
}

/** Advances the springs to `now`; gives the pose and how open the eyes are. */
private fun step(s: MascotState, mood: Mood, still: Boolean, now: Double): Pair<Pose, Float> {
    if (s.last == null) {
        s.moodStart = now
        s.mood = mood
        s.requested = mood
    }
    s.enter(mood, now)
    val dt = min(0.05, now - (s.last ?: (now - 1.0 / 60))).toFloat()
    s.last = now
    if (still) return settle(s, mood) to 1f
    val p = pose(s, now)
    for (i in 0 until 5) {
        s.head[i].step(p.head[i], dt, if (i == 0) 400f else 190f, if (i == 0) 14f else 15f)
    }
    for (i in 0 until 2) s.look[i].step(p.look[i], dt, 260f, 24f)
    for (i in 0 until 3) s.eye[i].step(p.eye[i], dt, 300f, 24f)
    if (s.prop != p.prop) {
        s.propAlpha = max(0f, s.propAlpha - dt * 6)
        if (s.propAlpha == 0f) s.prop = p.prop
    } else if (p.prop != null) {
        s.propAlpha = min(1f, s.propAlpha + dt * 4)
    }
    var blink = 1f
    if (p.blink) {
        if (now > s.nextBlink && s.blinkStart == null) {
            s.blinkStart = now
            s.nextBlink = now + Random.nextDouble(2.2, 5.5)
        }
        s.blinkStart?.let { b ->
            val u = ((now - b) / 0.2).toFloat()
            if (u >= 1) s.blinkStart = null else blink = max(0.08f, abs(1 - 2 * u))
        }
    }
    return p to blink
}

/**
 * Animations off: the mood's resting pose at once, symbol and all, with no
 * springs to wait out.
 */
private fun settle(s: MascotState, mood: Mood): Pose {
    // No `done` interlude: nothing would end it.
    s.mood = mood
    s.hopStart = null
    s.glance = floatArrayOf(0f, 0f)
    s.nextGlance = Double.POSITIVE_INFINITY
    // Far enough in that the mood's opening moves are over, short of the doze.
    val p = pose(s, s.moodStart + 1)
    for (i in 0 until 5) s.head[i].reset(p.head[i])
    for (i in 0 until 2) s.look[i].reset(p.look[i])
    for (i in 0 until 3) s.eye[i].reset(p.eye[i])
    s.prop = p.prop
    s.propAlpha = if (p.prop == null) 0f else 1f
    return p
}

private fun DrawScope.draw(
    s: MascotState, art: MascotArt, mood: Mood, still: Boolean, small: Boolean, top: Float,
    propColor: Color, askColor: Color,
) {
    val now = now()
    val (p, blink) = step(s, mood, still, now)
    val k = size.width / BOX.width
    translate(-BOX.left * k, (top - BOX.top) * k) {
        scale(k, k, pivot = Offset.Zero) {
            // Squash and turn about the chin.
            translate(CX + s.head[0].x, FLOOR + s.head[1].x) {
                rotate(s.head[2].x, pivot = Offset.Zero) {
                    scale(s.head[3].x, s.head[4].x, pivot = Offset.Zero) {
                        translate(-CX, -FLOOR) {
                            // The ink parts alone, then the head with them as
                            // holes, as the desktop paints it: nothing shows
                            // behind the head on any ground.
                            drawPath(art.ink, Brand.furInk)
                            drawPath(art.body, Brand.fur)
                            eyes(s, art, p, blink, small)
                        }
                    }
                }
            }
            if (top > 0) {
                val prop = s.prop
                if (prop != null && s.propAlpha > 0.02f) {
                    val anchor = Offset(CX + s.head[0].x * 0.5f, -19.3f + s.head[1].x * 0.6f)
                    val grow = 0.6f + 0.4f * s.propAlpha
                    translate(anchor.x, anchor.y) {
                        scale(grow, grow, pivot = Offset.Zero) {
                            drawProp(prop, now - s.moodStart, if (prop == Prop.ASK) askColor else propColor, s.propAlpha)
                        }
                    }
                }
            }
        }
    }
}

private fun DrawScope.eyes(s: MascotState, art: MascotArt, p: Pose, blink: Float, small: Boolean) {
    val bigger = if (small) 1.3f else 1f
    art.eyes.forEachIndexed { i, r ->
        val mirror = if (i == 0) 1f else -1f
        val w = r.width * s.eye[0].x * bigger
        val h = r.height * max(0.06f, s.eye[1].x) * blink * bigger
        val c = Offset(r.center.x + s.look[0].x * 4.5f, r.center.y + s.look[1].x * 4f)
        translate(c.x, c.y) {
            rotate(s.eye[2].x * mirror, pivot = Offset.Zero) {
                when (p.eyes) {
                    Eyes.OPEN -> drawRoundRect(
                        Brand.amber, Offset(-w / 2, -h / 2), Size(w, h), CornerRadius(min(w, h) / 2),
                    )
                    Eyes.HAPPY -> drawPath(
                        Path().apply {
                            moveTo(-w * 0.75f, 4f)
                            quadraticTo(0f, -10f, w * 0.75f, 4f)
                        },
                        Brand.amber, style = Stroke(5.5f, cap = StrokeCap.Round),
                    )
                    Eyes.CROSS -> {
                        val st = Stroke(5.5f, cap = StrokeCap.Round)
                        drawPath(Path().apply { moveTo(-7f, -7f); lineTo(7f, 7f); moveTo(7f, -7f); lineTo(-7f, 7f) }, Brand.amber, style = st)
                    }
                }
            }
        }
    }
}

private fun DrawScope.drawProp(prop: Prop, t: Double, color: Color, alpha: Float) {
    val line = Stroke(7f, cap = StrokeCap.Round, join = StrokeJoin.Round)
    when (prop) {
        Prop.DOTS -> for (i in 0 until 3) {
            val u = max(0.0, sin(t * 5.2 - i * 0.95)).toFloat()
            drawCircle(color, 7f, Offset(-21f + 21f * i, -9 * u), alpha = alpha * (0.4f + 0.6f * u))
        }
        Prop.ASK -> translate(0f, (-7 * abs(sin(t * PI))).toFloat()) {
            rotate((6 * sin(t * 3.1)).toFloat(), pivot = Offset.Zero) {
                val hook = Path()
                for (k in 0..14) {
                    val u = -PI + k / 14.0 * 1.3 * PI
                    val x = (9 * cos(u)).toFloat()
                    val y = (-9 + 9 * sin(u)).toFloat()
                    if (k == 0) hook.moveTo(x, y) else hook.lineTo(x, y)
                }
                hook.lineTo(0f, 3f)
                drawPath(hook, color, alpha = alpha, style = line)
                drawCircle(color, 4.5f, Offset(0f, 13f), alpha = alpha)
            }
        }
        Prop.BANG -> {
            val dx = if (t < 0.8) (5 * sin(t * 42) * exp(-3 * t)).toFloat() else 0f
            translate(dx, 0f) {
                drawLine(color, Offset(0f, -18f), Offset(0f, 1f), 8f, StrokeCap.Round, alpha = alpha)
                drawCircle(color, 4.5f, Offset(0f, 12f), alpha = alpha)
            }
        }
        Prop.CHECK -> {
            val tick = Path().apply { moveTo(-15f, 0f); lineTo(-5f, 11f); lineTo(16f, -12f) }
            val measure = PathMeasure().apply { setPath(tick, false) }
            val part = Path()
            measure.getSegment(0f, measure.length * min(1.0, t / 0.3).toFloat(), part, true)
            drawPath(part, color, alpha = alpha, style = Stroke(9f, cap = StrokeCap.Round, join = StrokeJoin.Round))
        }
        Prop.ZZZ -> for (i in 0 until 3) {
            // Three z's drifting up and fading, one after the other.
            val u = (t * 0.45 + i / 3.0) % 1.0
            val size = (8 + 8 * u).toFloat()
            translate((4 + 48 * u).toFloat(), (12 - 40 * u).toFloat()) {
                val z = Path().apply {
                    moveTo(-size / 2, -size / 2)
                    lineTo(size / 2, -size / 2)
                    lineTo(-size / 2, size / 2)
                    lineTo(size / 2, size / 2)
                }
                drawPath(z, color, alpha = alpha * sin(u * PI).toFloat(), style = Stroke(4f, cap = StrokeCap.Round, join = StrokeJoin.Round))
            }
        }
    }
}
