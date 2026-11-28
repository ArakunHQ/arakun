package com.arakun.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.arakun.android.R
import com.arakun.kit.Align
import com.arakun.kit.Block as MdBlock
import com.arakun.kit.Item
import com.arakun.kit.isFigure
import com.arakun.kit.parseMarkdown
import kotlinx.coroutines.delay

/**
 * An agent's reply as blocks: paragraphs, headings, lists, quotes, rules,
 * code and tables, each with its inline spans. The caller wraps it in a
 * SelectionContainer so the text stays selectable.
 */
@Composable
fun MarkdownView(text: String, modifier: Modifier = Modifier) {
    val blocks = remember(text) { parseMarkdown(text) }
    Blocks(blocks, modifier.fillMaxWidth())
}

/** The body's line height, a little looser for reading. */
private val Prose = Type.body.copy(lineHeight = Type.body.lineHeight * 1.08f)

@Composable
private fun Blocks(blocks: List<MdBlock>, modifier: Modifier = Modifier) {
    val c = Palette.current
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        blocks.forEachIndexed { n, b ->
            when (b) {
                is MdBlock.Paragraph -> Text(markdown(b.text, c), style = Prose)
                is MdBlock.Heading -> Text(
                    markdown(b.text, c),
                    style = when (b.level) {
                        1 -> geist(20f, FontWeight.SemiBold).copy(lineHeight = 27.sp)
                        2 -> geist(18f, FontWeight.SemiBold).copy(lineHeight = 25.sp)
                        else -> geist(16f, FontWeight.SemiBold).copy(lineHeight = 23.sp)
                    },
                    modifier = Modifier.padding(top = if (n > 0) 6.dp else 0.dp).semantics { heading() },
                )
                is MdBlock.Items -> ItemsView(b.items)
                is MdBlock.Quote -> {
                    val bar = c.lineStrong
                    Box(
                        Modifier
                            .drawBehind { drawRect(bar, size = Size(3.dp.toPx(), size.height)) }
                            .padding(start = 15.dp),
                    ) {
                        CompositionLocalProvider(LocalContentColor provides c.textMuted) { Blocks(b.blocks) }
                    }
                }
                MdBlock.Rule -> Box(Modifier.fillMaxWidth().padding(vertical = 6.dp).height(1.dp).background(c.line))
                is MdBlock.Code -> CodeView(b)
                is MdBlock.Table -> TableView(b)
            }
        }
    }
}

@Composable
private fun ItemsView(items: List<Item>) {
    val c = Palette.current
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        items.forEach { item ->
            Row(Modifier.padding(start = (item.depth * 18).dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                val marker = Modifier.widthIn(min = 16.dp)
                when {
                    item.checked != null -> BrandIcon(
                        if (item.checked == true) R.drawable.ic_checkmark_circle02 else R.drawable.ic_circle,
                        16.dp,
                        if (item.checked == true) c.amberInk else c.textFaint,
                        Modifier.padding(top = 4.dp).semantics { contentDescription = if (item.checked == true) "Done" else "To do" },
                    )
                    item.number != null -> Text("${item.number}.", style = Prose, color = c.textMuted, modifier = marker)
                    else -> Text(BULLETS[item.depth % BULLETS.size], style = Prose, color = c.textMuted, modifier = marker)
                }
                Text(markdown(item.text, c), style = Prose, modifier = Modifier.weight(1f))
            }
        }
    }
}

private val BULLETS = listOf("•", "◦", "▪")

/**
 * A fenced block: monospaced, scrolling sideways rather than wrapping, with
 * its language and a copy button. A chart or diagram shows as its code.
 */
