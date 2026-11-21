import ArakunKit
import SwiftUI

/// One entry of the transcript.
struct EntryView: View {
    let entry: Entry

    var body: some View {
        switch entry {
        case .user(let text, let at, let elapsed, let pastes):
            HStack {
                Spacer(minLength: 48)
                VStack(alignment: .trailing, spacing: 6) {
                    Text(text)
                        .font(.body16)
                        .textSelection(.enabled)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .card()
                    ForEach(Array(pastes.enumerated()), id: \.offset) { _, p in
                        Text(p).font(.code).foregroundStyle(Color.textMuted).lineLimit(4)
                            .padding(10)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
                    }
                    HStack(spacing: 6) {
                        Text(at, style: .time)
                        if let elapsed {
                            Text("·")
                            Text(Duration.milliseconds(elapsed).formatted(.units(allowed: [.minutes, .seconds], width: .narrow)))
                        }
                    }
                    .font(.mono(11))
                    .foregroundStyle(Color.textFaint)
                }
            }
            .padding(.top, 6)
        case .agent(let text):
            Text(markdown(text))
                .font(.body16)
                .lineSpacing(3)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        case .thought(let text):
            Fold(icon: "ai-idea", title: "Thinking") {
                Text(text).font(.small).foregroundStyle(Color.textMuted).textSelection(.enabled)
            }
        case .tool(let t):
            ToolView(tool: t)
        case .plan(let items):
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    Icon("check-list", size: 14)
                    Eyebrow("Plan")
                    Spacer()
                    Text("\(items.filter { $0.status == "completed" }.count) of \(items.count)").font(.mono(11)).foregroundStyle(Color.textFaint)
                }
                .foregroundStyle(Color.textFaint)
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .top, spacing: 10) {
                        Icon(item.status == "completed" ? "checkmark-circle02" : item.status == "in_progress" ? "circle-dashed" : "circle", size: 16)
                            .foregroundStyle(item.status == "in_progress" ? Color.amberInk : item.status == "completed" ? Color.textMuted : Color.textFaint)
                            .padding(.top, 1)
                        Text(item.content)
                            .font(.callout)
                            .strikethrough(item.status == "completed", color: .textFaint)
                            .foregroundStyle(item.status == "completed" ? Color.textFaint : Color.text)
                    }
                }
            }
            .padding(14)
            .card()
        case .notice(let text, let error):
            HStack(alignment: .top, spacing: 8) {
                Icon(error ? "alert-circle" : "help-circle", size: 15).padding(.top, 1)
                Text(text).font(.small)
            }
            .foregroundStyle(error ? Color.failure : Color.textMuted)
        case .compacted(let before, let after):
            HStack(spacing: 10) {
                Rectangle().fill(Color.line).frame(height: 1)
                Text(compactedText(before, after)).font(.mono(11)).foregroundStyle(Color.textFaint).fixedSize()
                Rectangle().fill(Color.line).frame(height: 1)
            }
        case .unknown:
            EmptyView()
        }
    }

    private func compactedText(_ before: Int?, _ after: Int?) -> String {
        guard let before, let after else { return "Compacted" }
        return "Compacted \(before.formatted()) → \(after.formatted()) tokens"
    }

    private func markdown(_ s: String) -> AttributedString {
        var a = (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
        // Inline code in Geist Mono, on a faint well.
        for run in a.runs where run.inlinePresentationIntent?.contains(.code) == true {
            a[run.range].font = .mono(14.5)
            a[run.range].backgroundColor = Color.lineStrong.opacity(0.6)
        }
        for run in a.runs where run.inlinePresentationIntent?.contains(.stronglyEmphasized) == true {
            a[run.range].font = .geist(16, .semibold)
        }
        for run in a.runs where run.link != nil {
            a[run.range].foregroundColor = Color.amberInk
        }
        return a
    }
}

/// A row that opens to show more: thinking, a tool's output.
struct Fold<Content: View, Trailing: View>: View {
    let icon: String
    let title: String
    var mono = false
    @ViewBuilder var content: Content
    @ViewBuilder var trailing: Trailing
    @State private var open = false

    init(icon: String, title: String, mono: Bool = false, @ViewBuilder content: () -> Content,
         @ViewBuilder trailing: () -> Trailing = { EmptyView() }) {
        self.icon = icon
        self.title = title
        self.mono = mono
        self.content = content()
        self.trailing = trailing()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy(duration: 0.2)) { open.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Icon(icon, size: 15).foregroundStyle(Color.textFaint)
                    Text(title)
                        .font(mono ? .code : .small)
                        .foregroundStyle(Color.textMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 0)
                    trailing
                    Icon("arrow-right01", size: 13)
                        .foregroundStyle(Color.textFaint)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open {
                content
                    .padding(.leading, 25)
                    .transition(.opacity)
            }
        }
    }
}

/// A tool call, folded to its title; open it for the command and output.
struct ToolView: View {
    let tool: Entry.Tool

    var body: some View {
        Fold(icon: symbol, title: tool.title.isEmpty ? tool.kind : tool.title, mono: true) {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(tool.diffs.enumerated()), id: \.offset) { _, d in
                    HStack {
                        Text(d.path).font(.code).foregroundStyle(Color.textMuted).lineLimit(1).truncationMode(.head)
                        Spacer()
                        Changes(added: d.added, removed: d.removed)
                    }
                }
                if tool.command != nil || tool.output?.isEmpty == false {
                    VStack(alignment: .leading, spacing: 6) {
                        if let c = tool.command {
                            Text("$ " + c).font(.code).foregroundStyle(Color.text).textSelection(.enabled)
                        }
                        if let o = tool.output, !o.isEmpty {
                            ScrollView(.horizontal, showsIndicators: false) {
                                Text(o).font(.mono(11.5)).foregroundStyle(Color.textMuted).textSelection(.enabled).fixedSize()
                            }
                        }
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
                    .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.line))
                }
                if let code = tool.exitCode, code != 0 {
                    Text("Exit code \(code)").font(.mono(11.5)).foregroundStyle(Color.failure)
                }
            }
        } trailing: {
            HStack(spacing: 8) {
                Changes(added: tool.added, removed: tool.removed)
                state
            }
        }
    }

    @ViewBuilder private var state: some View {
        switch tool.status {
        case "in_progress", "pending":
            ProgressView().controlSize(.mini).tint(Color.attention)
        case "failed":
            HStack(spacing: 4) {
                Icon("mark-cross", size: 12)
                Text("Failed").font(.mono(11))
            }
            .foregroundStyle(Color.failure)
        default:
            EmptyView()
        }
    }

    private var symbol: String {
        switch tool.kind {
        case "execute": "computer-terminal01"
        case "edit": "pencil-edit01"
        case "read": "file01"
        case "search": "search01"
        case "fetch": "globe"
        case "delete": "delete02"
        case "think": "ai-idea"
        default: "wrench01"
        }
    }
}
