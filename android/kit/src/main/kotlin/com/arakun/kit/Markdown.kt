package com.arakun.kit

/**
 * The blocks of an agent's reply: paragraphs, headings, lists, quotes, rules,
 * fenced code and tables. It follows blocks() of phone/src/lib/markdown.tsx,
 * which is enough for what agents write. Text inside a block stays as written,
 * for the app to read its inline spans (code, strong, emphasis, links).
 */
sealed interface Block {
    /** Lines of text; the line breaks stay. */
    data class Paragraph(val text: String) : Block

    /** `#` to `######`. */
    data class Heading(val level: Int, val text: String) : Block

    /** The items of one list, nested ones among them with a greater depth. */
    data class Items(val items: List<Item>) : Block

    /** `>` lines, which hold blocks of their own. */
    data class Quote(val blocks: List<Block>) : Block

    /** `---`, `***` or `___`. */
    data object Rule : Block

    /**
     * A fenced block. [lang] is the first word after the fence, so a
     * `chart` or `mermaid` block is code with that name. [closed] is false
     * while the reply is still streaming the block in.
     */
    data class Code(val lang: String, val text: String, val closed: Boolean) : Block

    /** A table: its head, how each column lines up, and its rows, each as wide as the head. */
    data class Table(val head: List<String>, val align: List<Align>, val rows: List<List<String>>) : Block
}

/**
 * One list item. [number] is its number in an ordered list and null for a
 * bullet; [checked] is set for a task, `- [ ]` or `- [x]`. [depth] counts
 * from 0 for the outermost list.
 */
data class Item(val text: String, val depth: Int = 0, val number: Int? = null, val checked: Boolean? = null)

enum class Align { Start, Center, End }

/** Reads [src] into blocks. */
fun parseMarkdown(src: String): List<Block> = Parser(src.replace("\r\n", "\n").split('\n')).blocks()

private val FENCE = Regex("""^(\s*)(`{3,}|~{3,})\s*([^\s`]*).*$""")
private val HEADING = Regex("""^\s{0,3}(#{1,6})(?:\s+(.*?))?(?:\s+#+)?\s*$""")
private val RULE = Regex("""^\s{0,3}([-*_])(?:\s*\1){2,}\s*$""")
private val MARKER = Regex("""^(\s*)([-*+]|(\d{1,9})[.)])(?:\s+(.*))?$""")
private val TASK = Regex("""^\[([ xX])]\s+(.*)$""")
private val QUOTE = Regex("""^\s{0,3}>\s?(.*)$""")
private val DELIMITER = Regex("""^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$""")

private class Parser(val lines: List<String>) {
    var i = 0

    fun blocks(): List<Block> {
        val out = mutableListOf<Block>()
        while (i < lines.size) {
            val line = lines[i]
            when {
                line.isBlank() -> i++
                FENCE.matches(line) -> out += code()
                HEADING.matches(line) -> {
                    val m = HEADING.matchEntire(line)!!
                    out += Block.Heading(m.groupValues[1].length, m.groupValues[2].trim())
                    i++
                }
                // Before lists, as `* * *` is a rule and not a bullet.
                RULE.matches(line) -> {
                    out += Block.Rule
                    i++
                }
                item(line) != null -> out += list()
                QUOTE.matches(line) -> out += quote()
                table(i) -> out += table()
                else -> out += paragraph()
            }
        }
        return out
    }

    private fun code(): Block.Code {
        val m = FENCE.matchEntire(lines[i])!!
        val indent = m.groupValues[1].length
        val fence = m.groupValues[2]
        val body = mutableListOf<String>()
        i++
        // A closing fence is the same character, at least as long.
        val close = Regex("""^\s*${fence[0]}{${fence.length},}\s*$""")
        while (i < lines.size && !close.matches(lines[i])) {
            // Lines lose up to the opening fence's indent, as in CommonMark.
            val l = lines[i]
            val cut = l.takeWhile { it == ' ' }.length.coerceAtMost(indent)
            body += l.substring(cut)
            i++
        }
        val closed = i < lines.size
        if (closed) i++
        return Block.Code(m.groupValues[3], body.joinToString("\n"), closed)
    }

