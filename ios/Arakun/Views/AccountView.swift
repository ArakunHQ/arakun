import ArakunKit
import SwiftUI

/// Settings: who is signed in, the computer open, the account's signed-in
/// devices with Sign out, crash reports, and signing this phone out.
struct AccountView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var confirmSignOut = false
    @State private var signingOut: SignedInDevice?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 28) {
                    if let me = model.me {
                        section("Account") {
                            row(icon: "globe", me.login, detail: me.plan.isEmpty ? "GitHub" : "GitHub · \(me.plan) plan")
                        }
                    }
                    if let m = model.machine {
                        section("Computer") {
                            row(icon: m.kind == "server" ? "computer-terminal01" : "computer", m.name,
                                detail: model.isLinked ? "Connected through the relay" : "Not connected")
                            if model.machines.count > 1 {
                                divider
                                Button {
                                    dismiss()
                                    model.leaveMachine()
                                } label: {
                                    row(icon: "arrow-left01", "All computers", detail: nil)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    if !(model.me?.notices ?? []).isEmpty {
                        VStack(alignment: .leading, spacing: 10) {
                            Eyebrow("Notices").padding(.horizontal, 4)
                            NoticeBanners()
                        }
                    }
                    section("Signed-in devices") {
                        if let e = model.devicesError {
                            VStack(alignment: .leading, spacing: 10) {
                                Problem(text: e)
                                Button("Try again") { Task { await model.loadDevices() } }
                                    .buttonStyle(.outline)
                            }
                            .padding(16)
                        } else if model.devices.isEmpty {
                            row(icon: "loading03", "Loading…", detail: nil)
                        }
                        ForEach(Array(model.devices.enumerated()), id: \.element.id) { i, d in
                            if i > 0 { divider }
                            HStack(spacing: 12) {
                                row(icon: d.kind == "ios" || d.kind == "android" || d.kind == "phone" ? "signal-full02" : "computer",
                                    d.name, detail: detail(d))
                                Button("Sign out") { signingOut = d }
                                    .buttonStyle(.outline(.failure))
                                    .padding(.trailing, 12)
                            }
                        }
                    }
                    section("Crash reports") {
                        // A closure, not `set: Telemetry.set`: Swift 6.3 crashes making the thunk
                        // from a @MainActor function reference.
                        Toggle("Send crash reports", isOn: Binding(get: { Telemetry.consent == true }, set: { Telemetry.set($0) }))
                            .font(.body16)
                            .padding(16)
                    }
                    Button {
                        confirmSignOut = true
                    } label: {
                        Text("Sign out").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.outline(.failure))
                }
                .padding(16)
                .frame(maxWidth: 640)
                .frame(maxWidth: .infinity)
            }
            .background(Color.ground.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .refreshable { await model.loadDevices() }
            .task { await model.loadDevices() }
            .confirmationDialog("Sign out of \(model.me?.login ?? "GitHub") on this phone?", isPresented: $confirmSignOut, titleVisibility: .visible) {
                Button("Sign out", role: .destructive) {
                    dismiss()
                    model.signOut()
                }
            }
            .confirmationDialog(
                "Sign out \(signingOut?.name ?? "this device")?",
                isPresented: Binding(get: { signingOut != nil }, set: { if !$0 { signingOut = nil } }),
                titleVisibility: .visible,
                presenting: signingOut
            ) { d in
                Button("Sign out", role: .destructive) {
                    if d.current { dismiss() }
                    model.signOut(d)
                }
            } message: { d in
                Text(d.current
                    ? "This is this phone. It will need to sign in again."
                    : "It will need to sign in again to reach your computers.")
            }
        }
    }

    private func detail(_ d: SignedInDevice) -> String {
        let kind = switch d.kind {
        case "app": "Arakun app"
        case "server": "Server"
        case "phone": "Phone page"
        case "ios": "iPhone app"
        case "android": "Android app"
        default: d.kind
        }
        if d.current { return "\(kind) · this phone" }
        let ago = Date(timeIntervalSince1970: d.lastUsedAt).shortAgo
        return ago == "now" ? "\(kind) · used now" : "\(kind) · used \(ago) ago"
    }

    private var divider: some View {
        Rectangle().fill(Color.line).frame(height: 1).padding(.leading, 16)
    }

    private func section<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Eyebrow(title).padding(.horizontal, 4)
            VStack(alignment: .leading, spacing: 0) { content() }
                .frame(maxWidth: .infinity, alignment: .leading)
                .card()
        }
    }

    private func row(icon: String, _ title: String, detail: String?) -> some View {
        HStack(spacing: 12) {
            Icon(icon, size: 18).foregroundStyle(Color.textMuted)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.geist(16, .medium))
                if let detail { Text(detail).font(.small).foregroundStyle(Color.textMuted) }
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .contentShape(Rectangle())
    }
}

/// The operator's notices for this account, until put away.
struct NoticeBanners: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL

    var body: some View {
        ForEach(model.me?.notices ?? []) { n in
            HStack(alignment: .top, spacing: 10) {
                Icon("alert-circle", size: 16).padding(.top, 2)
                VStack(alignment: .leading, spacing: 4) {
                    Text(n.text).font(.callout)
                    if let link = n.link {
                        Button(link.absoluteString) { openURL(link) }
                            .font(.small)
                            .foregroundStyle(Color.amberInk)
                            .buttonStyle(.plain)
                    }
                }
                Spacer(minLength: 0)
                Button { model.dismiss(n) } label: { Icon("cancel01", size: 14).padding(4) }
                    .buttonStyle(.plain)
                    .foregroundStyle(Color.textMuted)
                    .accessibilityLabel("Dismiss")
            }
            .padding(12)
            .card()
        }
    }
}
