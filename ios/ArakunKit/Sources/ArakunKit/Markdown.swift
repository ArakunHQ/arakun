import Foundation

// The block structure of what an agent writes: headings, paragraphs, lists,
// quotes, rules, fenced code and tables. Inline spans (bold, code, links) stay
// in each block's text for the app to style. It follows phone/src/lib/markdown.tsx,
// so a reply reads the same on every phone.

/// One block of a markdown reply.
public enum MarkdownBlock: Equatable, Sendable {
    case heading(level: Int, text: String)
    case paragraph(String)
    case list([MarkdownListItem])
    case quote([MarkdownBlock])
    case rule
    /// A fenced block. `chart` and `mermaid` blocks come through as code too.
    case code(language: String, text: String)
    case table(MarkdownTable)

    /// Splits a reply into blocks.
    public static func parse(_ src: String) -> [MarkdownBlock] {
        var p = Parser(lines: src.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n"))
        return p.blocks()
    }
}

public struct MarkdownListItem: Equatable, Sendable {
    /// The item's number in an ordered list; nil for a bullet.
    public var number: Int?
    /// How far the item is nested, from 0.
    public var depth: Int
    /// A task list's box: true when ticked, nil when the item has none.
    public var checked: Bool?
    public var text: String

    public init(number: Int? = nil, depth: Int = 0, checked: Bool? = nil, text: String) {
        self.number = number
        self.depth = depth
        self.checked = checked
        self.text = text
    }
}

public struct MarkdownTable: Equatable, Sendable {
    public enum Alignment: Equatable, Sendable { case leading, center, trailing }

    public var header: [String]
    /// One per column.
    public var alignments: [Alignment]
    /// Each row has as many cells as the header.
    public var rows: [[String]]

    public init(header: [String], alignments: [Alignment], rows: [[String]]) {
        self.header = header
        self.alignments = alignments
        self.rows = rows
    }
}

private struct Parser {
    let lines: [String]
    var i = 0

    init(lines: [String]) { self.lines = lines }

    mutating func blocks() -> [MarkdownBlock] {
        var out: [MarkdownBlock] = []
        while i < lines.count {
            let line = lines[i]
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                i += 1
            } else if let fence = Self.fence(line) {
                out.append(code(fence))
            } else if let h = Self.heading(line) {
                out.append(h)
                i += 1
            } else if Self.isRule(line) {
                out.append(.rule)
                i += 1
            } else if Self.listMark(line) != nil {
                out.append(list())
            } else if Self.isQuote(line) {
                var q: [String] = []
                while i < lines.count, Self.isQuote(lines[i]) {
                    q.append(Self.unquote(lines[i]))
                    i += 1
                }
                var inner = Parser(lines: q)
                out.append(.quote(inner.blocks()))
            } else if let t = table() {
                out.append(.table(t))
            } else {
                var para: [String] = [line.trimmingCharacters(in: .whitespaces)]
                i += 1
                while i < lines.count, !startsBlock(i) {
                    para.append(lines[i].trimmingCharacters(in: .whitespaces))
                    i += 1
                }
                out.append(.paragraph(para.joined(separator: "\n")))
            }
        }
        return out
    }

    /// Whether line `at` ends a paragraph.
    func startsBlock(_ at: Int) -> Bool {
        let line = lines[at]
        return line.trimmingCharacters(in: .whitespaces).isEmpty || Self.fence(line) != nil || Self.heading(line) != nil
            || Self.isRule(line) || Self.listMark(line) != nil || Self.isQuote(line) || isTable(at)
    }

    // MARK: Code

    struct Fence { let marker: Character; let length: Int; let language: String }

    static func fence(_ line: String) -> Fence? {
        let s = line.drop { $0 == " " }
        guard line.count - s.count <= 3, let c = s.first, c == "`" || c == "~" else { return nil }
        let run = s.prefix { $0 == c }
        guard run.count >= 3 else { return nil }
        let info = s.dropFirst(run.count).trimmingCharacters(in: .whitespaces)
        // A backtick fence's info may not hold a backtick, or ```a``` inline
        // would open a block.
        if c == "`", info.contains("`") { return nil }
        let language = info.split(separator: " ").first.map(String.init) ?? ""
        return Fence(marker: c, length: run.count, language: language)
    }

    mutating func code(_ open: Fence) -> MarkdownBlock {
        var code: [String] = []
        i += 1
        while i < lines.count {
            let s = lines[i].trimmingCharacters(in: .whitespaces)
            if s.count >= open.length, s.allSatisfy({ $0 == open.marker }) { break }
            code.append(lines[i])
            i += 1
        }
        // Past the closing fence; a block still streaming in has none yet.
        i += 1
        return .code(language: open.language, text: code.joined(separator: "\n"))
    }

    // MARK: Headings, rules, quotes

    static func heading(_ line: String) -> MarkdownBlock? {
        let s = line.drop { $0 == " " }
        guard line.count - s.count <= 3 else { return nil }
        let hashes = s.prefix { $0 == "#" }
        guard (1...6).contains(hashes.count) else { return nil }
        let rest = s.dropFirst(hashes.count)
        guard rest.isEmpty || rest.first == " " || rest.first == "\t" else { return nil }
        var text = rest.trimmingCharacters(in: .whitespaces)
        // A closing run of #s is decoration.
        let trailing = text.reversed().prefix { $0 == "#" }.count
        if trailing > 0 {
            let kept = text.dropLast(trailing)
            if kept.isEmpty || kept.last == " " { text = kept.trimmingCharacters(in: .whitespaces) }
        }
        return .heading(level: hashes.count, text: text)
    }

