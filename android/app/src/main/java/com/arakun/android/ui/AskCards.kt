package com.arakun.android.ui

import android.view.HapticFeedbackConstants
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateList
import androidx.compose.runtime.toMutableStateList
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.arakun.android.R
import com.arakun.kit.PermissionAsk
import com.arakun.kit.QuestionAsk
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** The frame both asks share: amber, since they are what the session waits on. */
@Composable
private fun AskFrame(icon: Int, title: String, trailing: String? = null, content: @Composable () -> Unit) {
    val c = Palette.current
    Column(
        Modifier.fillMaxWidth().card(c.amberSoft, Brand.amber.copy(alpha = 0.4f)).padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            BrandIcon(icon, 15.dp, c.amberInk)
            Eyebrow(title, c.amberInk)
            Spacer(Modifier.weight(1f))
            trailing?.let { Text(it, style = mono(11f), color = c.textFaint) }
        }
        content()
    }
}

/** The agent waits to be allowed to do something. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun PermissionCard(ask: PermissionAsk, enabled: Boolean, pick: (PermissionAsk.Option) -> Unit) {
    val c = Palette.current
    // The allow-always option waiting for its second yes.
    // Keyed by the ask, so a newer ask never confirms the older one's option.
    var always by remember(ask.toolCallId) { mutableStateOf<PermissionAsk.Option?>(null) }
    AskFrame(R.drawable.ic_hand, "Permission") {
        Text(
            ask.title,
            style = Type.code,
            maxLines = 6,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.fillMaxWidth().background(c.deep, RoundedCornerShape(Radius.control)).padding(10.dp),
        )
        // As the phone page has it, a stray touch allows nothing: a one-time
        // allow is held, an allow for the session asks a second time. A reject
        // is red text.
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            ask.options.forEach { o ->
                when {
                    o.kind == "allow_always" -> OutlineButton(o.name, { always = o }, enabled = enabled)
                    o.kind.startsWith("allow") -> HoldButton(o.name, enabled) { pick(o) }
                    else -> OutlineButton(o.name, { pick(o) }, enabled = enabled, tone = if (o.kind.startsWith("reject")) c.failure else c.text)
                }
            }
        }
    }
    always?.let { o ->
        AlertDialog(
            onDismissRequest = { always = null },
            title = { Text("Allow this for the rest of the session?", style = geist(18f, FontWeight.SemiBold)) },
            text = { Text("The agent will not ask again before doing it.", style = Type.callout, color = c.textMuted) },
            confirmButton = {
                // Offline the pick would be dropped; the dialog waits instead.
                TextButton(
                    onClick = {
                        always = null
                        pick(o)
                    },
                    enabled = enabled,
                ) { Text(o.name, color = if (enabled) c.amberInk else c.textFaint, style = geist(15f, FontWeight.Medium)) }
            },
            dismissButton = {
                TextButton(onClick = { always = null }) { Text("Cancel", color = c.textMuted, style = geist(15f, FontWeight.Medium)) }
            },
            containerColor = c.raised,
        )
    }
}

private const val HOLD_MS = 700

/** The primary button, answered only once the finger has stayed down while it fills. */
@Composable
private fun HoldButton(title: String, enabled: Boolean, action: () -> Unit) {
    val fill = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    val view = LocalView.current
    val shape = RoundedCornerShape(Radius.control)
    Box(
        Modifier
            .heightIn(min = 40.dp)
            .alpha(if (enabled) 1f else 0.4f)
            .clip(shape)
            .background(Brand.amber, shape)
            // TalkBack's double tap is deliberate already.
            .semantics(mergeDescendants = true) {
                role = Role.Button
                contentDescription = title
                if (!enabled) disabled()
                onClick(label = title) {
                    if (enabled) action()
                    enabled
                }
            }
            .pointerInput(enabled) {
                if (!enabled) return@pointerInput
                detectTapGestures(onPress = {
                    // The fill only shows the hold: with animations off it is
                    // full at once, so the time is kept apart from it.
                    val filling = scope.launch { fill.animateTo(1f, tween(HOLD_MS, easing = LinearEasing)) }
                    val held = scope.launch {
                        delay(HOLD_MS.toLong())
                        view.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                        action()
                    }
                    tryAwaitRelease()
                    if (held.isActive) held.cancel()
                    filling.cancel()
                    scope.launch { fill.animateTo(0f, tween(150)) }
                })
            },
        contentAlignment = Alignment.CenterStart,
    ) {
        Box(Modifier.matchParentSize()) {
            Box(Modifier.fillMaxHeight().fillMaxWidth(fill.value).background(Brand.onAmber.copy(alpha = 0.18f)))
        }
        Text(
            "Hold to ${title.lowercase()}",
            style = geist(15f, FontWeight.SemiBold),
            color = Brand.onAmber,
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
        )
    }
}

/**
 * The agent asks one or more questions; each takes options, words of your
 * own, or both.
 */
