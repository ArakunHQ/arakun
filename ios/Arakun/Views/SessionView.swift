import ArakunKit
import SwiftUI

/// One session, live: its entries, what it asks, and the composer.
struct SessionView: View {
    let id: String
    @Environment(AppModel.self) private var model
    @State private var draft = ""
    @FocusState private var typing: Bool
    /// Whether the end of the transcript is on screen; new entries follow only then.
    @State private var atEnd = true
    @State private var unseen = false
    /// The files the session changed and their diffs, in a sheet over it.
    @State private var showingChanges = false
    @Environment(\.accessibilityReduceMotion) private var still

    var body: some View {
        let s = model.session?.id == id ? model.session : nil
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 18) {
                    if let s {
                        SessionHeader(session: s) { showingChanges = true }
                        if s.from > 0 {
                            Button(model.loadingOlder ? "Loading…" : "Show \(s.from) earlier entries") { model.loadOlder() }
                                .buttonStyle(.outline)
                                .frame(maxWidth: .infinity)
                                // Offline it would ask nothing.
                                .disabled(model.loadingOlder || !model.isLinked)
                        }
                        ForEach(Array(s.entries.enumerated()), id: \.offset) { i, e in
                            EntryView(entry: e).id(s.from + i)
                        }
                        if s.status == .working, let started = s.turnStarted {
                            WorkingLine(since: started)
                        }
                    } else {
                        ProgressView().tint(Color.textMuted).frame(maxWidth: .infinity).padding(.top, 60)
                    }
                    // Room for the raccoon perched over the composer, inside the
                    // list so scrolling to the end keeps the last entry clear of it.
                    Color.clear.frame(height: 48).id("end")
                        .onAppear { atEnd = true; unseen = false }
                        .onDisappear { atEnd = false }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .scrollDismissesKeyboard(.interactively)
            .defaultScrollAnchor(.bottom)
            .onChange(of: s?.count) { _, _ in
                // Someone reading further up keeps their place.
                if atEnd {
                    withAnimation(still ? nil : .default) { proxy.scrollTo("end", anchor: .bottom) }
                } else {
                    unseen = true
                }
            }
            .overlay(alignment: .bottom) {
                if unseen {
                    Button {
                        withAnimation(still ? nil : .default) { proxy.scrollTo("end", anchor: .bottom) }
                    } label: {
                        HStack(spacing: 6) {
                            Icon("arrow-down01", size: 14)
                            Text("New entries").font(.geist(14, .medium))
                        }
                    }
                    .buttonStyle(.outline)
                    .padding(.bottom, 8)
                    .transition(.opacity)
                }
            }
        }
        .background(Color.ground.ignoresSafeArea())
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 10) {
                ConnectionBanner()
                // Offline the cards stay but wait: an answer now would be lost.
                Group {
                    if let p = s?.permission {
                        PermissionCard(ask: p) { model.allow($0) }
                    } else if let q = s?.question {
                        QuestionCard(ask: q, answer: model.answer, skip: model.skipQuestion).id(q.toolCallId)
                    } else if let r = s?.secret {
                        SecretCard(ask: r) { model.answerSecret(r, values: $0) }.id(r.id)
                    }
                }
                .disabled(!model.isLinked)
                if let s, !s.queue.isEmpty {
                    QueueCard(queue: s.queue, paused: s.queuePaused, linked: model.isLinked,
                              takeBack: takeBack, resume: model.resumeQueue)
                }
                Composer(draft: $draft, typing: $typing, working: s?.status == .working, linked: model.isLinked,
                         send: { send() }, stopAndSend: { send(now: true) }, stop: model.stop)
            }
            .padding(.horizontal, 12)
            .padding(.top, 10)
            .padding(.bottom, 6)
            .background(Color.ground)
            // The hairline first, so the raccoon perched across it is drawn over it.
            .overlay(alignment: .top) { Rectangle().fill(Color.line).frame(height: 1) }
            .overlay(alignment: .topTrailing) {
                // Perched on the edge, over the last lines: it shows how the session is doing.
                Mascot(mood: mood(s), width: 52)
                    .offset(x: -20, y: -50)
            }
        }
        .navigationTitle(s?.title.isEmpty == false ? s!.title : "Session")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Color.ground, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(s?.title.isEmpty == false ? s!.title : "Session")
                        .font(.geist(15, .semibold, relativeTo: .headline))
                        .lineLimit(1)
                    if let s {
                        HStack(spacing: 6) {
                            StatusDot(status: s.status)
                            Text([s.agent, s.status.word].filter { !$0.isEmpty }.joined(separator: " · "))
                                .font(.mono(11, relativeTo: .caption2))
                                .foregroundStyle(Color.textMuted)
                        }
                    }
                }
                .frame(maxWidth: 260)
            }
            // A computer from before muting has no `/mute`: no switch for it.
            if model.canMute {
                ToolbarItem(placement: .topBarTrailing) {
                    let muted = model.isMuted(id)
                    Button { model.mute(session: id, muted: !muted) } label: {
                        Image(systemName: muted ? "bell.slash" : "bell")
                    }
                    .foregroundStyle(muted ? Color.textFaint : Color.text)
                    .disabled(!model.isLinked)
                    .accessibilityLabel(muted ? "Unmute" : "Mute")
                }
            }
        }
        .sheet(isPresented: $showingChanges) { ChangesView(id: id) }
        .onAppear { if model.session?.id != id { model.open(id) } }
        .onDisappear { if model.session?.id == id { model.close() } }
    }

    private func mood(_ s: LiveSession?) -> Mood {
        if case .computerAway = model.connection { return .sleeping }
        guard let s else { return .thinking }
        switch s.status {
        case .needsInput: return .waiting
        case .error: return .error
        case .working:
            if case .thought = s.entries.last { return .thinking }
            return .working
        case .idle: return .idle
        }
    }

    private func send(now: Bool = false) {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        // Not sent (the link is down): the text stays to send again.
        if model.say(text, now: now) { draft = "" }
    }

    /// Out of the queue and into the box, after anything written there.
    private func takeBack(_ index: Int, _ text: String) {
        guard model.takeBack(index, text: text) else { return }
        let had = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        draft = had.isEmpty ? text : "\(had)\n\n\(text)"
    }
}

