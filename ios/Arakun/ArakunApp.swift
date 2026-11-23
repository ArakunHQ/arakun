import ArakunKit
import SwiftUI
import UIKit

@main
struct ArakunApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var phase
    /// A computer's page a link asks to open, until the person says yes or no.
    @State private var opened: URL?

    init() {
        Appearance.apply()
        // Early, so a crash report left by the last run goes out.
        Telemetry.apply()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                // Any web page or message can open a link, so it never acts on
                // its own: an old relay link gets its sentence, and a link to a
                // computer's own page opens in the browser only after a yes.
                .onOpenURL { url in opened = model.opened(Self.pairingText(url)) }
                .alert(
                    "Open this computer's page?",
                    isPresented: Binding(get: { opened != nil }, set: { if !$0 { opened = nil } }),
                    presenting: opened
                ) { page in
                    Button("Open") { UIApplication.shared.open(page) }
                    Button("Cancel", role: .cancel) {}
                } message: { page in
                    Text("A link asks to open the page of \(page.host ?? "a computer") in the browser, to pair the browser with it. Only go on if you opened it from Arakun on your own computer.")
                }
                .onChange(of: phase) { _, now in model.visible(now == .active) }
        }
    }

    /// `arakun://pair?link=<link>` carries a pairing link opened from elsewhere.
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
    @Environment(\.accessibilityReduceMotion) private var still

    var body: some View {
        Group {
            #if DEBUG
            if AppModel.isDemo, UserDefaults.standard.bool(forKey: "demoMoods") {
                MoodGallery()
            } else {
                screen
            }
            #else
            screen
            #endif
        }
        .font(.body16)
        .foregroundStyle(Color.text)
        .tint(.amberInk)
        .background(Color.ground.ignoresSafeArea())
        .animation(still ? nil : .smooth, value: model.signedIn == nil)
        .animation(still ? nil : .smooth, value: model.machine?.label)
        .alert("Send crash reports?", isPresented: $askCrashReports) {
            Button("Send crash reports") { Telemetry.set(true) }
            Button("Don't send", role: .cancel) { Telemetry.set(false) }
        } message: {
            Text("When Arakun crashes, it can send where in its code it happened, so we can fix it. Never your sessions, messages or code. Change it any time in Settings.")
        }
    }

    /// Signed out: sign in. Signed in: the computers, then the one picked.
    @ViewBuilder private var screen: some View {
        if model.signedIn == nil {
            SignInView()
        } else if model.machine == nil {
            MachinesView()
        } else {
            InboxView()
        }
    }
}
