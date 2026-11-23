import ArakunKit
import SwiftUI

/// Signed in: the account's computers, to pick the one to open.
struct MachinesView: View {
    @Environment(AppModel.self) private var model
    @State private var settings = false
    @State private var confirmSignOut = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    header
                    NoticeBanners()
                    if model.machinesLoaded, let said = model.accountSaid {
                        Problem(text: said)
                    }
                    content
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 32)
                .frame(maxWidth: 640)
                .frame(maxWidth: .infinity)
            }
            .background(Color.ground.ignoresSafeArea())
            .refreshable { await model.refreshMachines() }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        if let me = model.me { Text("Signed in as \(me.login)") }
                        Button("Settings", systemImage: "gearshape") { settings = true }
                        Button("Sign out", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) { confirmSignOut = true }
                    } label: {
                        Icon("more-horizontal", size: 22).foregroundStyle(Color.textMuted)
                    }
                    .accessibilityLabel("More")
                }
            }
            .sheet(isPresented: $settings) { AccountView() }
            .confirmationDialog("Sign out of \(model.me?.login ?? "GitHub") on this phone?", isPresented: $confirmSignOut, titleVisibility: .visible) {
                Button("Sign out", role: .destructive) { model.signOut() }
            }
            .alert("Arakun", isPresented: Binding(get: { model.notice != nil }, set: { if !$0 { model.notice = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(model.notice ?? "")
            }
            // The online states stay fresh while the list is on screen.
            .task {
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(15))
                    guard !Task.isCancelled else { return }
                    await model.poll()
                }
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Eyebrow(model.me.map { "Signed in as \($0.login)" } ?? "Arakun")
            Text("Computers")
                .font(.geist(34, .semibold, relativeTo: .largeTitle))
                .tracking(-1.0)
        }
        .padding(.top, 4)
    }

    @ViewBuilder private var content: some View {
        if model.blocked {
            EmptyView()
        } else if !model.machinesLoaded {
            VStack(spacing: 14) {
                if let said = model.accountSaid {
                    Mascot(mood: .error, width: 110)
                    Text(said)
                        .font(.callout)
                        .foregroundStyle(Color.textMuted)
                        .multilineTextAlignment(.center)
                    Button("Try again") { Task { await model.loadAccount(openOne: true) } }
                        .buttonStyle(.outline)
                } else {
                    Mascot(mood: .thinking, width: 110)
                    Text("Loading your computers…").font(.callout).foregroundStyle(Color.textMuted)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 48)
        } else if model.machines.isEmpty {
            VStack(spacing: 14) {
                Mascot(mood: .idle, width: 110)
                Text("No computers yet").font(.geist(18, .semibold))
                Text("Sign in on a computer running Arakun.")
                    .font(.callout)
                    .foregroundStyle(Color.textMuted)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 48)
        } else {
            VStack(spacing: 0) {
                ForEach(Array(model.machines.enumerated()), id: \.element.id) { i, m in
                    if i > 0 { Rectangle().fill(Color.line).frame(height: 1).padding(.leading, 16) }
                    Button { model.pick(m) } label: { MachineRow(machine: m) }
                        .buttonStyle(.plain)
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: Radius.card))
            .card()
        }
    }
}

/// One computer: its name, what it is, and whether it is there.
struct MachineRow: View {
    let machine: Machine

    var body: some View {
        HStack(spacing: 12) {
            Icon(machine.kind == "server" ? "computer-terminal01" : "computer", size: 20)
                .foregroundStyle(Color.textMuted)
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name).font(.geist(16, .medium))
                Text(machine.kind == "server" ? "Server" : "Arakun app")
                    .font(.small)
                    .foregroundStyle(Color.textMuted)
            }
            Spacer(minLength: 8)
            HStack(spacing: 6) {
                Circle()
                    .fill(machine.online ? Color.amber : Color.textFaint)
                    .frame(width: 7, height: 7)
                Text(seen).font(.mono(11.5, relativeTo: .caption))
            }
            .foregroundStyle(machine.online ? Color.text : Color.textFaint)
        }
        .padding(16)
        .contentShape(Rectangle())
    }

    /// "online", "last seen 3h ago", or "offline" when it never was.
    private var seen: String {
        if machine.online { return "online" }
        guard let at = machine.lastSeenAt, at > 0 else { return "offline" }
        let ago = Date(timeIntervalSince1970: at).shortAgo
        return ago == "now" ? "last seen now" : "last seen \(ago) ago"
    }
}

/// A sentence about something that went wrong.
struct Problem: View {
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Icon("alert-circle", size: 15)
            Text(text)
        }
        .font(.small)
        .foregroundStyle(Color.failure)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
