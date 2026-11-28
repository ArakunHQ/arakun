import ArakunKit
import SwiftUI
import UIKit

/// An agent's reply as block markdown: headings, lists, quotes, rules, code
/// and tables. ArakunKit finds the blocks; inline spans go through
/// `AttributedString`.
struct MarkdownView: View {
    let blocks: [MarkdownBlock]

    init(_ text: String) { blocks = MarkdownBlock.parse(text) }
    init(blocks: [MarkdownBlock]) { self.blocks = blocks }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                BlockView(block: block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Bold, italics, inline code and links in Geist, sized to the block around them.
enum InlineMarkdown {
    static func render(_ s: String, size: CGFloat = 16, bold: Bool = false) -> AttributedString {
        var a = (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
        for run in a.runs {
            let intent = run.inlinePresentationIntent ?? []
            if intent.contains(.code) {
                // Inline code in Geist Mono, on a faint well.
                a[run.range].font = .mono(size - 1.5)
                a[run.range].backgroundColor = Color.lineStrong.opacity(0.6)
            } else if intent.contains(.stronglyEmphasized), !bold {
                a[run.range].font = .geist(size, .semibold)
            }
            if run.link != nil {
                a[run.range].foregroundColor = Color.amberInk
            }
        }
        return a
    }
}

private struct BlockView: View {
    let block: MarkdownBlock
    @ScaledMetric(relativeTo: .body) private var indent: CGFloat = 18
    @ScaledMetric(relativeTo: .body) private var markerWidth: CGFloat = 14

    var body: some View {
        switch block {
        case .heading(let level, let text):
            let size = Self.headingSize(level)
            Text(InlineMarkdown.render(text, size: size, bold: true))
                .font(.geist(size, .semibold, relativeTo: level <= 2 ? .title3 : .headline))
                .textSelection(.enabled)
                .padding(.top, level <= 2 ? 6 : 2)
                .accessibilityAddTraits(.isHeader)
        case .paragraph(let text):
            prose(text)
        case .list(let items):
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .top, spacing: 8) {
                        marker(item).frame(minWidth: markerWidth, alignment: .trailing)
                        prose(item.text).frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.leading, CGFloat(item.depth) * indent)
                    .accessibilityElement(children: .combine)
                    .accessibilityValue(Self.state(item))
                }
            }
        case .quote(let inner):
            MarkdownView(blocks: inner)
                .foregroundStyle(Color.textMuted)
                .padding(.leading, 14)
                .overlay(alignment: .leading) {
                    Rectangle().fill(Color.lineStrong).frame(width: 3)
                }
        case .rule:
            Rectangle().fill(Color.line).frame(height: 1).padding(.vertical, 4).accessibilityHidden(true)
        case .code(let language, let text):
            CodeBlockView(language: language, text: text)
        case .table(let table):
            TableView(table: table)
        }
    }

    private func prose(_ text: String) -> some View {
        Text(InlineMarkdown.render(text))
            .font(.body16)
            .lineSpacing(3)
            .textSelection(.enabled)
    }

    @ViewBuilder private func marker(_ item: MarkdownListItem) -> some View {
        if let checked = item.checked {
            Icon(checked ? "checkmark-circle02" : "circle", size: 16)
                .foregroundStyle(checked ? Color.textMuted : Color.textFaint)
                .padding(.top, 3)
        } else if let n = item.number {
            Text("\(n).").font(.body16).monospacedDigit().foregroundStyle(Color.textFaint)
        } else {
            Text(item.depth == 0 ? "•" : "◦").font(.body16).foregroundStyle(Color.textFaint)
                .accessibilityHidden(true)
        }
    }

    /// A task's box for VoiceOver, which skips the icon.
    private static func state(_ item: MarkdownListItem) -> String {
        switch item.checked {
        case true?: "Done"
        case false?: "Not done"
        case nil: ""
        }
    }

    private static func headingSize(_ level: Int) -> CGFloat {
        switch level {
        case 1: 22
        case 2: 19
        case 3: 17
        default: 16
        }
    }
}

/// A fenced block: monospaced, scrolling sideways, with a copy button.
/// Charts and diagrams show as code until the app can draw them.
private struct CodeBlockView: View {
    let language: String
    let text: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Eyebrow(language.isEmpty ? "Code" : language)
                Spacer(minLength: 0)
                Button {
                    UIPasteboard.general.string = text
                    copied = true
                    Task { @MainActor in
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                } label: {
                    HStack(spacing: 4) {
                        Icon(copied ? "tick02" : "copy01", size: 13)
                        Text(copied ? "Copied" : "Copy").font(.mono(11.5))
                    }
                    .foregroundStyle(copied ? Color.amberInk : Color.textMuted)
                    .frame(minHeight: 32)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(copied ? "Copied" : "Copy code")
            }
            .padding(.leading, 10)
            .padding(.trailing, 8)
            Rectangle().fill(Color.line).frame(height: 1)
            ScrollView(.horizontal) {
                Text(text)
                    .font(.code)
                    .foregroundStyle(Color.text)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(10)
            }
            .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        }
        .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
        .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.line))
        .clipShape(RoundedRectangle(cornerRadius: Radius.control))
    }
}

/// A table that scrolls sideways when it is wider than the screen.
private struct TableView: View {
    let table: MarkdownTable
    @ScaledMetric(relativeTo: .footnote) private var wide: CGFloat = 240

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(Array(table.header.enumerated()), id: \.offset) { j, c in
                        cell(c, column: j, head: true)
                    }
                }
                Rectangle().fill(Color.lineStrong).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                ForEach(Array(table.rows.enumerated()), id: \.offset) { r, row in
                    if r > 0 {
                        Rectangle().fill(Color.line).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                    }
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { j, c in
                            cell(c, column: j, head: false)
                        }
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.line))
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
    }

    private func cell(_ s: String, column: Int, head: Bool) -> some View {
        let align = column < table.alignments.count ? table.alignments[column] : .leading
        // Short cells stay on one line; long ones get room to wrap into a few.
        let long = s.count > 28
        return Text(InlineMarkdown.render(s, size: 14, bold: head))
            .font(.geist(14, head ? .semibold : .regular, relativeTo: .footnote))
            .foregroundStyle(head ? Color.text : Color.textMuted)
            .multilineTextAlignment(align == .trailing ? .trailing : align == .center ? .center : .leading)
            .fixedSize(horizontal: !long, vertical: true)
            .frame(width: long ? wide : nil, alignment: Self.frame(align))
            .textSelection(.enabled)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .gridColumnAlignment(Self.frame(align).horizontal)
            .accessibilityAddTraits(head ? .isHeader : [])
    }

    private static func frame(_ a: MarkdownTable.Alignment) -> Alignment {
        switch a {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }
}
