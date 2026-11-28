import ArakunKit
import SwiftUI

/// Every session on the computer, the ones waiting for you first.
struct InboxView: View {
    @Environment(AppModel.self) private var model
    @State private var path: [String] = []
    @State private var starting = false
    @State private var query = ""
    @State private var settings = false
    @State private var limits = false

    var body: some View {
        NavigationStack(path: $path) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 28) {
                    header
                    ConnectionBanner()
                    NoticeBanners()
                    let waiting = shown.filter { $0.status == .needsInput }
                    if !waiting.isEmpty {
                        group("Needs you", color: .amberInk, waiting, highlight: true)
                    }
                    ForEach(projects(shown.filter { $0.status != .needsInput }), id: \.0) { project, sessions in
                        group(project.isEmpty ? "Sessions" : project, color: .textFaint, sessions, highlight: false)
                    }
                    if model.isLinked && model.inbox.isEmpty {
                        Empty(mood: mood)
                    } else if !model.inbox.isEmpty && shown.isEmpty {
                        Text("No sessions match “\(query.trimmingCharacters(in: .whitespaces))”.")
                            .font(.callout)
                            .foregroundStyle(Color.textMuted)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 48)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 32)
            }
            .background(Color.ground.ignoresSafeArea())
            .searchable(text: $query, prompt: "Search sessions")
            .refreshable { model.refresh() }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .navigationDestination(for: String.self) { id in
                SessionView(id: id)
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        if let me = model.me { Text("Signed in as \(me.login)") }
                        Button("Reconnect", systemImage: "arrow.clockwise") { model.reconnectNow() }
                        if model.machines.count > 1 {
                            Button("All computers", systemImage: "desktopcomputer") { model.leaveMachine() }
                        }
                        Button("Plan limits", systemImage: "gauge.with.dots.needle.50percent") { limits = true }
                        Button("Settings", systemImage: "gearshape") { settings = true }
                    } label: {
                        Icon("more-horizontal", size: 22).foregroundStyle(Color.textMuted)
                    }
                    .accessibilityLabel("More")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { starting = true } label: {
                        Icon("pencil-edit02", size: 22)
                    }
                    .foregroundStyle(Color.text)
                    .disabled(!model.isLinked || model.places.isEmpty)
                    .accessibilityLabel("New session")
                }
            }
            .sheet(isPresented: $starting) {
                NewSessionView { id in
                    starting = false
                    path = [id]
                }
            }
            #if DEBUG
            .onAppear {
                if AppModel.isDemo, let id = UserDefaults.standard.string(forKey: "demoOpen") { path = [id] }
                if AppModel.isDemo, UserDefaults.standard.bool(forKey: "demoNew") { starting = true }
                if AppModel.isDemo, UserDefaults.standard.bool(forKey: "demoLimits") { limits = true }
            }
            #endif
            .onAppear {
                // A notification tap picked this computer and its session together.
                if let id = model.session?.id, path.isEmpty { path = [id] }
            }
            .onChange(of: model.session?.id) { _, id in
                // A session started from the sheet opens on its own.
                if let id, path.last != id { path = [id] }
                // Another computer now: the session on screen was the old one's.
                if id == nil, !path.isEmpty { path = [] }
            }
            .sheet(isPresented: $settings) { AccountView() }
            .sheet(isPresented: $limits) { PlanLimitsView() }
            .alert("Arakun", isPresented: Binding(get: { model.notice != nil }, set: { if !$0 { model.notice = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(model.notice ?? "")
            }
        }
    }

    /// The raccoon's mood for the whole computer: what most needs you wins.
    private var mood: Mood {
        if case .computerAway = model.connection { return .sleeping }
        if model.connection == .connecting { return .thinking }
        if model.inbox.contains(where: { $0.status == .needsInput }) { return .waiting }
        if model.inbox.contains(where: { $0.status == .working }) { return .working }
        return .idle
    }

    private var header: some View {
        HStack(alignment: .bottom) {
            headerText
            Spacer()
            // The empty inbox shows a big one instead: one raccoon per screen.
            if !model.inbox.isEmpty { Mascot(mood: mood, width: 64) }
        }
    }

    private var headerText: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                LinkDot(connection: model.connection)
                Eyebrow(model.host.isEmpty ? "Arakun" : model.host)
            }
            Text("Sessions")
                .font(.geist(34, .semibold, relativeTo: .largeTitle))
                .tracking(-1.0)
            if !summary.isEmpty {
                Text(summary).font(.small).foregroundStyle(Color.textMuted)
            }
        }
        .padding(.top, 4)
    }

    private var summary: String {
        let needs = model.inbox.filter { $0.status == .needsInput }.count
        let working = model.inbox.filter { $0.status == .working }.count
        var parts: [String] = []
        if needs > 0 { parts.append("\(needs) need\(needs == 1 ? "s" : "") you") }
        if working > 0 { parts.append("\(working) working") }
        parts.append("\(model.inbox.count) in all")
        return model.inbox.isEmpty ? "" : parts.joined(separator: " · ")
    }

    private var shown: [InboxSession] {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return model.inbox }
        return model.inbox.filter { "\($0.title) \($0.project) \($0.env)".lowercased().contains(q) }
    }

    /// Sessions by project, in the order their newest one was touched.
    private func projects(_ list: [InboxSession]) -> [(String, [InboxSession])] {
        var order: [String] = []
        var by: [String: [InboxSession]] = [:]
        for s in list {
            if by[s.project] == nil { order.append(s.project) }
            by[s.project, default: []].append(s)
        }
        return order.map { ($0, by[$0]!) }
    }

    private func group(_ title: String, color: Color, _ sessions: [InboxSession], highlight: Bool) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Eyebrow(title, color: color)
                Spacer()
                Text("\(sessions.count)").font(.mono(11)).foregroundStyle(Color.textFaint)
            }
            .padding(.horizontal, 4)
            VStack(spacing: 0) {
                ForEach(Array(sessions.enumerated()), id: \.element.id) { i, s in
                    if i > 0 { Rectangle().fill(highlight ? Color.amber.opacity(0.2) : Color.line).frame(height: 1).padding(.leading, 16) }
                    NavigationLink(value: s.id) {
                        SessionRow(session: s, muted: model.isMuted(s.id))
                    }
                    .buttonStyle(RowStyle())
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: Radius.card))
            .card(fill: highlight ? .amberSoft : .raised, stroke: highlight ? Color.amber.opacity(0.35) : .line)
        }
    }
}

