import Foundation

/// A picture in a transcript: the name to show while it loads, and the link
/// the computer serves it at.
public struct Picture: Equatable, Hashable, Sendable {
    public let name: String
    /// A signed `/image?s=…&p=…` link. The signature is a secret: never log it.
    public let url: String

    public init(name: String, url: String) {
        self.name = name
        self.url = url
    }

    /// Whether the computer serves it, so the app fetches it through the relay.
    /// Only those: the app loads nothing from links the agent wrote itself.
    public var served: Bool { url.hasPrefix("/image?") }
}

/// A piece of the agent's markdown: words, or one of the computer's pictures.
public enum MarkdownPiece: Equatable, Sendable {
    case text(String)
    case picture(Picture)

    /// Cuts `markdown` at the images the computer serves, so each shows on its
    /// own at the width of the screen, between the words around it. Other
    /// images stay in the words, as written.
    public static func split(_ markdown: String) -> [MarkdownPiece] {
        var out: [MarkdownPiece] = []
        var words = ""
        var rest = Substring(markdown)
        while let start = rest.range(of: "![") {
            let after = rest[start.upperBound...]
            guard let close = after.range(of: "]("),
                  let end = after[close.upperBound...].firstIndex(of: ")")
            else { break }
            let alt = after[..<close.lowerBound]
            let picture = Picture(name: name(alt: String(alt), url: String(after[close.upperBound..<end])),
                                  url: String(after[close.upperBound..<end]).trimmingCharacters(in: .whitespaces))
            // An alt text over lines is no image: markdown would not read it as one.
            guard picture.served, !alt.contains("\n") else {
                words += rest[..<start.upperBound]
                rest = after
                continue
            }
            words += rest[..<start.lowerBound]
            push(&out, words)
            words = ""
            out.append(.picture(picture))
            rest = after[after.index(after: end)...]
        }
        words += rest
        push(&out, words)
        return out
    }

    private static func push(_ out: inout [MarkdownPiece], _ words: String) {
        // The picture stands on its own row: the line breaks around it go.
        let trimmed = words.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { out.append(.text(trimmed)) }
    }

    /// The alt text, or else the file's name from the link's `p`.
    static func name(alt: String, url: String) -> String {
        let alt = alt.trimmingCharacters(in: .whitespaces)
        if !alt.isEmpty { return alt }
        let path = URLComponents(string: url.trimmingCharacters(in: .whitespaces))?
            .queryItems?.first { $0.name == "p" }?.value ?? ""
        let file = path.split(separator: "/").last.map(String.init) ?? ""
        return file.isEmpty ? "Image" : file
    }
}
