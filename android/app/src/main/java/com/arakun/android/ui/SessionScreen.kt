package com.arakun.android.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.kit.Entry
import com.arakun.kit.LiveSession
import com.arakun.kit.SessionStatus
import kotlinx.coroutines.launch

/** One session, live: its entries, what it asks, and the composer. */
@Composable
fun SessionScreen(model: AppModel, id: String) {
    val c = Palette.current
    val s = model.session?.takeIf { it.id == id }
    val draft = rememberTextFieldState()
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    var unseen by remember { mutableStateOf(false) }
    // Whether the end of the transcript is on screen; new entries follow only then.
    val atEnd by remember { derivedStateOf { !list.canScrollForward } }
    LaunchedEffect(atEnd) { if (atEnd) unseen = false }
    var followed by remember { mutableStateOf(false) }
    // The session's changes or one of its diffs, over everything else here.
    var sheet by remember { mutableStateOf<ChangesSheet?>(null) }
    val focus = LocalFocusManager.current
    val lastIndex = (s?.let { 1 + (if (it.from > 0) 1 else 0) + it.entries.size + it.queue.size + 1 }) ?: 1

    val working = s?.status == SessionStatus.WORKING && s.turnStarted != null
    // The working line counts too: a turn can start without a new entry. So
    // does the last entry, which streaming text grows without adding one.
    LaunchedEffect(s?.count, s?.entries?.lastOrNull(), s?.queue?.size, working) {
        if (s == null) return@LaunchedEffect
        if (!followed) {
            // Opened at the end, as a chat is.
            followed = true
            list.scrollToItem(lastIndex)
        } else if (atEnd) {
            // Someone reading further up keeps their place.
            list.animateScrollToItem(lastIndex)
        } else {
            unseen = true
        }
    }

    Column(
        Modifier
            .fillMaxSize()
            .background(c.ground)
            // Under a sheet the session is not there for a screen reader either.
            .then(if (sheet != null) Modifier.clearAndSetSemantics {} else Modifier),
    ) {
        SessionTopBar(
            s,
            muted = model.isMuted(id),
            canMute = model.isLinked,
            knowsMuting = model.knowsMuting,
            onMute = { model.mute(id, !model.isMuted(id)) },
            onBack = model::close,
        )
        Box(Modifier.weight(1f)) {
            LazyColumn(
                state = list,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(18.dp),
            ) {
                if (s == null) {
                    item {
                        Box(Modifier.fillMaxWidth().padding(top = 60.dp), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(Modifier.size(22.dp), color = c.textMuted, strokeWidth = 2.dp)
                        }
                    }
                } else {
                    item(key = "header") {
                        SessionHeader(s) {
                            // The keyboard would stay up over the sheet.
                            focus.clearFocus()
                            sheet = ChangesSheet.Files
                        }
                    }
                    if (s.from > 0) {
                        item(key = "older") {
                            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                                OutlineButton(
                                    if (model.loadingOlder) "Loading…" else "Show ${s.from} earlier entries",
                                    model::loadOlder,
                                    // Offline the request would go nowhere.
                                    enabled = !model.loadingOlder && model.canSend,
                                )
                            }
                        }
                    }
                    itemsIndexed(s.entries, key = { i, _ -> "e" + (s.from + i) }) { _, e -> EntryView(e) }
                    itemsIndexed(s.queue, key = { i, _ -> "q$i" }) { _, q -> QueuedView(q) }
                    if (working) {
                        item(key = "working") { WorkingLine(s.turnStarted!!) }
                    }
                }
                // Room for the raccoon perched over the composer, inside the
                // list so scrolling to the end keeps the last entry clear of it.
                item(key = "end") { Spacer(Modifier.height(48.dp)) }
            }
            if (unseen) {
                OutlineButton(
                    onClick = { scope.launch { list.animateScrollToItem(lastIndex) } },
                    modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 8.dp),
                ) {
                    BrandIcon(R.drawable.ic_arrow_down01, 14.dp, LocalContentColor.current)
                    Text("New entries", style = geist(14f, FontWeight.Medium))
                }
            }
        }
        Box {
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(c.ground)
                    // The hairline first, so the raccoon perched across it is drawn over it.
                    .drawWithContent {
                        drawContent()
                        drawLine(c.line, Offset(0f, 0f), Offset(size.width, 0f), 1.dp.toPx())
                    }
                    .navigationBarsPadding()
                    .imePadding()
                    .padding(start = 12.dp, end = 12.dp, top = 10.dp, bottom = 6.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                ConnectionBanner(model)
                // Offline the cards stay but wait: an answer now would be lost.
                val linked = model.isLinked || model.demo != null
                s?.permission?.let { p ->
                    PermissionCard(p, enabled = linked) { model.allow(it) }
                } ?: s?.question?.let { q ->
                    androidx.compose.runtime.key(q.toolCallId) {
                        QuestionCard(q, enabled = linked, answer = model::answer, skip = model::skipQuestion)
                    }
                } ?: s?.secret?.let { r ->
                    androidx.compose.runtime.key(r.id) {
                        SecretCard(r, enabled = linked, save = model::saveSecret, decline = model::declineSecret)
                    }
                }
                Composer(
                    draft = draft,
                    working = s?.status == SessionStatus.WORKING,
                    linked = linked,
                    send = {
                        val text = draft.text.trim().toString()
                        if (text.isNotEmpty()) {
                            // Offline the prompt stays in the box, to send again.
                            if (model.say(text)) draft.clearText()
                        }
                    },
                    stop = model::stop,
                )
            }
            // Perched on the edge, over the last lines: it shows how the session is doing.
            Mascot(
                sessionMood(model, s),
                52.dp,
                Modifier.align(Alignment.TopEnd).offset(x = (-20).dp, y = (-50).dp),
            )
        }
    }
    val over = sheet
    if (over != null && s != null) {
        ChangesSheetView(model, s, over) { sheet = it }
        // Back leaves a diff for the list, and the list for the session.
        BackHandler { sheet = if (over is ChangesSheet.Diff) ChangesSheet.Files else null }
    }
}