/// What the session has cost and changed so far, above its first entry.
private struct SessionHeader: View {
    let session: LiveSession
    let openChanges: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                if let u = session.usage {
                    Text(u).font(.mono(11.5)).foregroundStyle(Color.textFaint)
                }
                Spacer(minLength: 0)
                if session.changes.files > 0 {
                    // The counts open the list of files; the outline says they can be tapped.
                    Button(action: openChanges) {
                        HStack(spacing: 6) {
                            Icon("file-diff", size: 13).foregroundStyle(Color.textFaint)
                            Text(session.changes.files == 1 ? "1 file" : "\(session.changes.files) files")
                                .font(.mono(11.5))
                                .foregroundStyle(Color.textMuted)
                            Changes(added: session.changes.added, removed: session.changes.removed)
                            Icon("arrow-right01", size: 11).foregroundStyle(Color.textFaint)
                        }
                        .padding(.horizontal, 10)
                        .frame(minHeight: 32)
                        .background(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.line))
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Show changes")
                }
            }
        }
        .padding(.bottom, 4)
    }
}

/// The running turn's clock, under the last entry.
private struct WorkingLine: View {
    let since: Date

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { ctx in
            HStack(spacing: 8) {
                StatusDot(status: .working)
                Text("Working · \(Duration.seconds(max(0, ctx.date.timeIntervalSince(since))).formatted(.time(pattern: .minuteSecond)))")
                    .font(.mono(12))
                    .foregroundStyle(Color.textMuted)
            }
        }
    }
}