@Composable
fun QuestionCard(ask: QuestionAsk, enabled: Boolean, answer: (List<String>) -> Unit, skip: () -> Unit) {
    val c = Palette.current
    // Saved, so turning the phone keeps the page, the picks and the words.
    // The caller keys this card by the ask, so a new ask starts empty.
    var page by rememberSaveable { mutableIntStateOf(0) }
    val picked = rememberSaveable(
        saver = listSaver<SnapshotStateList<Set<Int>>, IntArray>(
            save = { list -> list.map { it.sorted().toIntArray() } },
            restore = { saved -> saved.map { it.toSet() }.toMutableStateList() },
        ),
    ) { mutableStateListOf<Set<Int>>().apply { repeat(ask.questions.size) { add(emptySet()) } } }
    val typed = rememberSaveable(
        saver = listSaver<List<TextFieldState>, String>(
            save = { list -> list.map { it.text.toString() } },
            restore = { saved -> saved.map { TextFieldState(it) } },
        ),
    ) { List(ask.questions.size) { TextFieldState() } }
    var confirmSkip by rememberSaveable { mutableStateOf(false) }

    fun answerOf(i: Int): String {
        val q = ask.questions[i]
        val parts = picked[i].sorted().map { q.options[it] }.toMutableList()
        typed[i].text.trim().toString().takeIf { it.isNotEmpty() }?.let(parts::add)
        return parts.joinToString(", ")
    }

    val n = ask.questions.size
    AskFrame(R.drawable.ic_help_circle, "Question", trailing = if (n > 1) "${page + 1} of $n" else null) {
        if (n == 0) {
            // Nothing to answer, but the agent still waits on it.
            Text("The agent asked no question.", style = Type.small, color = c.textMuted)
            OutlineButton("Skip", skip, enabled = enabled)
            return@AskFrame
        }
        val q = ask.questions[page.coerceAtMost(n - 1)]
        val last = page >= n - 1
        if (n > 1 && q.topic.isNotEmpty()) Text(q.topic, style = Type.small, color = c.textMuted)
        Text(q.question, style = geist(16f, FontWeight.Medium))
        if (q.multi) Text("Pick any that apply.", style = Type.small, color = c.textMuted)
        Column(
            Modifier.heightIn(max = 200.dp).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            q.options.forEachIndexed { j, o ->
                val on = j in picked[page]
                OptionRow(o, on, q.multi, enabled) {
                    picked[page] = when {
                        on -> picked[page] - j
                        q.multi -> picked[page] + j
                        else -> setOf(j)
                    }
                }
            }
        }
        val shape = RoundedCornerShape(Radius.control)
        Field(
            state = typed[page],
            placeholder = "Or answer in your own words",
            enabled = enabled,
            modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp).background(c.raised, shape).border(1.dp, c.lineStrong, shape),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlineButton("Skip", { confirmSkip = true }, enabled = enabled)
            if (page > 0) OutlineButton("Back", { page -= 1 }, enabled = enabled)
            Spacer(Modifier.weight(1f))
            val ready = if (last) (0 until n).none { answerOf(it).isEmpty() } else answerOf(page).isNotEmpty()
            PrimaryButton(
                onClick = { if (last) answer((0 until n).map(::answerOf)) else page += 1 },
                enabled = enabled && ready,
                compact = true,
            ) { Text(if (last) "Answer" else "Next question") }
        }
    }
    if (confirmSkip) {
        AlertDialog(
            onDismissRequest = { confirmSkip = false },
            title = { Text("Skip all the questions?", style = geist(18f, FontWeight.SemiBold)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmSkip = false
                        skip()
                    },
                    enabled = enabled,
                ) { Text("Skip all", color = if (enabled) c.failure else c.textFaint, style = geist(15f, FontWeight.Medium)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmSkip = false }) { Text("Cancel", color = c.textMuted, style = geist(15f, FontWeight.Medium)) }
            },
            containerColor = c.raised,
        )
    }
}

@Composable
private fun OptionRow(text: String, on: Boolean, multi: Boolean, enabled: Boolean, toggle: () -> Unit) {
    val c = Palette.current
    val shape = RoundedCornerShape(Radius.control)
    Row(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(if (on) c.raised else androidx.compose.ui.graphics.Color.Transparent, shape)
            .border(1.dp, if (on) Brand.amber.copy(alpha = 0.5f) else c.line, shape)
            .toggleable(on, enabled = enabled, role = if (multi) Role.Checkbox else Role.RadioButton) { toggle() }
            .padding(horizontal = 12.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val markShape = if (multi) RoundedCornerShape(Radius.chip) else CircleShape
        Box(Modifier.size(18.dp).border(1.5.dp, if (on) c.amberInk else c.lineStrong, markShape), contentAlignment = Alignment.Center) {
            if (on) Box(Modifier.size(10.dp).background(c.amberInk, if (multi) RoundedCornerShape(2.dp) else CircleShape))
        }
        Text(text, style = Type.callout, modifier = Modifier.weight(1f))
    }
}
