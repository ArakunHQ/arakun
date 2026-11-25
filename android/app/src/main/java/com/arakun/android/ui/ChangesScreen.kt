package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.kit.ChangedFile
import com.arakun.kit.ChangedTurn
import com.arakun.kit.DiffLine
import com.arakun.kit.FileDiff
import com.arakun.kit.LiveSession

/** What is open over a session: the files it changed, or one file's diffs. */
sealed interface ChangesSheet {
    data object Files : ChangesSheet
    data class Diff(val file: ChangedFile) : ChangesSheet
}

/** The sheet over the session, in the session's own chrome. `open(null)` closes it. */
@Composable
fun ChangesSheetView(model: AppModel, s: LiveSession, sheet: ChangesSheet, open: (ChangesSheet?) -> Unit) {
    Column(
        Modifier
            .fillMaxSize()
            .background(Palette.current.ground)
            // Nothing under the sheet takes a tap meant for it.
            .pointerInput(Unit) {},
    ) {
        when (sheet) {
            ChangesSheet.Files -> FilesSheet(model, s, open)
            // A diff goes back to the list it was picked from.
            is ChangesSheet.Diff -> DiffSheet(model, sheet.file) { open(ChangesSheet.Files) }
        }
    }
}

@Composable
private fun SheetTopBar(title: String, sub: String, onBack: () -> Unit) {
    val c = Palette.current
    Box(
        Modifier
            .fillMaxWidth()
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
        Column(
            Modifier.align(Alignment.Center).widthIn(max = 260.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(title, style = geist(15f, FontWeight.SemiBold), maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(sub, style = mono(11f), color = c.textMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

@Composable
private fun Waiting(model: AppModel) {
    val c = Palette.current
    Column(
        Modifier.fillMaxWidth().padding(top = 60.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        if (model.canSend) {
            CircularProgressIndicator(Modifier.size(22.dp), color = c.textMuted, strokeWidth = 2.dp)
        } else {
            // The request went nowhere; it is sent again when the link is back.
            Text("Waiting for the computer", style = Type.callout, color = c.textMuted)
        }
    }
}

/** Every file the session changed, turn by turn with the newest first. */
@Composable
private fun FilesSheet(model: AppModel, s: LiveSession, open: (ChangesSheet?) -> Unit) {
    val c = Palette.current
    // Asked when the sheet opens, again when a turn changes more, and again
    // when the link comes back: a request sent offline is dropped.
    LaunchedEffect(s.id, s.changes, model.canSend) { model.loadChanges() }
    val turns = model.changes
    SheetTopBar("Changes", s.title.ifEmpty { "This session" }) { open(null) }
    when {
        turns == null -> Waiting(model)
        turns.isEmpty() -> Box(Modifier.fillMaxWidth().padding(top = 60.dp), contentAlignment = Alignment.Center) {
            Text("No files changed", style = Type.callout, color = c.textMuted)
        }
        else -> {
            val now = rememberNow(30_000)
            LazyColumn(
                Modifier.fillMaxSize().navigationBarsPadding(),
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                turns.forEachIndexed { k, turn ->
                    item(key = "t$k") { TurnHeader(turn, now, first = k == 0) }
                    itemsIndexed(turn.files, key = { j, _ -> "t$k-f$j" }) { _, f ->
                        FileRow(f) { open(ChangesSheet.Diff(f)) }
                    }
                }
            }
        }
    }
}

@Composable
private fun TurnHeader(turn: ChangedTurn, now: Long, first: Boolean) {
    val c = Palette.current
    Row(
        Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = if (first) 0.dp else 12.dp, bottom = 2.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            turn.text.ifEmpty { "Prompt" },
            style = Type.small,
            fontStyle = if (turn.text.isEmpty()) FontStyle.Italic else FontStyle.Normal,
            color = c.textMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (turn.at > 0) Text(shortAgo(turn.at, now), style = mono(11.5f), color = c.textFaint)
    }
}

@Composable
private fun FileRow(f: ChangedFile, onClick: () -> Unit) {
    val c = Palette.current
    Row(
        Modifier.fillMaxWidth().card().tap(onClick = onClick).padding(horizontal = 12.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(f.name, style = mono(13.5f), maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
            // The end of a long folder says more than its start.
            if (f.dir.isNotEmpty()) {
                Text(f.dir, style = Type.caption, color = c.textFaint, maxLines = 1, overflow = TextOverflow.StartEllipsis)
            }
        }
        if (f.diffs.size > 1) Text("${f.diffs.size} edits", style = Type.caption, color = c.textFaint)
        Changes(f.added, f.removed)
        BrandIcon(R.drawable.ic_arrow_right01, 13.dp, c.textFaint)
    }
}

/** A file's edits in one turn, in order. Read-only: the phone changes no file. */
@Composable
private fun DiffSheet(model: AppModel, file: ChangedFile, onBack: () -> Unit) {
    // Again when the link comes back, for the ones that did not come.
    LaunchedEffect(file, model.canSend) { model.loadDiffs(file.diffs) }
    SheetTopBar(file.name, "Read-only · edit on the computer", onBack)
    val n = file.diffs.size
    SelectionContainer {
        LazyColumn(Modifier.fillMaxSize().navigationBarsPadding(), contentPadding = PaddingValues(vertical = 8.dp)) {
            file.diffs.forEachIndexed { k, ref ->
                val d = model.diffs[ref]
                item(key = "m$k") { DiffMeta(d, file.path, if (n > 1) "Edit ${k + 1} of $n" else null, waiting = !model.canSend) }
                if (d != null) diffLines(k, d)
            }
        }
    }
}

@Composable
private fun DiffMeta(d: FileDiff?, path: String, nth: String?, waiting: Boolean) {
    val c = Palette.current
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        nth?.let { Text(it, style = geist(13f, FontWeight.SemiBold), color = c.textMuted) }
        Text(
            d?.path ?: path,
            style = mono(11.5f),
            color = c.textMuted,
            maxLines = 1,
            overflow = TextOverflow.StartEllipsis,
            modifier = Modifier.weight(1f),
        )
        when {
            d == null -> Text(if (waiting) "Waiting for the computer" else "Loading…", style = Type.caption, color = c.textFaint)
            d.newFile -> Text("New file", style = Type.caption, color = c.textFaint)
            d.deleted -> Text("Deleted", style = Type.caption, color = c.textFaint)
        }
        if (d != null) Changes(d.added, d.removed)
    }
}

private fun LazyListScope.diffLines(k: Int, d: FileDiff) {
    itemsIndexed(d.lines, key = { j, _ -> "d$k-$j" }, contentType = { _, l -> l is DiffLine.Hunk }) { _, l ->
        when (l) {
            is DiffLine.Hunk -> HunkRow(l.text)
            is DiffLine.Text -> LineRow(l)
        }
    }
    if (d.cut > 0) {
        item(key = "c$k") {
            Text(
                "… ${d.cut} more lines",
                style = Type.small,
                color = Palette.current.textFaint,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
            )
        }
    }
}

@Composable
private fun HunkRow(text: String) {
    val c = Palette.current
    Text(
        text,
        style = mono(11f),
        color = c.textFaint,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.fillMaxWidth().background(c.raised).padding(horizontal = 16.dp, vertical = 4.dp),
    )
}

/**
 * One line: its number, its mark and its text. A long line wraps, so a diff
 * reads top to bottom on a narrow screen without scrolling sideways.
 */
@Composable
private fun LineRow(l: DiffLine.Text) {
    val c = Palette.current
    val tone = when (l.mark) {
        '+' -> c.success
        '-' -> c.failure
        else -> null
    }
    Row(
        Modifier
            .fillMaxWidth()
            .drawBehind {
                if (tone != null) {
                    drawRect(tone.copy(alpha = 0.12f))
                    drawRect(tone, size = Size(2.dp.toPx(), size.height))
                }
            }
            .padding(end = 12.dp),
    ) {
        Text(
            l.number?.toString() ?: "",
            style = mono(11.5f).copy(lineHeight = Type.code.lineHeight),
            color = c.textFaint,
            textAlign = TextAlign.End,
            maxLines = 1,
            modifier = Modifier.width(44.dp).padding(end = 6.dp),
        )
        Text(
            when (l.mark) {
                '+' -> "+"
                '-' -> "−"
                else -> ""
            },
            style = mono(11.5f, FontWeight.Medium).copy(lineHeight = Type.code.lineHeight),
            color = tone ?: c.textFaint,
            textAlign = TextAlign.Center,
            modifier = Modifier.width(14.dp),
        )
        Text(
            l.text,
            style = mono(11.5f).copy(lineHeight = Type.code.lineHeight),
            color = tone ?: c.textMuted,
            modifier = Modifier.weight(1f),
        )
    }
}