/// A row that dims while pressed, like a list cell.
private struct RowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .contentShape(Rectangle())
            .background(configuration.isPressed ? Color.lineStrong.opacity(0.6) : .clear)
    }
}

struct SessionRow: View {
    let session: InboxSession
    /// The computer sends no notices about it.
    var muted = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(session.title.isEmpty ? "New session" : session.title)
                    .font(.geist(16, .medium))
                    .foregroundStyle(Color.text)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                Spacer(minLength: 8)
                if muted {
                    Image(systemName: "bell.slash")
                        .resizable()
                        .scaledToFit()
                        .frame(width: 12, height: 12)
                        .foregroundStyle(Color.textFaint)
                        .accessibilityLabel("Muted")
                }
                // Redrawn each half minute, so an idle row's age keeps up.
                TimelineView(.periodic(from: .now, by: 30)) { _ in
                    Text(session.updated.shortAgo)
                        .font(.mono(11.5))
                        .foregroundStyle(Color.textFaint)
                }
            }
            if let asks = session.asks {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Icon("hand", size: 13).alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4 }
                    Text(asks).font(.code).lineLimit(2)
                }
                .foregroundStyle(Color.amberInk)
            }
            HStack(spacing: 10) {
                StatusChip(status: session.status)
                Text(session.agent).font(.small).foregroundStyle(Color.textMuted)
                if !session.env.isEmpty {
                    Text(session.env).font(.mono(11.5)).foregroundStyle(Color.textFaint).lineLimit(1).truncationMode(.middle)
                }
                Spacer(minLength: 0)
                Changes(added: session.added, removed: session.removed)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Lines added and removed, in the status colors with their signs.
struct Changes: View {
    let added: Int
    let removed: Int

    var body: some View {
        if added + removed > 0 {
            HStack(spacing: 6) {
                Text("+" + String(added)).foregroundStyle(Color.success)
                Text("−" + String(removed)).foregroundStyle(Color.failure)
            }
            .font(.mono(11.5))
        }
    }
}

/// A session's state: a dot and a word, never the color alone.
struct StatusChip: View {
    let status: SessionStatus

    var body: some View {
        HStack(spacing: 6) {
            StatusDot(status: status)
            Text(status.word).font(.mono(11, .medium)).foregroundStyle(tone)
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(RoundedRectangle(cornerRadius: Radius.chip).fill(tone.opacity(0.12)))
    }

    private var tone: Color {
        switch status {
        case .idle: .textMuted
        case .working: .attention
        case .needsInput: .amberInk
        case .error: .failure
        }
    }
}

struct StatusDot: View {
    let status: SessionStatus
    @State private var pulse = false
    @Environment(\.accessibilityReduceMotion) private var still

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 7, height: 7)
            .opacity(status == .working && pulse ? 0.35 : 1)
            .animation(status == .working ? .easeInOut(duration: 0.9).repeatForever() : .default, value: pulse)
            // Reduce Motion: a steady dot; the color alone says it is working.
            .onAppear { pulse = status == .working && !still }
            .onChange(of: status) { _, s in pulse = s == .working && !still }
            .onChange(of: still) { _, now in pulse = status == .working && !now }
            .accessibilityHidden(true)
    }

    private var color: Color {
        switch status {
        case .idle: .textFaint
        case .working: .attention
        case .needsInput: .amber
        case .error: .failure
        }
    }
}

