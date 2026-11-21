import SwiftUI

@main
struct ArakunApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var phase

    init() {
        Appearance.apply()
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
    }
}
