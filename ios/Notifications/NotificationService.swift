import ArakunKit
import Foundation
import UserNotifications

/// Reads a notice before iOS shows it. The cloud Worker sends APNs an alert
/// that says only "Open Arakun to see what changed.", with the computer's
/// encrypted notice in `e`. Decrypted with this phone's push keys, the
/// computer's own title and body replace it, grouped by session. When it
/// does not decrypt, the alert shows as it came.
final class NotificationService: UNNotificationServiceExtension {
    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        guard let content = request.content.mutableCopy() as? UNMutableNotificationContent else {
            return contentHandler(request.content)
        }
        let group = Bundle.main.object(forInfoDictionaryKey: "ArakunKeychainGroup") as? String
        guard let e = content.userInfo["e"] as? String,
              let keys = PushKeyStore(accessGroup: group).load(),
              let notice = try? WebPush.open(e, keys: keys)
        else { return contentHandler(content) }
        if !notice.title.isEmpty { content.title = notice.title }
        if !notice.body.isEmpty { content.body = notice.body }
        content.subtitle = notice.subtitle
        content.threadIdentifier = notice.session
        // Only what a tap needs stays; the sealed notice goes.
        content.userInfo = ["session": notice.session, "kind": notice.kind]
        contentHandler(content)
    }
}
