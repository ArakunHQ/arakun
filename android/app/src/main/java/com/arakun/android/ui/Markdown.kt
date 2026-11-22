package com.arakun.android.ui

import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.sp

/**
 * The agent's words with inline Markdown only, as the iOS app reads them:
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
    fun closes(marker: String, from: Int): Int {
        val end = s.indexOf(marker, from)
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
                val end = closes("`", i + 1)
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
                val end = closes(ch.toString(), i + 1)
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
                val url = if (close > 0 && close + 1 < s.length && s[close + 1] == '(') s.indexOf(')', close + 2) else -1
                if (url < 0) { plain.append(ch); i++ } else {
                    flush()
                    withLink(LinkAnnotation.Url(s.substring(close + 2, url), link)) { append(markdown(s.substring(i + 1, close), c)) }
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
