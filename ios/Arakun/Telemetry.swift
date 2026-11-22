import Foundation
import PostHog

/// Crash reports, sent to Arakun's PostHog project only after the user says
/// yes, as the desktop sends its own (src/telemetry.rs).
///
/// Only the crash goes: its stack trace and the device model and system
/// version. No screens, taps, sessions or usage, no person profile, no
/// location, and never a prompt, a message or code. The answer is kept on this
/// phone and changed from the More menu. Debug builds send nothing.
@MainActor
enum Telemetry {
    /// The project token: write-only, so it is safe in the app. The desktop's.
    private static let key = "phc_kXKrLfaWumKa3mfXcFVBCRpyKa6DZiUv7hCkQW2epLQZ"
    private static let host = "https://us.i.posthog.com"
    /// True or false once asked; missing until then.
    private static let answer = "crashReports"
    private static var started = false

    /// The user's answer, or nil before they have given one.
    static var consent: Bool? { UserDefaults.standard.object(forKey: answer) as? Bool }

    static func set(_ yes: Bool) {
        UserDefaults.standard.set(yes, forKey: answer)
        apply()
    }

    /// Starts, resumes or stops sending to match the answer. Called at launch
    /// and on every change.
    static func apply() {
        #if !DEBUG
        guard consent == true else {
            if started { PostHogSDK.shared.optOut() }
            return
        }
        guard !started else {
            PostHogSDK.shared.optIn()
            return
        }
        started = true
        let c = PostHogConfig(projectToken: key, host: host)
        c.errorTrackingConfig.autoCapture = true
        // Everything else the SDK would collect stays off.
        c.captureApplicationLifecycleEvents = false
        c.captureScreenViews = false
        c.captureElementInteractions = false
        c.rageClickConfig.enabled = false
        c.sessionReplay = false
        c.surveys = false
        c.preloadFeatureFlags = false
        c.sendFeatureFlagEvent = false
        c.personProfiles = .never
        c.setDefaultPersonProperties = false
        c.disableGeoIp = true
        // Should a later SDK start sending something new, it is dropped here.
        let onlyCrashes: BeforeSendBlock = { $0.event == "$exception" ? $0 : nil }
        c.setBeforeSend(onlyCrashes)
        PostHogSDK.shared.setup(c)
        // The SDK keeps an opt-out from an earlier "no"; the yes given since wins.
        PostHogSDK.shared.optIn()
        #endif
    }
}
