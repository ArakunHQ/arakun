import ArakunKit
import SwiftUI

/// The frame both asks share: amber, since they are what the session waits on.
private struct AskFrame<Content: View>: View {
    let icon: String
    let title: String
    var trailing: String?
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Icon(icon, size: 15)
                Eyebrow(title, color: .amberInk)
                Spacer()
                if let trailing { Text(trailing).font(.mono(11)).foregroundStyle(Color.textFaint) }
            }
            .foregroundStyle(Color.amberInk)
            content
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card(fill: .amberSoft, stroke: Color.amber.opacity(0.4))
    }
}

/// The agent waits to be allowed to do something.
struct PermissionCard: View {
    let ask: PermissionAsk
    let pick: (PermissionAsk.Option) -> Void

    var body: some View {
        AskFrame(icon: "hand", title: "Permission") {
            Text(ask.title)
                .font(.code)
                .lineLimit(6)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
            ViewThatFits {
                HStack(spacing: 8) { buttons }
                VStack(alignment: .leading, spacing: 8) { buttons }
            }
        }
    }

    /// The first allow is the one primary button; a reject is red text.
    @ViewBuilder private var buttons: some View {
        let primary = ask.options.first(where: { $0.kind.hasPrefix("allow") })?.id
        ForEach(ask.options) { o in
            if o.id == primary {
                Button(o.name) { pick(o) }.buttonStyle(.primaryCompact).fixedSize()
            } else {
                Button(o.name) { pick(o) }.buttonStyle(.outline(o.kind.hasPrefix("reject") ? .failure : .text)).fixedSize()
            }
        }
    }
}

/// The agent asks one or more questions; each takes options, words of your
/// own, or both.
struct QuestionCard: View {
    let ask: QuestionAsk
    let answer: ([String]) -> Void
    let skip: () -> Void
    @State private var page = 0
    @State private var picked: [Set<Int>] = []
    @State private var typed: [String] = []
    @State private var confirmSkip = false

    var body: some View {
        let q = ask.questions.isEmpty ? nil : ask.questions[min(page, ask.questions.count - 1)]
        AskFrame(icon: "help-circle", title: "Question", trailing: ask.questions.count > 1 ? "\(page + 1) of \(ask.questions.count)" : nil) {
            if let q, picked.count == ask.questions.count {
                if ask.questions.count > 1, !q.topic.isEmpty {
                    Text(q.topic).font(.small).foregroundStyle(Color.textMuted)
                }
                Text(q.question).font(.geist(16, .medium))
                if q.multi { Text("Pick any that apply.").font(.small).foregroundStyle(Color.textMuted) }
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(q.options.enumerated()), id: \.offset) { j, o in
                            option(o, on: picked[page].contains(j), multi: q.multi) { toggle(j, multi: q.multi) }
                        }
                    }
                }
                .frame(maxHeight: 200)
                .fixedSize(horizontal: false, vertical: true)
                TextField("", text: $typed[page], prompt: Text("Or answer in your own words").foregroundStyle(Color.textFaint))
                    .font(.callout)
                    .padding(.horizontal, 12)
                    .frame(minHeight: 40)
                    .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.raised))
                    .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.lineStrong))
                HStack(spacing: 8) {
                    Button("Skip") { confirmSkip = true }.buttonStyle(.outline)
                    if page > 0 { Button("Back") { page -= 1 }.buttonStyle(.outline) }
                    Spacer()
                    Button(last ? "Answer" : "Next question") {
                        if last { answer(answers) } else { page += 1 }
                    }
                    .buttonStyle(.primaryCompact)
                    .disabled(last ? answers.contains(where: \.isEmpty) : answerOf(page).isEmpty)
                }
            }
        }
        .onAppear {
            picked = ask.questions.map { _ in [] }
            typed = ask.questions.map { _ in "" }
        }
        .confirmationDialog("Skip all the questions?", isPresented: $confirmSkip, titleVisibility: .visible) {
            Button("Skip all", role: .destructive, action: skip)
        }
    }

    private func option(_ text: String, on: Bool, multi: Bool, tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            HStack(spacing: 10) {
                ZStack {
                    if multi {
                        RoundedRectangle(cornerRadius: Radius.chip).strokeBorder(on ? Color.amberInk : Color.lineStrong, lineWidth: 1.5)
                    } else {
                        Circle().strokeBorder(on ? Color.amberInk : Color.lineStrong, lineWidth: 1.5)
                    }
                    if on {
                        if multi { RoundedRectangle(cornerRadius: 2).fill(Color.amberInk).padding(4) }
                        else { Circle().fill(Color.amberInk).padding(4) }
                    }
                }
                .frame(width: 18, height: 18)
                Text(text).font(.callout).foregroundStyle(Color.text).multilineTextAlignment(.leading)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(RoundedRectangle(cornerRadius: Radius.control).fill(on ? Color.raised : Color.clear))
            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(on ? Color.amber.opacity(0.5) : Color.line))
        }
        .buttonStyle(.plain)
    }

    private var last: Bool { page >= ask.questions.count - 1 }
    private var answers: [String] { ask.questions.indices.map(answerOf) }

    private func answerOf(_ i: Int) -> String {
        guard i < picked.count else { return "" }
        let q = ask.questions[i]
        var parts = picked[i].sorted().map { q.options[$0] }
        let own = typed[i].trimmingCharacters(in: .whitespaces)
        if !own.isEmpty { parts.append(own) }
        return parts.joined(separator: ", ")
    }

    private func toggle(_ j: Int, multi: Bool) {
        if picked[page].contains(j) {
            picked[page].remove(j)
        } else if multi {
            picked[page].insert(j)
        } else {
            picked[page] = [j]
        }
    }
}