    private fun list(): Block.Items {
        val items = mutableListOf<Item>()
        // The indent of each open level, so a nested list's depth follows
        // how far it is indented rather than by how many spaces.
        val levels = mutableListOf<Int>()
        while (i < lines.size) {
            val line = lines[i]
            if (line.isBlank()) {
                // A blank line between items keeps the list going.
                val next = lines.getOrNull(i + 1)
                if (next != null && item(next) != null) {
                    i++
                    continue
                }
                break
            }
            val it = item(line)
            if (it == null) {
                // A line indented under an item carries it on.
                if (items.isNotEmpty() && line.startsWith("  ") && !FENCE.matches(line)) {
                    val last = items.removeAt(items.lastIndex)
                    items += last.copy(text = last.text + " " + line.trim())
                    i++
                    continue
                }
                break
            }
            val (indent, entry) = it
            while (levels.isNotEmpty() && indent < levels.last()) levels.removeAt(levels.lastIndex)
            if (levels.isEmpty() || indent > levels.last() + 1) levels += indent
            items += entry.copy(depth = levels.size - 1)
            i++
        }
        return Block.Items(items)
    }

    /** The indent and item a list line starts, or null when it starts none. */
    private fun item(line: String): Pair<Int, Item>? {
        if (RULE.matches(line)) return null
        val m = MARKER.matchEntire(line) ?: return null
        val indent = m.groupValues[1].replace("\t", "    ").length
        val number = m.groupValues[3].takeIf { it.isNotEmpty() }?.toIntOrNull()
        var text = m.groupValues[4]
        // A bare number with nothing after it is a line of text, as "2." is.
        if (text.isEmpty() && number != null) return null
        var checked: Boolean? = null
        TASK.matchEntire(text)?.let {
            checked = it.groupValues[1] != " "
            text = it.groupValues[2]
        }
        return indent to Item(text.trim(), number = number, checked = checked)
    }

    private fun quote(): Block.Quote {
        val inner = mutableListOf<String>()
        while (i < lines.size) {
            val m = QUOTE.matchEntire(lines[i]) ?: break
            inner += m.groupValues[1]
            i++
        }
        return Block.Quote(Parser(inner).blocks())
    }

    /** Whether a table starts at line [at]: a row of cells over a row of dashes. */
    private fun table(at: Int): Boolean {
        val head = lines[at]
        val next = lines.getOrNull(at + 1) ?: return false
        return '|' in head && DELIMITER.matches(next) && ('|' in next || head.trim().startsWith('|')) &&
            cells(head).let { it.isNotEmpty() && it.size == cells(next).size }
    }

    private fun table(): Block.Table {
        val head = cells(lines[i])
        val align = cells(lines[i + 1]).map {
            when {
                it.startsWith(':') && it.endsWith(':') -> Align.Center
                it.endsWith(':') -> Align.End
                else -> Align.Start
            }
        }
        i += 2
        val rows = mutableListOf<List<String>>()
        while (i < lines.size && lines[i].isNotBlank() && '|' in lines[i]) {
            val row = cells(lines[i])
            rows += List(head.size) { row.getOrElse(it) { "" } }
            i++
        }
        return Block.Table(head, align, rows)
    }

    private fun paragraph(): Block.Paragraph {
        val para = mutableListOf(lines[i++])
        while (i < lines.size) {
            val l = lines[i]
            if (l.isBlank() || FENCE.matches(l) || HEADING.matches(l) || RULE.matches(l) || QUOTE.matches(l) || table(i)) break
            // An ordered list only breaks in when it starts at 1, so a
            // sentence that wraps onto "2023. That year…" stays a sentence.
            val it = item(l)?.second
            if (it != null && (it.number == null || it.number == 1)) break
            para += l
            i++
        }
        return Block.Paragraph(para.joinToString("\n"))
    }
}

/**
 * The cells of a table row, without the outer pipes. A `|` escaped with a
 * backslash or inside `code` is part of its cell.
 */
internal fun cells(row: String): List<String> {
    val s = row.trim().removePrefix("|")
    val out = mutableListOf<String>()
    val cell = StringBuilder()
    var code = false
    var j = 0
    while (j < s.length) {
        val ch = s[j]
        when {
            ch == '\\' && j + 1 < s.length && s[j + 1] == '|' -> {
                cell.append('|')
                j++
            }
            ch == '`' -> {
                code = !code
                cell.append(ch)
            }
            ch == '|' && !code -> {
                out += cell.toString().trim()
                cell.clear()
            }
            else -> cell.append(ch)
        }
        j++
    }
    // What follows the last pipe is a cell only when it holds something.
    if (cell.isNotBlank()) out += cell.toString().trim()
    return out
}

/** Whether a cell is a figure, which lines up on the right as in a spreadsheet. */
fun isFigure(cell: String): Boolean = FIGURE.matches(cell)

private val FIGURE = Regex("""^[-+−]?[$€£]?\s?[\d.,]+\s?(%|ms|s|x|k|M|GB|MB|KB)?$""")
