import ArakunKit
import SwiftUI

/// A new session in one of the places the computer offers, with its first prompt.
struct NewSessionView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var placePath = ""
    @State private var text = ""
    @State private var sent = false
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
                        Eyebrow("Prompt")
                        TextField("", text: $text, prompt: Text("What should the agent do?").foregroundStyle(Color.textFaint), axis: .vertical)
                            .font(.body16)
                            .lineLimit(5...14)
                            .focused($writing)
                            .padding(14)
                            .card(stroke: writing ? .lineStrong : .line)
                    }
                }
                .padding(16)
            }
            .background(Color.ground.ignoresSafeArea())
            .safeAreaInset(edge: .bottom) {
                Button {
                    guard let p = model.places.first(where: { $0.path == placePath }) else { return }
                    sent = true
                    model.startSession(in: p, text: text.trimmingCharacters(in: .whitespacesAndNewlines))
                } label: {
                    HStack(spacing: 10) {
                        if sent { ProgressView().tint(Color.onAmber) }
                        Text(sent ? "Starting…" : "Start session")
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.primary)
                .disabled(sent || placePath.isEmpty || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !model.isLinked)
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
            .onAppear { if placePath.isEmpty { placePath = model.places.first?.path ?? "" } }
            .onChange(of: model.session?.id) { _, id in
                if sent, let id { started(id) }
            }
            .onChange(of: model.notice) { _, n in
                // The computer refused it: let the form be sent again.
                if n != nil { sent = false }
            }
        }
        .presentationBackground(Color.ground)
    }

    private func place(_ p: Place) -> some View {
        let on = p.path == placePath
        return Button {
            placePath = p.path
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
