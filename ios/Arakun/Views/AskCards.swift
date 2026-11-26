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
    /// The allow-always option waiting for its second yes.
    @State private var always: PermissionAsk.Option?

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
        .confirmationDialog("Allow this for the rest of the session?",
                            isPresented: Binding(get: { always != nil }, set: { if !$0 { always = nil } }),
                            titleVisibility: .visible, presenting: always) { o in
            Button(o.name) { pick(o) }
        } message: { _ in
            Text("The agent will not ask again before doing it.")
        }
    }

    /// As the phone page has it, a stray touch allows nothing: a one-time
    /// allow is held, an allow for the session asks a second time. A reject
    /// is red text.
    @ViewBuilder private var buttons: some View {
        ForEach(ask.options) { o in
            if o.kind == "allow_always" {
                Button(o.name) { always = o }.buttonStyle(.outline).fixedSize()
            } else if o.kind.hasPrefix("allow") {
                HoldButton(title: o.name) { pick(o) }.fixedSize()
            } else {
                Button(o.name) { pick(o) }.buttonStyle(.outline(o.kind.hasPrefix("reject") ? .failure : .text)).fixedSize()
            }
        }
    }
}

/// The primary button, answered only once the finger has stayed down while it fills.
private struct HoldButton: View {
    let title: String
    let action: () -> Void
    @State private var holding = false
    @Environment(\.isEnabled) private var enabled
    @Environment(\.accessibilityReduceMotion) private var still
    private static let hold: Double = 0.7

    var body: some View {
        Text("Hold to \(title.lowercased())")
            .font(.geist(15, .semibold))
            .foregroundStyle(Color.onAmber)
            .padding(.horizontal, 14)
            .frame(minHeight: 40)
            .background(Color.amber)
            .overlay(alignment: .leading) {
                GeometryReader { g in
                    Rectangle().fill(Color.onAmber.opacity(0.18)).frame(width: holding ? g.size.width : 0)
                }
                // Reduce Motion: the fill snaps full while held.
                .animation(still ? nil : holding ? Animation.linear(duration: Self.hold) : Animation.easeOut(duration: 0.15), value: holding)
            }
            .clipShape(RoundedRectangle(cornerRadius: Radius.control))
            .opacity(enabled ? 1 : 0.4)
            .onLongPressGesture(minimumDuration: Self.hold) {
                guard enabled else { return }
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                action()
            } onPressingChanged: { holding = $0 && enabled }
            // VoiceOver's double tap is deliberate already.
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(title)
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { if enabled { action() } }
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
            } else if ask.questions.isEmpty {
                // Nothing to answer, but the agent still waits on it.
                Text("The agent asked no question.").font(.small).foregroundStyle(Color.textMuted)
                Button("Skip", action: skip).buttonStyle(.outline)
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
        .accessibilityAddTraits(on ? .isSelected : [])
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

/// The agent asks for a secret (`request_secret`): masked fields it never sees,
/// with Save and Decline. What is typed goes to the computer, which keeps it and
/// hands the agent a handle, so it stays out of the transcript. Over a
/// connection the computer does not call private, only Decline is offered.
struct SecretCard: View {
    let ask: SecretAsk
    /// The values to save, or nil to decline. Whether it went out.
    let answer: ([String]?) -> Bool
    @State private var values: [String] = []
    @State private var shown: Set<Int> = []
    @FocusState private var focus: Int?
    @Environment(\.openURL) private var openURL

    static let notPrivate = "This connection is not encrypted, so the secret can't be sent over it. Decline, or answer from a device that reaches the computer through your account or Tailscale."

    var body: some View {
        AskFrame(icon: "alert-circle", title: "Secret", trailing: "Stays out of the transcript") {
            Text(ask.label).font(.geist(16, .medium))
            if !ask.reason.isEmpty {
                Text(ask.reason).font(.small).foregroundStyle(Color.textMuted)
            }
            if let url = ask.url {
                Button(url.absoluteString) { openURL(url) }
                    .font(.code)
                    .foregroundStyle(Color.amberInk)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .buttonStyle(.plain)
            }
            if ask.isPrivate {
                if values.count == ask.fields.count {
                    ForEach(Array(ask.fields.enumerated()), id: \.offset) { i, f in field(f, at: i) }
                }
            } else {
                HStack(alignment: .top, spacing: 8) {
                    Icon("alert-circle", size: 15).foregroundStyle(Color.failure)
                    Text(Self.notPrivate).font(.small).foregroundStyle(Color.text)
                }
                .accessibilityElement(children: .combine)
            }
            HStack(spacing: 8) {
                Button("Decline") { if answer(nil) { clear() } }.buttonStyle(.outline)
                Spacer()
                if ask.isPrivate {
                    Button("Save", action: save).buttonStyle(.primaryCompact).disabled(!ready)
                }
            }
        }
        .onAppear { clear() }
        // Leave nothing typed behind when the card goes.
        .onDisappear { clear() }
    }

    private func field(_ f: SecretAsk.Field, at i: Int) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(f.label).font(.small).foregroundStyle(Color.textMuted)
            HStack(spacing: 8) {
                Group {
                    if shown.contains(i) {
                        TextField("", text: $values[i])
                    } else {
                        SecureField("", text: $values[i])
                    }
                }
                .font(.code)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .textContentType(.password)
                .privacySensitive()
                .focused($focus, equals: i)
                .submitLabel(i == ask.fields.count - 1 ? .send : .next)
                .onSubmit { if i == ask.fields.count - 1 { save() } else { focus = i + 1 } }
                .accessibilityLabel(f.label)
                Button(shown.contains(i) ? "Hide" : "Show") {
                    if shown.contains(i) { shown.remove(i) } else { shown.insert(i) }
                }
                .font(.small)
                .foregroundStyle(Color.textMuted)
                .buttonStyle(.plain)
                .accessibilityLabel(shown.contains(i) ? "Hide \(f.label)" : "Show \(f.label)")
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 40)
            .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.raised))
            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.lineStrong))
        }
    }

    private var ready: Bool {
        ask.isPrivate && values.count == ask.fields.count && !values.contains(where: \.isEmpty)
    }

    private func save() {
        guard ready else { return }
        if answer(values) { clear() }
    }

    private func clear() {
        values = ask.fields.map { _ in "" }
        shown = []
    }
}
