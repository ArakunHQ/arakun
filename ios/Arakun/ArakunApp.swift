import ArakunKit
import SwiftUI

@main
struct ArakunApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var phase
    /// A pairing link another app opened, until the user says yes or no.
    @State private var opened: PairingLink?

    init() {
        Appearance.apply()
        // Early, so a crash report left by the last run goes out.
        Telemetry.apply()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                // Any web page or message can open a link, so it never pairs
                // on its own: that would let a stranger's computer take over
                // the phone, or replace the one it is paired with.
                .onOpenURL { url in
                    let text = Self.pairingText(url)
                    guard let link = PairingLink(text) else { return model.pair(text) }
                    guard link.secure else {
                        model.pairError = "That link does not use a secure connection. Scan the code in Arakun instead."
                        return
                    }
                    opened = link
                }
                .alert(
                    model.credentials == nil ? "Pair with this computer?" : "Replace your paired computer?",
                    isPresented: Binding(get: { opened != nil }, set: { if !$0 { opened = nil } }),
                    presenting: opened
                ) { link in
                    Button(model.credentials == nil ? "Pair" : "Replace") { model.pair(Self.text(of: link)) }
                    Button("Cancel", role: .cancel) {}
                } message: { link in
                    Text(model.credentials == nil
                        ? "A link asks to pair this phone with \(link.host). Only go on if you opened it from Arakun on your own computer."
                        : "A link asks to pair this phone with \(link.host) instead of the computer it is paired with now. Only go on if you opened it from Arakun on your own computer.")
                }
                .onChange(of: phase) { _, now in model.visible(now == .active) }
        }
    }

    /// The QR code holds an https link to the relay; `arakun://pair?link=<it>`
    /// carries the same, for a link opened from elsewhere.
    static func pairingText(_ url: URL) -> String {
        guard url.scheme == "arakun",
              let link = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "link" })?.value
        else { return url.absoluteString }
        return link
    }

    /// The link back as text, for `AppModel.pair`.
    static func text(of link: PairingLink) -> String {
        var c = URLComponents()
        c.scheme = link.secure ? "https" : "http"
        c.host = link.host.split(separator: ":").first.map(String.init)
        if let port = link.host.split(separator: ":").dropFirst().first { c.port = Int(port) }
        c.percentEncodedFragment = "p=\(Self.encode(link.code))&s=\(Self.encode(link.secret))"
        return c.string ?? ""
    }

    private static func encode(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? s
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    /// The one question about crash reports, until it has an answer.
    @State private var askCrashReports = Telemetry.consent == nil && !AppModel.isDemoLaunch

    var body: some View {
        Group {
            #if DEBUG
            if AppModel.isDemo, UserDefaults.standard.bool(forKey: "demoMoods") {
                MoodGallery()
            } else if model.credentials == nil {
                PairView()
            } else {
                InboxView()
            }
            #else
            if model.credentials == nil {
                PairView()
            } else {
                InboxView()
            }
            #endif
        }
        .font(.body16)
        .foregroundStyle(Color.text)
        .tint(.amberInk)
        .background(Color.ground.ignoresSafeArea())
        .animation(.smooth, value: model.credentials == nil)
        .alert("Send crash reports?", isPresented: $askCrashReports) {
            Button("Send crash reports") { Telemetry.set(true) }
            Button("Don't send", role: .cancel) { Telemetry.set(false) }
        } message: {
            Text("When Arakun crashes, it can send where in its code it happened, so we can fix it. Never your sessions, messages or code. Change it any time in the More menu.")
        }
    }
}
