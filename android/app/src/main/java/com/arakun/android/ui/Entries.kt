package com.arakun.android.ui

import android.text.format.DateFormat
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.arakun.android.R
import com.arakun.kit.Entry
import java.text.NumberFormat
import java.util.Date

/** One entry of the transcript. */
@Composable
fun EntryView(entry: Entry) {
    val c = Palette.current
    when (entry) {
        is Entry.User -> UserEntry(entry)
        is Entry.Agent -> SelectionContainer {
            Text(markdown(entry.text, c), style = Type.body.copy(lineHeight = Type.body.lineHeight * 1.08f), modifier = Modifier.fillMaxWidth())
        }
        is Entry.Thought -> Fold(R.drawable.ic_ai_idea, "Thinking") {
            SelectionContainer { Text(entry.text, style = Type.small, color = c.textMuted) }
        }
        is Entry.Tool -> ToolView(entry)
        is Entry.Plan -> PlanView(entry)
        is Entry.Notice -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            val tone = if (entry.error) c.failure else c.textMuted
            BrandIcon(if (entry.error) R.drawable.ic_alert_circle else R.drawable.ic_help_circle, 15.dp, tone, Modifier.padding(top = 1.dp))
            Text(entry.text, style = Type.small, color = tone)
        }
        is Entry.Compacted -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Box(Modifier.weight(1f).height(1.dp).background(c.line))
            val n = NumberFormat.getIntegerInstance()
            Text(
                if (entry.before != null && entry.after != null) {
                    "Compacted ${n.format(entry.before)} → ${n.format(entry.after)} tokens"
                } else {
                    "Compacted"
                },
                style = mono(11f),
                color = c.textFaint,
            )
            Box(Modifier.weight(1f).height(1.dp).background(c.line))
        }
        Entry.Unknown -> {}
    }
}

