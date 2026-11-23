import ArakunKit
import Foundation

/// What the app keeps in the Keychain: the account's session, on this device
/// only (ArakunKit's `KeychainSessionStore`).
enum Keychain {
    static let session = KeychainSessionStore()

    /// Before sign-in, pairing through the relay left a ticket and a token
    /// here. The account's session replaces them, so they go on the first
    /// launch after the update. The app never paired any other way, so
    /// nothing else was kept under this name.
    static func forgetOldPairing() {
        KeychainItem(service: "com.arakun.ios.relay", account: "credentials").write(nil)
    }
}
