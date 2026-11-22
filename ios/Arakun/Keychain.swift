import ArakunKit
import Foundation
import Security

/// The pairing credentials, kept in the Keychain: they are what lets anyone
/// holding them into the computer's sessions.
enum Keychain {
    private static let service = "com.arakun.ios.relay"
    private static let account = "credentials"

    static func load() -> Credentials? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
        ]
        var out: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        return try? JSONDecoder().decode(Credentials.self, from: data)
    }

    /// Stores `c`, or forgets the credentials when it is nil. New credentials
    /// replace the old ones in place, so a failed write keeps the old ones.
    @discardableResult
    static func save(_ c: Credentials?) -> Bool {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        guard let c else {
            let status = SecItemDelete(base as CFDictionary)
            return status == errSecSuccess || status == errSecItemNotFound
        }
        guard let data = try? JSONEncoder().encode(c) else { return false }
        let fields: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let status = SecItemUpdate(base as CFDictionary, fields as CFDictionary)
        guard status == errSecItemNotFound else { return status == errSecSuccess }
        return SecItemAdd(base.merging(fields) { $1 } as CFDictionary, nil) == errSecSuccess
    }
}