/// The link's state as the live dot: amber while up.
private struct LinkDot: View {
    let connection: AppModel.Connection

    var body: some View {
        Circle()
            .fill(connection == .connected ? Color.amber : Color.textFaint)
            .frame(width: 7, height: 7)
            .accessibilityLabel(connection == .connected ? "Connected" : "Not connected")
    }
}

private struct Empty: View {
    let mood: Mood

    var body: some View {
        VStack(spacing: 14) {
            Mascot(mood: mood, width: 110)
            Text("No sessions yet").font(.geist(18, .semibold))
            Text("Start one here with the pencil, or in Arakun on your computer.")
                .font(.callout)
                .foregroundStyle(Color.textMuted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 48)
    }
}

/// How the link to the computer is doing, when it is not simply up.
struct ConnectionBanner: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        switch model.connection {
        case .connected, .idle:
            EmptyView()
        case .connecting:
            line(icon: "loading03", "Reaching \(model.host.isEmpty ? "your computer" : model.host) through the relay…",
                 detail: "Arakun must be running on the computer, signed in to the same account.", tone: .textMuted)
        case .computerAway(let since):
            line(icon: "computer", "Your computer is offline.",
                 detail: since.map { "Last seen \($0.shortAgo) ago. The app reconnects when it is back." }, tone: .textMuted)
        case .limited(let which, _):
            // Said here once, for as long as it holds: an alert at every try
            // would come back for hours.
            line(icon: "alert-circle", Self.limit(which), detail: Self.limitDetail(which), tone: .textMuted, retry: true)
        }
    }

    private static func limit(_ which: String) -> String {
        switch which {
        case "daily": "Today's relay limit for your account is used up."
        case "phones": "Too many devices are connected to this computer."
        default: "The relay is busy."
        }
    }

    private static func limitDetail(_ which: String) -> String {
        switch which {
        case "daily": "It starts over at midnight UTC, and the app reconnects then."
        case "phones": "Close Arakun on one of them. The app tries again in a few minutes."
        default: "The app tries again in a few minutes."
        }
    }

    private func line(icon: String, _ text: String, detail: String?, tone: Color, retry: Bool = false) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Icon(icon, size: 16).padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                Text(text).font(.callout)
                if let detail { Text(detail).font(.small).foregroundStyle(Color.textFaint) }
                if retry {
                    Button("Try again") { model.reconnectNow() }
                        .buttonStyle(.outline)
                        .padding(.top, 8)
                }
            }
            Spacer(minLength: 0)
        }
        .foregroundStyle(tone)
        .padding(12)
        .card()
    }
}

extension SessionStatus {
    /// The word beside the status dot.
    var word: String {
        switch self {
        case .idle: "Idle"
        case .working: "Working"
        case .needsInput: "Needs you"
        case .error: "Error"
        }
    }
}

extension Date {
    /// "now", "4m", "3h", "2d": the inbox's short ages.
    var shortAgo: String {
        let s = max(0, Int(Date().timeIntervalSince(self)))
        switch s {
        case ..<60: return "now"
        case ..<3600: return "\(s / 60)m"
        case ..<86400: return "\(s / 3600)h"
        default: return "\(s / 86400)d"
        }
    }
}
