import ArakunKit
import Foundation
import UIKit
import UserNotifications

/// Where push notices are set up: the keys in the Keychain group the app
/// shares with its notification service extension, and which APNs the token
/// comes from.
enum PushSetup {
    /// `<team>.com.arakun.ios.shared`: Info.plist names it, so the build
    /// fills in the team, and Notifications/NotificationService.swift reads
    /// the same key.
    static let accessGroup = Bundle.main.object(forInfoDictionaryKey: "ArakunKeychainGroup") as? String
    static let keys = PushKeyStore(accessGroup: accessGroup)

    /// A development build's token comes from APNs's sandbox, and the Worker
    /// must send through it.
    static let sandbox: Bool = {
        #if DEBUG
        true
        #else
        false
        #endif
    }()

    /// Whether the person turned notifications on here. No secret, so
    /// UserDefaults; the keys stay in the Keychain.
    static let wantedKey = "arakun.notify"
}

/// What the system says about notifications for Arakun. Asked off the main
/// actor, since its settings object is not Sendable.
enum PushPermission: Sendable {
    /// Not asked yet.
    case ask
    case granted
    /// Turned off for Arakun in the iPhone's Settings.
    case denied

    static func current() async -> PushPermission {
        let status = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
        switch status {
        case .notDetermined: return .ask
        case .denied: return .denied
        default: return .granted
        }
    }

    /// Asks the person, the first time; after that the system answers alone.
    static func request() async -> Bool {
        (try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])) ?? false
    }
}

/// Hears APNs's token, and the notifications shown in the foreground or
/// tapped. It holds the model, so a tap that launched the app finds it.
@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    let model = AppModel()

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // Before launch ends, or the tap that opened the app is not passed on.
        UNUserNotificationCenter.current().delegate = self
        model.launched()
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        model.tookPushToken(deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // No token, so nothing to send; the next launch asks APNs again.
    }

    /// In the foreground: nothing for the session already on screen.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        let session = notification.request.content.userInfo["session"] as? String
        let onScreen = await MainActor.run { self.model.isOnScreen(session) }
        return onScreen ? [] : [.banner, .list, .sound]
    }

    /// A tap opens the session the notice is about.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            didReceive response: UNNotificationResponse) async {
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier,
              let session = response.notification.request.content.userInfo["session"] as? String
        else { return }
        await MainActor.run { self.model.tapped(session: session) }
    }
}
