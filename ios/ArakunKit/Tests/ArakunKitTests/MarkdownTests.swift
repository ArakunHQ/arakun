import Testing
@testable import ArakunKit

struct MarkdownTests {
    @Test func paragraphsKeepTheirLineBreaksAndInlineMarks() {
        #expect(MarkdownBlock.parse("Hello **there**\n`code` and [a](https://x.y)\n\nNext") == [
            .paragraph("Hello **there**\n`code` and [a](https://x.y)"),
            .paragraph("Next"),
        ])
    }

    @Test func readsHeadingsOfEachLevel() {
        #expect(MarkdownBlock.parse("# One\n## Two ##\n###### Six\n####### Seven\n#nospace") == [
            .heading(level: 1, text: "One"),
            .heading(level: 2, text: "Two"),
            .heading(level: 6, text: "Six"),
            .paragraph("####### Seven\n#nospace"),
        ])
    }

    @Test func aHeadingEndsAParagraph() {
        #expect(MarkdownBlock.parse("Intro\n## Steps\nDo it") == [
            .paragraph("Intro"), .heading(level: 2, text: "Steps"), .paragraph("Do it"),
        ])
    }

    @Test func readsBulletAndNumberedListsWithNesting() {
        let md = """
        - one
        - two
          wrapped
          - inner
            1. deep
        - three
        """
        #expect(MarkdownBlock.parse(md) == [.list([
            MarkdownListItem(text: "one"),
            MarkdownListItem(text: "two\nwrapped"),
            MarkdownListItem(depth: 1, text: "inner"),
            MarkdownListItem(number: 1, depth: 2, text: "deep"),
            MarkdownListItem(text: "three"),
        ])])
    }

    @Test func keepsTheNumbersOfAnOrderedListAndJoinsALooseOne() {
        #expect(MarkdownBlock.parse("3. c\n\n4) d\n\nAfter") == [
            .list([MarkdownListItem(number: 3, text: "c"), MarkdownListItem(number: 4, text: "d")]),
            .paragraph("After"),
        ])
    }

    @Test func readsTaskListBoxes() {
        #expect(MarkdownBlock.parse("- [ ] todo\n- [x] done\n- plain") == [.list([
            MarkdownListItem(checked: false, text: "todo"),
            MarkdownListItem(checked: true, text: "done"),
            MarkdownListItem(text: "plain"),
        ])])
    }

    @Test func readsRulesButNotEmphasis() {
        #expect(MarkdownBlock.parse("a\n\n---\n* * *\n___\n**b**") == [
            .paragraph("a"), .rule, .rule, .rule, .paragraph("**b**"),
        ])
    }

    @Test func readsQuotesWithBlocksInside() {
        #expect(MarkdownBlock.parse("> Note\n> - a\n>\n> more\nafter") == [
            .quote([.paragraph("Note"), .list([MarkdownListItem(text: "a")]), .paragraph("more")]),
            .paragraph("after"),
        ])
    }

    @Test func readsFencedCodeAsWritten() {
        let md = """
        Run:
        ```sh
        cargo test
          # **not bold**
        ```
        ~~~
        a
        ```
        ~~~
        """
        #expect(MarkdownBlock.parse(md) == [
            .paragraph("Run:"),
            .code(language: "sh", text: "cargo test\n  # **not bold**"),
            .code(language: "", text: "a\n```"),
        ])
    }

    @Test func aFenceNeedsAsManyMarksToClose() {
        #expect(MarkdownBlock.parse("````md\n```js\nx\n```\n````") == [.code(language: "md", text: "```js\nx\n```")])
    }

    @Test func anUnclosedFenceRunsToTheEnd() {
        #expect(MarkdownBlock.parse("```swift\nlet a = 1\n") == [.code(language: "swift", text: "let a = 1\n")])
    }

    @Test func chartAndMermaidBlocksStayCode() {
        #expect(MarkdownBlock.parse("```mermaid\nflowchart TD\n```\n```chart\na: 1\n```") == [
            .code(language: "mermaid", text: "flowchart TD"),
            .code(language: "chart", text: "a: 1"),
        ])
    }

    @Test func readsTablesWithAlignmentAndRaggedRows() {
        let md = """
        Results:
        | Name | Time | Note |
        |:-----|-----:|:----:|
        | a \\| b | 12 ms | `x` |
        | c | 3 |
        | d | 4 | e | extra |

        After
        """
        #expect(MarkdownBlock.parse(md) == [
            .paragraph("Results:"),
            .table(MarkdownTable(
                header: ["Name", "Time", "Note"],
                alignments: [.leading, .trailing, .center],
                rows: [["a | b", "12 ms", "`x`"], ["c", "3", ""], ["d", "4", "e"]]
            )),
            .paragraph("After"),
        ])
    }

    @Test func readsATableWithoutOuterPipes() {
        #expect(MarkdownBlock.parse("a | b\n--- | ---\n1 | 2") == [
            .table(MarkdownTable(header: ["a", "b"], alignments: [.leading, .leading], rows: [["1", "2"]])),
        ])
    }

    @Test func aPipeWithoutASeparatorIsText() {
        #expect(MarkdownBlock.parse("a | b\nc | d") == [.paragraph("a | b\nc | d")])
    }

    @Test func readsWindowsLineEndingsAndEmptyInput() {
        #expect(MarkdownBlock.parse("# A\r\nb\r\n") == [.heading(level: 1, text: "A"), .paragraph("b")])
        #expect(MarkdownBlock.parse("").isEmpty)
        #expect(MarkdownBlock.parse("\n  \n").isEmpty)
    }
}
