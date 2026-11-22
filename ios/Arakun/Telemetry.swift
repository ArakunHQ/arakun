import ArakunKit
import Foundation
import PostHog

/// Crash reports, sent to Arakun's PostHog project only after the user says
/// yes, as the desktop sends its own (src/telemetry.rs).
///
/// Only the crash goes: its type, its message without links, paths, emails or
/// tokens, the stack trace, and the app, system and device model
/// (`CrashReport`). No screens, taps, sessions or usage, no person profile,
/// no location, and never a prompt, a message or code. The answer is kept on this
/// phone and changed from the More menu. Debug builds send nothing.
@MainActor
enum Telemetry {
    /// The project token: write-only, so it is safe in the app. The desktop's.
    private static let key = "phc_kXKrLfaWumKa3mfXcFVBCRpyKa6DZiUv7hCkQW2epLQZ"
    private static let host = "https://us.i.posthog.com"
    /// True or false once asked; missing until then.
    private nonisolated static let answer = "crashReports"
    private static var started = false

    /// The user's answer, or nil before they have given one.
    static var consent: Bool? { UserDefaults.standard.object(forKey: answer) as? Bool }

    static func set(_ yes: Bool) {
        UserDefaults.standard.set(yes, forKey: answer)
        apply()
    }

    /// Read off the main thread by `Gate`, so straight from the defaults.
    nonisolated static var allowed: Bool { UserDefaults.standard.object(forKey: answer) as? Bool == true }

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
        // Steps are a log of what the app did before the crash: not promised, not sent.
        c.errorTrackingConfig.exceptionSteps.enabled = false
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
        let onlyCrashes: BeforeSendBlock = { event in
            guard event.event == "$exception" else { return nil }
            event.properties = CrashReport.clean(event.properties)
            return event
        }
        c.setBeforeSend(onlyCrashes)
        // Opting out stops new reports, but the SDK would still send the ones
        // it has queued. Its requests fail while the answer is no, so they
        // wait, and go only if the user says yes again.
        let session = URLSessionConfiguration.default
        session.protocolClasses = [Gate.self] + (session.protocolClasses ?? [])
        c.urlSessionConfiguration = session
        PostHogSDK.shared.setup(c)
        // The SDK keeps an opt-out from an earlier "no"; the yes given since wins.
        PostHogSDK.shared.optIn()
        #endif
    }
}

/// Fails every request to PostHog while the user's answer is no.
private final class Gate: URLProtocol {
    override class func canInit(with _: URLRequest) -> Bool { !Telemetry.allowed }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
    }

    override func stopLoading() {}
}