struct Composer: View {
    @Binding var draft: String
    var typing: FocusState<Bool>.Binding
    let working: Bool
    let linked: Bool
    let send: () -> Void
    /// Stops the running turn and sends what is written once it stopped.
    let stopAndSend: () -> Void
    let stop: () -> Void

    private var empty: Bool { draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("", text: $draft, prompt: Text(working ? "Queue a follow-up" : "Message the agent").foregroundStyle(Color.textFaint),
                      axis: .vertical)
                .font(.body16)
                .lineLimit(1...6)
                .focused(typing)
                .padding(.horizontal, 14)
                .padding(.vertical, 11)
            if working && empty {
                Button(action: stop) {
                    HStack(spacing: 6) {
                        Icon("square", size: 14)
                        Text("Stop").font(.geist(14, .medium))
                    }
                    .foregroundStyle(Color.failure)
                    .padding(.horizontal, 12)
                    .frame(height: 34)
                    .background(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.failure.opacity(0.5)))
                }
                // Offline the cancel would be dropped, as a message would.
                .disabled(!linked)
                .opacity(linked ? 1 : 0.5)
                .padding(6)
            } else {
                let ready = !empty && linked
                if working {
                    // Send alone queues it behind the turn; this goes now.
                    Button(action: stopAndSend) {
                        Icon("square", size: 14)
                            .foregroundStyle(Color.failure)
                            .frame(width: 34, height: 34)
                            .background(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.failure.opacity(0.5)))
                    }
                    .disabled(!ready)
                    .opacity(ready ? 1 : 0.5)
                    .padding(.vertical, 6)
                    .accessibilityLabel("Stop and send")
                }
                Button(action: send) {
                    Icon("arrow-up01", size: 18)
                        .foregroundStyle(ready ? Color.onAmber : Color.textFaint)
                        .frame(width: 34, height: 34)
                        .background(RoundedRectangle(cornerRadius: Radius.control).fill(ready ? Color.amber : Color.lineStrong))
                }
                .disabled(!ready)
                .animation(.snappy(duration: 0.15), value: ready)
                .padding(6)
                .accessibilityLabel("Send")
            }
        }
        .background(RoundedRectangle(cornerRadius: Radius.card).fill(Color.raised))
        .overlay(RoundedRectangle(cornerRadius: Radius.card).strokeBorder(typing.wrappedValue ? Color.lineStrong : Color.line))
    }
}

/// The prompts the computer holds until the turn ends, over the composer.
/// Each can be taken back into the box; after a Stop they wait for Resume.
struct QueueCard: View {
    let queue: [String]
    let paused: Bool
    let linked: Bool
    let takeBack: (Int, String) -> Void
    let resume: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Eyebrow(paused ? "Paused" : "Queued")
                Text(paused ? "· waits until you resume" : "· sends when this turn ends")
                    .font(.mono(11))
                    .foregroundStyle(Color.textFaint)
                    .lineLimit(1)
                Spacer(minLength: 0)
                if paused {
                    Button("Resume", action: resume)
                        .buttonStyle(.outline)
                        .disabled(!linked)
                }
            }
            .frame(minHeight: 40)
            ForEach(Array(queue.enumerated()), id: \.offset) { i, q in
                HStack(spacing: 8) {
                    Text(q)
                        .font(.callout)
                        .foregroundStyle(Color.textMuted)
                        .lineLimit(2)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Button { takeBack(i, q) } label: {
                        Text("Take back")
                            .font(.geist(14, .medium))
                            .foregroundStyle(Color.text)
                            .padding(.horizontal, 10)
                            .frame(minHeight: 40)
                    }
                    .buttonStyle(.plain)
                    .disabled(!linked)
                    .opacity(linked ? 1 : 0.5)
                    .accessibilityLabel("Take back: \(q)")
                }
            }
        }
        .padding(.leading, 14)
        .padding(.trailing, 6)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: Radius.card).strokeBorder(Color.lineStrong, style: .init(lineWidth: 1, dash: [4, 3])))
    }
}