    /// Three or more of the same -, * or _, spaces allowed between.
    static func isRule(_ line: String) -> Bool {
        let s = line.filter { $0 != " " && $0 != "\t" }
        guard s.count >= 3, let c = s.first, "-*_".contains(c) else { return false }
        return s.allSatisfy { $0 == c }
    }

    static func isQuote(_ line: String) -> Bool { line.drop { $0 == " " }.first == ">" }

    static func unquote(_ line: String) -> String {
        var s = line.drop { $0 == " " }.dropFirst()
        if s.first == " " { s = s.dropFirst() }
        return String(s)
    }

    // MARK: Lists

    struct Mark { let indent: Int; let number: Int?; let width: Int }

    /// A list item's marker: `- `, `* `, `+ `, `1. ` or `1) `.
    static func listMark(_ line: String) -> Mark? {
        let indent = line.prefix { $0 == " " || $0 == "\t" }.reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
        let s = line.drop { $0 == " " || $0 == "\t" }
        guard let c = s.first else { return nil }
        if "-*+".contains(c) {
            let after = s.dropFirst()
            guard after.first == " " || after.first == "\t" else { return nil }
            return Mark(indent: indent, number: nil, width: line.count - after.count + 1)
        }
        let digits = s.prefix { $0.isASCII && $0.isNumber }
        guard (1...9).contains(digits.count), let n = Int(digits) else { return nil }
        let after = s.dropFirst(digits.count)
        guard let p = after.first, p == "." || p == ")" else { return nil }
        let rest = after.dropFirst()
        guard rest.first == " " || rest.first == "\t" else { return nil }
        return Mark(indent: indent, number: n, width: line.count - rest.count + 1)
    }

    mutating func list() -> MarkdownBlock {
        var items: [MarkdownListItem] = []
        // The indents seen so far, one per depth.
        var indents: [Int] = []
        while i < lines.count {
            let line = lines[i]
            if let m = Self.listMark(line), !Self.isRule(line) {
                while let last = indents.last, m.indent < last { indents.removeLast() }
                if indents.isEmpty || m.indent > indents.last! + 1 {
                    indents.append(m.indent)
                }
                var text = String(line.dropFirst(min(m.width, line.count))).trimmingCharacters(in: .whitespaces)
                var checked: Bool?
                for (box, value) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] where text.hasPrefix(box) {
                    checked = value
                    text = String(text.dropFirst(box.count))
                }
                items.append(MarkdownListItem(number: m.number, depth: min(indents.count - 1, 4), checked: checked, text: text))
                i += 1
                continue
            }
            let blank = line.trimmingCharacters(in: .whitespaces).isEmpty
            let indented = line.hasPrefix("  ") || line.hasPrefix("\t")
            if !blank, indented, !items.isEmpty, Self.fence(line) == nil {
                // A wrapped line of the item above.
                items[items.count - 1].text += "\n" + line.trimmingCharacters(in: .whitespaces)
                i += 1
            } else if blank, i + 1 < lines.count, Self.listMark(lines[i + 1]) != nil, !Self.isRule(lines[i + 1]) {
                // A loose list: a blank line between items.
                i += 1
            } else {
                break
            }
        }
        return .list(items)
    }

    // MARK: Tables

    func isTable(_ at: Int) -> Bool {
        at + 1 < lines.count && lines[at].contains("|") && Self.alignments(lines[at + 1]) != nil
    }

    /// The separator under a table's header: `| --- | :-: | --: |`.
    static func alignments(_ line: String) -> [MarkdownTable.Alignment]? {
        guard line.contains("-") else { return nil }
        let raw = line.trimmingCharacters(in: .whitespaces)
        guard raw.contains("|") || raw.hasPrefix(":") || raw.hasPrefix("-") else { return nil }
        var out: [MarkdownTable.Alignment] = []
        for cell in cells(line) {
            let left = cell.hasPrefix(":"), right = cell.hasSuffix(":")
            let dashes = cell.drop { $0 == ":" }.reversed().drop { $0 == ":" }
            guard !dashes.isEmpty, dashes.allSatisfy({ $0 == "-" }) else { return nil }
            out.append(left && right ? .center : right ? .trailing : .leading)
        }
        // One dash run with no pipe is a rule, not a table.
        if out.count == 1, !raw.contains("|") { return nil }
        return out.isEmpty ? nil : out
    }

    /// A row's cells, without the outer pipes. `\|` is a pipe inside a cell.
    static func cells(_ row: String) -> [String] {
        var s = row.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("|") { s.removeFirst() }
        if s.hasSuffix("|"), !s.hasSuffix("\\|") { s.removeLast() }
        var out: [String] = []
        var cell = ""
        var escaped = false
        for c in s {
            if escaped {
                cell.append(c == "|" ? "|" : "\\\(c)")
                escaped = false
            } else if c == "\\" {
                escaped = true
            } else if c == "|" {
                out.append(cell.trimmingCharacters(in: .whitespaces))
                cell = ""
            } else {
                cell.append(c)
            }
        }
        if escaped { cell.append("\\") }
        out.append(cell.trimmingCharacters(in: .whitespaces))
        return out
    }

    mutating func table() -> MarkdownTable? {
        guard isTable(i), let aligns = Self.alignments(lines[i + 1]) else { return nil }
        let header = Self.cells(lines[i])
        guard header.count == aligns.count else { return nil }
        i += 2
        var rows: [[String]] = []
        while i < lines.count, lines[i].contains("|"), !lines[i].trimmingCharacters(in: .whitespaces).isEmpty {
            var row = Self.cells(lines[i])
            if row.count < header.count { row += Array(repeating: "", count: header.count - row.count) }
            rows.append(Array(row.prefix(header.count)))
            i += 1
        }
        return MarkdownTable(header: header, alignments: aligns, rows: rows)
    }
}
