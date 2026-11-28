import ArakunKit
import SwiftUI

/// A new session in one of the places the computer offers, with its first prompt.
struct NewSessionView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var placePath = ""
    @State private var text = ""
    /// The agent the session starts with; a place picks its own at first.
    @State private var agent = ""
    /// The model and reasoning picked here; the rest stay as the agent would start.
    @State private var picks = StartPicks()
    /// Photos for the first prompt; they stay until the session has started.
    @State private var photos: [Photo] = []
    @FocusState private var writing: Bool
    let started: (String) -> Void

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 10) {
                        Eyebrow("Where")
                        VStack(spacing: 0) {
                            ForEach(Array(model.places.enumerated()), id: \.element.id) { i, p in
                                if i > 0 { Rectangle().fill(Color.line).frame(height: 1).padding(.leading, 44) }
                                place(p)
                            }
                        }
                        .card()
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Eyebrow("Agent")
                        agents
                    }
                    if let o = options {
                        ForEach(o.settings) { set in
                            SettingRows(set: set, picked: picks[set.key]) { picks.pick(set.key, $0) }
                        }
                    } else {
                        Text(model.isLinked ? "Loading models…" : "Waiting for the computer")
                            .font(.small)
                            .foregroundStyle(Color.textFaint)
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Eyebrow("Prompt")
                        TextField("", text: $text, prompt: Text("What should the agent do?").foregroundStyle(Color.textFaint), axis: .vertical)
                            .font(.body16)
                            .lineLimit(5...14)
                            .focused($writing)
                            .padding(14)
                            .card(stroke: writing ? .lineStrong : .line)
                        HStack(alignment: .center, spacing: 10) {
                            PhotoButton(photos: $photos, labelled: true)
                            PhotoChips(photos: $photos)
                        }
                    }
                }
                .padding(16)
            }
            .background(Color.ground.ignoresSafeArea())
            .safeAreaInset(edge: .bottom) {
                Button {
                    guard let p = model.places.first(where: { $0.path == placePath }) else { return }
                    // Picks for another place or agent than the options shown are not sent.
                    model.startSession(in: p, agent: agent, settings: options == nil ? StartPicks() : picks,
                                       text: text.trimmingCharacters(in: .whitespacesAndNewlines), photos: photos)
                } label: {
                    HStack(spacing: 10) {
                        if model.starting { ProgressView().tint(Color.onAmber) }
                        Text(model.starting ? "Starting…" : "Start session")
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.primary)
                // The model's flag, not the form's: a start sent before Cancel
                // still blocks another until it is answered.
                .disabled(model.starting || !model.places.contains(where: { $0.path == placePath }) || (text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && photos.isEmpty) || !model.isLinked)
                .padding(16)
                .background(Color.ground)
            }
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }.foregroundStyle(Color.textMuted)
                }
            }
            .onAppear { if placePath.isEmpty { choose(model.places.first) } }
            .onChange(of: model.places) { _, places in
                // The chosen place went away: fall back to the first one left.
                if !places.contains(where: { $0.path == placePath }) { choose(places.first) }
            }
            // What a session there would start with. Asked again once the link is
            // back, as an ask while away is lost.
            .task(id: Asked(env: placePath, agent: agent, linked: model.isLinked)) {
                guard !placePath.isEmpty, !agent.isEmpty, model.isLinked else { return }
                model.loadStartOptions(env: placePath, agent: agent)
            }
            .onChange(of: model.session?.id) { _, id in
                if let id { started(id) }
            }
        }
        .presentationBackground(Color.ground)
    }

    private struct Asked: Equatable {
        let env: String
        let agent: String
        let linked: Bool
    }

    /// The computer's answer, when it is about the place and agent picked now.
    private var options: StartOptions? {
        guard let o = model.startOptions, o.isFor(env: placePath, agent: agent) else { return nil }
        return o
    }

    /// A place, with the agent it starts and none of the picks for another.
    private func choose(_ p: Place?) {
        placePath = p?.path ?? ""
        agent = p.map { $0.agent.isEmpty ? Agents.all[0].id : $0.agent } ?? ""
        picks.clear()
    }

    /// The agents on offer, the place's own among them even when this app does not know it.
    private var agents: some View {
        var list = Agents.all.map(\.id)
        if !agent.isEmpty, !list.contains(agent) { list.insert(agent, at: 0) }
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(list, id: \.self) { id in
                    let on = id == agent
                    Button {
                        guard !on else { return }
                        agent = id
                        picks.clear()
                    } label: {
                        Text(Agents.name(id))
                            .font(.geist(14, .medium))
                            .foregroundStyle(on ? Color.text : Color.textMuted)
                            .padding(.horizontal, 12)
                            .frame(minHeight: 36)
                            .background(RoundedRectangle(cornerRadius: Radius.control).fill(on ? Color.amberSoft : Color.raised))
                            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(on ? Color.amberInk : Color.line))
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
        }
    }

    private func place(_ p: Place) -> some View {
        let on = p.path == placePath
        return Button {
            if !on { choose(p) }
        } label: {
            HStack(spacing: 12) {
                Icon("folder01", size: 18).foregroundStyle(on ? Color.amberInk : Color.textFaint)
                VStack(alignment: .leading, spacing: 3) {
                    Text(p.project.isEmpty ? p.path : p.project).font(.geist(15, .medium)).foregroundStyle(Color.text)
                    HStack(spacing: 6) {
                        if !p.env.isEmpty { Text(p.env) }
                        if let b = p.branch, b != p.env {
                            Icon("git-branch", size: 11)
                            Text(b)
                        }
                    }
                    .font(.mono(11.5))
                    .foregroundStyle(Color.textFaint)
                    .lineLimit(1)
                }
                Spacer()
                ZStack {
                    Circle().strokeBorder(on ? Color.amberInk : Color.lineStrong, lineWidth: 1.5)
                    if on { Circle().fill(Color.amberInk).padding(4) }
                }
                .frame(width: 18, height: 18)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}