@Composable
private fun CodeView(code: MdBlock.Code) {
    val c = Palette.current
    val context = LocalContext.current
    var copied by remember(code.text) { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(1500)
            copied = false
        }
    }
    Column(Modifier.fillMaxWidth().card(c.deep, c.line, Radius.control)) {
        Row(Modifier.fillMaxWidth().padding(start = 12.dp, end = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(code.lang, style = mono(11f), color = c.textFaint, modifier = Modifier.weight(1f))
            Row(
                Modifier
                    .minimumInteractiveComponentSize()
                    .tap {
                        context.getSystemService(ClipboardManager::class.java)
                            ?.setPrimaryClip(ClipData.newPlainText("Code", code.text))
                        copied = true
                    }
                    .semantics { contentDescription = if (copied) "Copied" else "Copy code" }
                    .padding(horizontal = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(5.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                BrandIcon(R.drawable.ic_copy01, 13.dp, c.textMuted)
                Text(if (copied) "Copied" else "Copy", style = mono(11f), color = c.textMuted)
            }
        }
        Text(
            code.text,
            style = Type.code,
            color = c.text,
            softWrap = false,
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(start = 12.dp, end = 12.dp, bottom = 12.dp),
        )
    }
}

/**
 * A table that scrolls sideways. Columns are as wide as their widest cell,
 * up to a cap past which long cells wrap; figures line up on the right.
 */
@Composable
private fun TableView(t: MdBlock.Table) {
    val c = Palette.current
    val cols = t.head.size
    val cap = with(LocalDensity.current) { 260.dp.roundToPx() }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        val n = t.rows.size
        Text(
            "$n row${if (n == 1) "" else "s"} · $cols column${if (cols == 1) "" else "s"}",
            style = mono(11f),
            color = c.textFaint,
        )
        Box(Modifier.fillMaxWidth().card(c.ground, c.line, Radius.control)) {
            Layout(
                content = {
                    (listOf(t.head) + t.rows).forEachIndexed { r, row ->
                        row.forEachIndexed { k, cell ->
                            val end = t.align[k] == Align.End || (r > 0 && isFigure(cell))
                            val align = when {
                                end -> Alignment.TopEnd
                                t.align[k] == Align.Center -> Alignment.TopCenter
                                else -> Alignment.TopStart
                            }
                            val line = c.line
                            Box(
                                Modifier
                                    .then(if (r == 0) Modifier.background(c.raised) else Modifier)
                                    .drawBehind {
                                        if (r > 0) drawLine(line, Offset(0f, 0f), Offset(size.width, 0f), 1.dp.toPx())
                                        if (k > 0) drawLine(line, Offset(0f, 0f), Offset(0f, size.height), 1.dp.toPx())
                                    }
                                    .padding(horizontal = 10.dp, vertical = 7.dp),
                                contentAlignment = align,
                            ) {
                                Text(
                                    markdown(cell, c),
                                    style = if (r == 0) Type.small.copy(fontWeight = FontWeight.SemiBold) else Type.small,
                                    textAlign = if (end) TextAlign.End else if (t.align[k] == Align.Center) TextAlign.Center else TextAlign.Start,
                                )
                            }
                        }
                    }
                },
                modifier = Modifier.horizontalScroll(rememberScrollState()),
            ) { measurables, _ ->
                val rows = measurables.chunked(cols)
                val widths = IntArray(cols) { k ->
                    val widest = rows.maxOf { it[k].maxIntrinsicWidth(Constraints.Infinity) }
                    // A long word or address keeps its line, up to twice the cap.
                    val word = rows.maxOf { it[k].minIntrinsicWidth(Constraints.Infinity) }
                    widest.coerceAtMost(cap).coerceAtLeast(word.coerceAtMost(cap * 2))
                }
                val heights = rows.map { row -> row.indices.maxOf { k -> row[k].minIntrinsicHeight(widths[k]) } }
                val placed = rows.mapIndexed { r, row -> row.mapIndexed { k, m -> m.measure(Constraints.fixed(widths[k], heights[r])) } }
                layout(widths.sum(), heights.sum()) {
                    var y = 0
                    placed.forEachIndexed { r, row ->
                        var x = 0
                        row.forEachIndexed { k, p ->
                            p.place(x, y)
                            x += widths[k]
                        }
                        y += heights[r]
                    }
                }
            }
        }
    }
}

/**
 * Inline Markdown, as the iOS app reads it, for the text inside a block:
 * `code` in Geist Mono on a faint well, **strong**, *emphasis* and links.
 * Line breaks and everything else stay as written.
 */
fun markdown(s: String, c: Palette): AnnotatedString = buildAnnotatedString {
    val code = SpanStyle(fontFamily = GeistMono, fontSize = 14.5.sp, background = c.lineStrong.copy(alpha = 0.6f))
    val link = TextLinkStyles(SpanStyle(color = c.amberInk))
    var i = 0
    val plain = StringBuilder()
    fun flush() {
        if (plain.isNotEmpty()) {
            append(plain.toString())
            plain.clear()
        }
    }
    // Whether the character at `at` follows an odd run of backslashes.
    fun escaped(at: Int): Boolean {
        var n = 0
        while (at - n - 1 >= 0 && s[at - n - 1] == '\\') n++
        return n % 2 == 1
    }
    // Whether a one-character marker at `at` belongs to a doubled run, as
    // each `*` of a `**` does.
    fun doubled(at: Int): Boolean {
        val m = s[at]
        return (at > 0 && s[at - 1] == m) || (at + 1 < s.length && s[at + 1] == m)
    }
    // A backslash in code is itself, so only other spans skip escaped markers.
    // A single `*` or `_` closer skips the `**` and `__` of a span inside it,
    // so `*a **b** c*` is one emphasis around a strong.
    fun closes(marker: String, from: Int, escapes: Boolean = true, single: Boolean = false): Int {
        var end = s.indexOf(marker, from)
        while (end >= 0 && ((escapes && escaped(end)) || (single && doubled(end)))) end = s.indexOf(marker, end + 1)
        // An empty span or one across a blank line is not one.
        return if (end > from && !s.substring(from, end).contains("\n\n")) end else -1
    }
    while (i < s.length) {
        val ch = s[i]
        when {
            ch == '\\' && i + 1 < s.length && s[i + 1] in "\\`*_[]()#" -> {
                plain.append(s[i + 1])
                i += 2
            }
            ch == '`' -> {
                val end = closes("`", i + 1, escapes = false)
                if (end < 0) { plain.append(ch); i++ } else {
                    flush()
                    withStyle(code) { append(s.substring(i + 1, end)) }
                    i = end + 1
                }
            }
            s.startsWith("**", i) || s.startsWith("__", i) -> {
                val marker = s.substring(i, i + 2)
                val end = closes(marker, i + 2)
                if (end < 0) { plain.append(marker); i += 2 } else {
                    flush()
                    withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(markdown(s.substring(i + 2, end), c)) }
                    i = end + 2
                }
            }
            (ch == '*' || ch == '_') && i + 1 < s.length && !s[i + 1].isWhitespace() &&
                // Snake_case words keep their underscores.
                !(ch == '_' && i > 0 && s[i - 1].isLetterOrDigit()) -> {
                val end = closes(ch.toString(), i + 1, single = true)
                if (end < 0 || s[end - 1].isWhitespace() || (ch == '_' && end + 1 < s.length && s[end + 1].isLetterOrDigit())) {
                    plain.append(ch)
                    i++
                } else {
                    flush()
                    withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(markdown(s.substring(i + 1, end), c)) }
                    i = end + 1
                }
            }
            ch == '[' -> {
                val close = closes("]", i + 1)
                val url = if (close > 0 && close + 1 < s.length && s[close + 1] == '(') closingParen(s, close + 2) else -1
                if (url < 0) { plain.append(ch); i++ } else {
                    flush()
                    val target = s.substring(close + 2, url)
                    val label = markdown(s.substring(i + 1, close), c)
                    // Only web links, as the phone page has it: another scheme
                    // could open some other app. The text stays, unlinked.
                    if (WEB.matches(target)) withLink(LinkAnnotation.Url(target, link)) { append(label) } else append(label)
                    i = url + 1
                }
            }
            else -> {
                plain.append(ch)
                i++
            }
        }
    }
    flush()
}