@Composable
private fun UserEntry(e: Entry.User) {
    val c = Palette.current
    val context = LocalContext.current
    Row(Modifier.fillMaxWidth().padding(top = 6.dp)) {
        Spacer(Modifier.weight(1f).widthIn(min = 48.dp))
        Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.weight(5f, fill = false)) {
            SelectionContainer {
                Text(e.text, style = Type.body, modifier = Modifier.card().padding(horizontal = 14.dp, vertical = 10.dp))
            }
            e.pastes.forEach { PasteView(it) }
            // The pictures themselves come through the channel; the app names them for now.
            e.images.forEach { name ->
                Row(
                    Modifier.background(c.deep, RoundedCornerShape(Radius.control)).padding(horizontal = 10.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    BrandIcon(R.drawable.ic_file01, 13.dp, c.textMuted)
                    Text(name, style = mono(11.5f), color = c.textMuted, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                }
            }
            val time = DateFormat.getTimeFormat(context).format(Date(e.at))
            Text(
                listOfNotNull(time, e.elapsedMs?.let(::elapsed)).joinToString(" · "),
                style = mono(11f),
                color = c.textFaint,
            )
        }
    }
}

/** "1m 35s", "12s": how long a turn took. */
private fun elapsed(ms: Int): String {
    val s = ms / 1000
    return if (s >= 60) "${s / 60}m ${s % 60}s" else "${s}s"
}

/** A pasted block, four lines until tapped open. */
@Composable
private fun PasteView(text: String) {
    val c = Palette.current
    var open by rememberSaveable { mutableStateOf(false) }
    val long = text.split('\n').size > 4
    Column(
        Modifier.fillMaxWidth().background(c.deep, RoundedCornerShape(Radius.control)).padding(10.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        SelectionContainer {
            Text(text, style = Type.code, color = c.textMuted, maxLines = if (open) Int.MAX_VALUE else 4, overflow = TextOverflow.Ellipsis)
        }
        if (long) {
            Text(
                if (open) "Show less" else "Show all",
                style = mono(11.5f),
                color = c.amberInk,
                modifier = Modifier.tap { open = !open },
            )
        }
    }
}

/** A row that opens to show more: thinking, a tool's output. */
@Composable
fun Fold(
    icon: Int,
    title: String,
    mono: Boolean = false,
    trailing: @Composable () -> Unit = {},
    content: @Composable () -> Unit,
) {
    val c = Palette.current
    var open by rememberSaveable { mutableStateOf(false) }
    val turn by animateFloatAsState(if (open) 90f else 0f, label = "fold")
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(
            Modifier
                .fillMaxWidth()
                .tap { open = !open }
                .semantics { stateDescription = if (open) "Expanded" else "Collapsed" }
                .padding(vertical = 2.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            BrandIcon(icon, 15.dp, c.textFaint)
            Text(
                title,
                style = if (mono) Type.code else Type.small,
                color = c.textMuted,
                maxLines = 1,
                overflow = TextOverflow.MiddleEllipsis,
                modifier = Modifier.weight(1f),
            )
            trailing()
            BrandIcon(R.drawable.ic_arrow_right01, 13.dp, c.textFaint, Modifier.rotate(turn))
        }
        AnimatedVisibility(open, enter = fadeIn(), exit = fadeOut()) {
            Box(Modifier.padding(start = 25.dp)) { content() }
        }
    }
}

/** A tool call, folded to its title; open it for the command and output. */
@Composable
private fun ToolView(tool: Entry.Tool) {
    val c = Palette.current
    val icon = when (tool.kind) {
        "execute" -> R.drawable.ic_computer_terminal01
        "edit" -> R.drawable.ic_pencil_edit01
        "read" -> R.drawable.ic_file01
        "search" -> R.drawable.ic_search01
        "fetch" -> R.drawable.ic_globe
        "delete" -> R.drawable.ic_delete02
        "think" -> R.drawable.ic_ai_idea
        else -> R.drawable.ic_wrench01
    }
    Fold(
        icon,
        tool.title.ifEmpty { tool.kind },
        mono = true,
        trailing = {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                Changes(tool.added, tool.removed)
                when (tool.status) {
                    "in_progress", "pending" -> CircularProgressIndicator(Modifier.size(12.dp), color = c.attention, strokeWidth = 1.5.dp)
                    "failed" -> Row(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                        BrandIcon(R.drawable.ic_mark_cross, 12.dp, c.failure)
                        Text("Failed", style = mono(11f), color = c.failure)
                    }
                }
            }
        },
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            tool.diffs.forEach { d ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(d.path, style = Type.code, color = c.textMuted, maxLines = 1, overflow = TextOverflow.StartEllipsis, modifier = Modifier.weight(1f))
                    Changes(d.added, d.removed)
                }
            }
            val output = tool.output?.takeIf { it.isNotEmpty() }
            if (tool.command != null || output != null) {
                val shape = RoundedCornerShape(Radius.control)
                Column(
                    Modifier.fillMaxWidth().background(c.deep, shape).border(1.dp, c.line, shape).padding(10.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    SelectionContainer {
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            tool.command?.let { Text("$ $it", style = Type.code) }
                            output?.let {
                                Text(
                                    it,
                                    style = mono(11.5f),
                                    color = c.textMuted,
                                    softWrap = false,
                                    modifier = Modifier.horizontalScroll(rememberScrollState()),
                                )
                            }
                        }
                    }
                }
            }
            tool.exitCode?.takeIf { it != 0 }?.let { Text("Exit code $it", style = mono(11.5f), color = c.failure) }
        }
    }
}

@Composable
private fun PlanView(plan: Entry.Plan) {
    val c = Palette.current
    Column(Modifier.fillMaxWidth().card().padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            BrandIcon(R.drawable.ic_check_list, 14.dp, c.textFaint)
            Eyebrow("Plan")
            Spacer(Modifier.weight(1f))
            Text("${plan.items.count { it.status == "completed" }} of ${plan.items.size}", style = mono(11f), color = c.textFaint)
        }
        plan.items.forEach { item ->
            val done = item.status == "completed"
            val now = item.status == "in_progress"
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                BrandIcon(
                    when {
                        done -> R.drawable.ic_checkmark_circle02
                        now -> R.drawable.ic_circle_dashed
                        else -> R.drawable.ic_circle
                    },
                    16.dp,
                    when {
                        now -> c.amberInk
                        done -> c.textMuted
                        else -> c.textFaint
                    },
                    Modifier.padding(top = 2.dp),
                )
                Text(
                    item.content,
                    style = Type.callout.copy(textDecoration = if (done) TextDecoration.LineThrough else null),
                    color = if (done) c.textFaint else c.text,
                )
            }
        }
    }
}

