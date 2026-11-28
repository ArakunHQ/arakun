import Testing
@testable import ArakunKit

struct PictureTests {
    @Test func cutsTheAgentsWordsAtTheComputersPictures() {
        let md = "Here is the screen:\n\n![the bar](/image?s=ab&p=%2Ftmp%2Fbar.png)\n\nAnd after it, `code`."
        #expect(MarkdownPiece.split(md) == [
            .text("Here is the screen:"),
            .picture(Picture(name: "the bar", url: "/image?s=ab&p=%2Ftmp%2Fbar.png")),
            .text("And after it, `code`."),
        ])
    }

    @Test func wordsWithoutPicturesStayWhole() {
        #expect(MarkdownPiece.split("  just words\n") == [.text("just words")])
        #expect(MarkdownPiece.split("") == [])
    }

    @Test func leavesImagesTheComputerDoesNotServeInTheWords() {
        let md = "a ![x](https://example.com/x.png) b ![y](notes.png) c"
        #expect(MarkdownPiece.split(md) == [.text(md)])
        // Unclosed, or an alt over lines: no image.
        #expect(MarkdownPiece.split("a ![x](/image?s=1") == [.text("a ![x](/image?s=1")])
        #expect(MarkdownPiece.split("![x\ny](/image?s=1&p=a)") == [.text("![x\ny](/image?s=1&p=a)")])
    }

    @Test func picturesSideBySideEachStandAlone() {
        let md = "![](/image?s=1&p=%2Fa%2Fone.png)![two](/image?s=2&p=b.png) end"
        #expect(MarkdownPiece.split(md) == [
            .picture(Picture(name: "one.png", url: "/image?s=1&p=%2Fa%2Fone.png")),
            .picture(Picture(name: "two", url: "/image?s=2&p=b.png")),
            .text("end"),
        ])
    }

    @Test func aPictureWithNoNameIsCalledImage() {
        #expect(MarkdownPiece.name(alt: " ", url: "/image?s=1") == "Image")
    }
}