private fun sessionMood(model: AppModel, s: LiveSession?): Mood {
    if (model.connection is AppModel.Connection.ComputerAway) return Mood.SLEEPING
    s ?: return Mood.THINKING
    return when (s.status) {
        SessionStatus.NEEDS_INPUT -> Mood.WAITING
        SessionStatus.ERROR -> Mood.ERROR
        SessionStatus.WORKING -> if (s.entries.lastOrNull() is Entry.Thought) Mood.THINKING else Mood.WORKING
        SessionStatus.IDLE -> Mood.IDLE
    }
}

@Composable
private fun SessionTopBar(
    s: LiveSession?,
    muted: Boolean,
    canMute: Boolean,
    knowsMuting: Boolean,
    onMute: () -> Unit,
    onBack: () -> Unit,
) {
    val c = Palette.current
    Box(
        Modifier
            .fillMaxWidth()
            .background(c.ground)
            .statusBarsPadding()
            .drawWithContent {
                drawContent()
                drawLine(c.line, Offset(0f, size.height), Offset(size.width, size.height), 1.dp.toPx())
            }
            .height(56.dp),
    ) {
        Box(Modifier.align(Alignment.CenterStart).padding(start = 4.dp)) {
            IconTap(R.drawable.ic_arrow_left01, "Back", c.text, onClick = onBack)
        }
        // A computer from before muting has no `/mute`: no switch for it.
        if (knowsMuting) {
            Box(Modifier.align(Alignment.CenterEnd).padding(end = 4.dp)) {
                IconTap(
                    if (muted) R.drawable.ic_notification_off01 else R.drawable.ic_notification01,
                    if (muted) "Unmute" else "Mute",
                    if (muted) c.textFaint else c.text,
                    enabled = canMute,
                    onClick = onMute,
                )
            }
        }
        Column(
            Modifier.align(Alignment.Center).widthIn(max = 260.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(
                s?.title?.ifEmpty { null } ?: "Session",
                style = geist(15f, FontWeight.SemiBold),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (s != null) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                    StatusDot(s.status)
                    Text(
                        listOf(s.agent, s.status.word).filter { it.isNotEmpty() }.joinToString(" · "),
                        style = mono(11f),
                        color = c.textMuted,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

/** What the session has cost and changed so far, above its first entry. */
@Composable
private fun SessionHeader(s: LiveSession, openChanges: () -> Unit) {
    val c = Palette.current
    Row(Modifier.fillMaxWidth().padding(bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        s.usage?.let { Text(it, style = mono(11.5f), color = c.textFaint) }
        Spacer(Modifier.weight(1f))
        if (s.changes.files > 0) {
            // The counts open the list of files; the outline says they can be tapped.
            Row(
                Modifier
                    .card(c.ground, c.line, Radius.control)
                    .tap(onClick = openChanges)
                    .semantics { contentDescription = "Show changes" }
                    .heightIn(min = 32.dp)
                    .padding(horizontal = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                BrandIcon(R.drawable.ic_file_diff, 13.dp, c.textFaint)
                Text(if (s.changes.files == 1) "1 file" else "${s.changes.files} files", style = mono(11.5f), color = c.textMuted)
                Changes(s.changes.added, s.changes.removed)
                BrandIcon(R.drawable.ic_arrow_right01, 11.dp, c.textFaint)
            }
        }
    }
}

/** The running turn's clock, under the last entry. */
@Composable
private fun WorkingLine(since: Long) {
    val now = rememberNow(1000)
    val secs = maxOf(0L, (now - since) / 1000)
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        StatusDot(SessionStatus.WORKING)
        Text("Working · %d:%02d".format(secs / 60, secs % 60), style = mono(12f), color = Palette.current.textMuted)
    }
}

@Composable
private fun Composer(
    draft: TextFieldState,
    working: Boolean,
    linked: Boolean,
    send: () -> Unit,
    stop: () -> Unit,
) {
    val c = Palette.current
    var typing by remember { mutableStateOf(false) }
    val empty = draft.text.isBlank()
    val shape = RoundedCornerShape(Radius.card)
    Row(
        Modifier.fillMaxWidth().background(c.raised, shape).border(1.dp, if (typing) c.lineStrong else c.line, shape),
        verticalAlignment = Alignment.Bottom,
    ) {
        Field(
            state = draft,
            placeholder = if (working) "Queue a follow-up" else "Message the agent",
            style = Type.body,
            lines = TextFieldLineLimits.MultiLine(maxHeightInLines = 6),
            padding = PaddingValues(horizontal = 14.dp, vertical = 12.dp),
            modifier = Modifier.weight(1f).onFocusChanged { typing = it.isFocused },
        )
        if (working && empty) {
            // Offline the cancel would be dropped, as a message would.
            Row(
                Modifier
                    .padding(6.dp)
                    .alpha(if (linked) 1f else 0.5f)
                    .heightIn(min = 34.dp)
                    .border(1.dp, c.failure.copy(alpha = 0.5f), RoundedCornerShape(Radius.control))
                    .tap(enabled = linked, onClick = stop)
                    .padding(horizontal = 12.dp, vertical = 7.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                BrandIcon(R.drawable.ic_square, 14.dp, c.failure)
                Text("Stop", style = geist(14f, FontWeight.Medium), color = c.failure)
            }
        } else {
            val ready = !empty && linked
            Box(
                Modifier
                    .padding(6.dp)
                    .size(34.dp)
                    .background(if (ready) Brand.amber else c.lineStrong, RoundedCornerShape(Radius.control))
                    .tap(enabled = ready, onClick = send)
                    .semantics { contentDescription = "Send" },
                contentAlignment = Alignment.Center,
            ) {
                BrandIcon(R.drawable.ic_arrow_up01, 18.dp, if (ready) Brand.onAmber else c.textFaint)
            }
        }
    }
}

@Composable
private fun QueuedView(text: String) {
    val c = Palette.current
    Row(Modifier.fillMaxWidth()) {
        Spacer(Modifier.weight(1f).widthIn(min = 48.dp))
        Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Eyebrow("Queued")
            Text(
                text,
                style = Type.callout,
                color = c.textMuted,
                modifier = Modifier
                    .drawWithContent {
                        drawContent()
                        val r = Radius.card.toPx()
                        drawRoundRect(
                            c.lineStrong,
                            cornerRadius = androidx.compose.ui.geometry.CornerRadius(r),
                            style = Stroke(1.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(4.dp.toPx(), 3.dp.toPx()))),
                        )
                    }
                    .padding(horizontal = 14.dp, vertical = 10.dp),
            )
        }
    }
}