/**
 * Where the link target that starts at `from` ends: its `)`, past any
 * balanced pairs inside, as in `https://en.wikipedia.org/wiki/Fox_(film)`.
 */
private fun closingParen(s: String, from: Int): Int {
    var depth = 0
    for (i in from until s.length) {
        when (s[i]) {
            '(' -> depth++
            ')' -> if (depth == 0) return i else depth--
            // A target has no spaces or line breaks: this was no link.
            ' ', '\n' -> return -1
        }
    }
    return -1
}

private val WEB = Regex("https?://\\S+", RegexOption.IGNORE_CASE)

/** A piece of the agent's words: text to draw as [markdown], or a picture on a line of its own. */
sealed interface Block {
    data class Words(val text: String) : Block
    data class Pic(val url: String, val alt: String) : Block
}

/**
 * The agent's words with each `![alt](url)` taken out as a picture, as the
 * phone page draws them. The computer rewrote the files its agent named to
 * signed `/image?…` links; those and `https:` pictures show, anything else
 * stays as written. Code spans and fenced blocks are left alone.
 */
fun blocks(s: String): List<Block> {
    val out = mutableListOf<Block>()
    val words = StringBuilder()
    fun flush() {
        val t = words.toString().trim('\n')
        if (t.isNotBlank()) out += Block.Words(t)
        words.clear()
    }
    var i = 0
    while (i < s.length) {
        val ch = s[i]
        when {
            ch == '\\' && i + 1 < s.length -> {
                words.append(s, i, i + 2)
                i += 2
            }
            ch == '`' -> {
                // A run of backticks closes on a run as long: a fence, or a code span.
                var n = 1
                while (i + n < s.length && s[i + n] == '`') n++
                val marker = "`".repeat(n)
                val end = s.indexOf(marker, i + n)
                val upTo = if (end < 0) i + n else end + n
                words.append(s, i, upTo)
                i = upTo
            }
            s.startsWith("![", i) -> {
                val close = s.indexOf(']', i + 2)
                val alt = if (close > 0) s.substring(i + 2, close) else ""
                val end = if (close > 0 && !alt.contains('\n') && close + 1 < s.length && s[close + 1] == '(') {
                    closingParen(s, close + 2)
                } else {
                    -1
                }
                val url = if (end > 0) s.substring(close + 2, end) else ""
                if (end < 0 || !shown(url)) {
                    words.append(ch)
                    i++
                } else {
                    flush()
                    out += Block.Pic(url, alt)
                    i = end + 1
                }
            }
            else -> {
                words.append(ch)
                i++
            }
        }
    }
    flush()
    return out
}

/** Whether the app can show a picture from `url`: the computer's own links and the web's. */
private fun shown(url: String): Boolean =
    (url.startsWith("/") && !url.startsWith("//")) || url.startsWith("https://", ignoreCase = true)

/**
 * What a picture is called while it loads: its description, else the file
 * name a signed link carries in `p`, else the last part of its address.
 */
fun pictureName(url: String, alt: String): String {
    if (alt.isNotBlank()) return alt
    val query = url.substringAfter('?', "")
    val path = query.split('&').firstOrNull { it.startsWith("p=") }?.removePrefix("p=")
        ?.let { runCatching { java.net.URLDecoder.decode(it, "UTF-8") }.getOrNull() }
        ?: url.substringBefore('?').substringBefore('#')
    return path.trimEnd('/').substringAfterLast('/').ifEmpty { "Picture" }
}
