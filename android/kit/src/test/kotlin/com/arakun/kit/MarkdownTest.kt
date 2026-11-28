package com.arakun.kit

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class MarkdownTest {
    @Test
    fun paragraphsKeepTheirLineBreaks() {
        assertEquals(
            listOf(Block.Paragraph("one\ntwo"), Block.Paragraph("three")),
            parseMarkdown("one\ntwo\n\n\nthree\n"),
        )
    }

    @Test
    fun headings() {
        assertEquals(
            listOf(Block.Heading(1, "Title"), Block.Heading(3, "Sub **bold**"), Block.Paragraph("#hashtag")),
            parseMarkdown("# Title #\n### Sub **bold**\n#hashtag"),
        )
    }

    @Test
    fun headingEndsAParagraph() {
        assertEquals(listOf(Block.Paragraph("text"), Block.Heading(2, "Next")), parseMarkdown("text\n## Next"))
    }

    @Test
    fun bulletsAndNumbers() {
        assertEquals(
            listOf(Block.Items(listOf(Item("a"), Item("b"), Item("c")))),
            parseMarkdown("- a\n* b\n+ c"),
        )
        assertEquals(
            listOf(Block.Items(listOf(Item("one", number = 3), Item("two", number = 4)))),
            parseMarkdown("3. one\n4) two"),
        )
    }

    @Test
    fun nestedItemsAndContinuations() {
        val md = """
            - top
              carried on
              - inner
                1. deepest
            - back
        """.trimIndent()
        assertEquals(
            listOf(
                Block.Items(
                    listOf(
                        Item("top carried on"),
                        Item("inner", depth = 1),
                        Item("deepest", depth = 2, number = 1),
                        Item("back"),
                    ),
                ),
            ),
            parseMarkdown(md),
        )
    }

    @Test
    fun aBlankLineBetweenItemsKeepsOneList() {
        assertEquals(
            listOf(Block.Items(listOf(Item("a", number = 1), Item("b", number = 2))), Block.Paragraph("after")),
            parseMarkdown("1. a\n\n2. b\n\nafter"),
        )
    }

    @Test
    fun tasks() {
        assertEquals(
            listOf(Block.Items(listOf(Item("done", checked = true), Item("todo", checked = false)))),
            parseMarkdown("- [x] done\n- [ ] todo"),
        )
    }

    @Test
    fun aWrappedYearStaysInItsParagraph() {
        assertEquals(listOf(Block.Paragraph("It started in\n2023. That year")), parseMarkdown("It started in\n2023. That year"))
    }

    @Test
    fun rules() {
        assertEquals(
            listOf(Block.Paragraph("a"), Block.Rule, Block.Rule, Block.Rule, Block.Paragraph("b")),
            parseMarkdown("a\n---\n* * *\n___\nb"),
        )
    }

    @Test
    fun quotesHoldBlocks() {
        assertEquals(
            listOf(Block.Quote(listOf(Block.Paragraph("said"), Block.Items(listOf(Item("x")))))),
            parseMarkdown("> said\n>\n> - x"),
        )
    }

    @Test
    fun fencedCode() {
        val md = "before\n```kotlin\nval a = 1\n\n  indented\n```\nafter"
        assertEquals(
            listOf(Block.Paragraph("before"), Block.Code("kotlin", "val a = 1\n\n  indented", closed = true), Block.Paragraph("after")),
            parseMarkdown(md),
        )
    }

    @Test
    fun aFenceInsideCodeNeedsItsOwnLength() {
        assertEquals(
            listOf(Block.Code("md", "```\ninner\n```", closed = true)),
            parseMarkdown("````md\n```\ninner\n```\n````"),
        )
        assertEquals(listOf(Block.Code("", "a", closed = true)), parseMarkdown("~~~\na\n~~~"))
    }

    @Test
    fun anIndentedFenceLosesItsIndent() {
        assertEquals(listOf(Block.Code("", "a\n b", closed = true)), parseMarkdown("  ```\n  a\n   b\n  ```"))
    }

    @Test
    fun aStreamingFenceIsOpen() {
        val code = parseMarkdown("```sh\nls -la").single() as Block.Code
        assertEquals("ls -la", code.text)
        assertFalse(code.closed)
    }

    @Test
    fun chartsAndDiagramsAreCode() {
        assertEquals(
            listOf(Block.Code("chart", "{\"type\": \"bar\"}", true), Block.Code("mermaid", "flowchart TD\n  a --> b", true)),
            parseMarkdown("```chart\n{\"type\": \"bar\"}\n```\n```mermaid\nflowchart TD\n  a --> b\n```"),
        )
    }

    @Test
    fun tables() {
        val md = """
            | Name | Size | Note |
            |:-----|-----:|:----:|
            | a    | 12   | `x|y` |
            | b \| c |
        """.trimIndent()
        assertEquals(
            listOf(
                Block.Table(
                    listOf("Name", "Size", "Note"),
                    listOf(Align.Start, Align.End, Align.Center),
                    listOf(listOf("a", "12", "`x|y`"), listOf("b | c", "", "")),
                ),
            ),
            parseMarkdown(md),
        )
    }

    @Test
    fun aTableWithoutOuterPipes() {
        assertEquals(
            listOf(Block.Paragraph("intro"), Block.Table(listOf("a", "b"), listOf(Align.Start, Align.Start), listOf(listOf("1", "2")))),
            parseMarkdown("intro\na | b\n--|--\n1 | 2"),
        )
    }

    @Test
    fun aPipeWithoutDashesIsText() {
        assertEquals(listOf(Block.Paragraph("a | b\nc")), parseMarkdown("a | b\nc"))
    }

    @Test
    fun figures() {
        assertTrue(isFigure("1,024"))
        assertTrue(isFigure("12 ms"))
        assertTrue(isFigure("$3.50"))
        assertFalse(isFigure("v1.2 beta"))
    }

    @Test
    fun windowsLineEnds() {
        assertEquals(listOf(Block.Heading(1, "a"), Block.Paragraph("b")), parseMarkdown("# a\r\nb\r\n"))
    }
}
