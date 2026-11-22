import SwiftUI

@main
struct ArakunApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var phase

    init() {
        Appearance.apply()
        // Early, so a crash report left by the last run goes out.
        Telemetry.apply()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .onOpenURL { url in model.pair(Self.pairingText(url)) }
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
