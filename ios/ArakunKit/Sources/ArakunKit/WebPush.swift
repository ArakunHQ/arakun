// Notices from the computer, as it sends them to the phone page's browser:
// Web Push, encrypted to a key pair and auth secret this phone made (RFC 8291,
// aes128gcm from RFC 8188). The cloud Worker takes the sealed body for APNs
// and passes it on unread in the alert's `e`, so only this phone can read
// what a notice says. core/src/phone/push.rs is the computer's half.
import CryptoKit
import Foundation

/// This phone's push keys: the P-256 key the computer encrypts notices to,
/// and the auth secret that goes with it. Made once and kept in the Keychain,
/// where the notification service extension reads them too.
public struct PushKeys: Equatable, Sendable {
    /// The private key's raw 32 bytes.
    let secret: Data
    /// 16 random bytes the computer mixes into every notice's keys.
    public let auth: Data

    public init?(secret: Data, auth: Data) {
        guard auth.count == 16, (try? P256.KeyAgreement.PrivateKey(rawRepresentation: secret)) != nil else { return nil }
        self.secret = Data(secret)
        self.auth = Data(auth)
    }

    /// New keys. CryptoKit's generator stops the app rather than hand back
    /// bytes that are not random.
    public static func generate() -> PushKeys {
        let key = P256.KeyAgreement.PrivateKey()
        let auth = SymmetricKey(size: SymmetricKeySize(bitCount: 128)).withUnsafeBytes { Data($0) }
        return PushKeys(secret: key.rawRepresentation, auth: auth)!
    }

    var privateKey: P256.KeyAgreement.PrivateKey {
        // `init` checked the bytes make a key.
        try! P256.KeyAgreement.PrivateKey(rawRepresentation: secret)
    }

    /// The public key, uncompressed (65 bytes): the subscription's `p256dh`.
    public var publicKey: Data { privateKey.publicKey.x963Representation }

    /// How the Keychain keeps them: the private key, then the auth secret.
    public var stored: Data { secret + auth }

    public init?(stored: Data) {
        let d = Data(stored)
        guard d.count == 48 else { return nil }
        self.init(secret: d.prefix(32), auth: d.suffix(16))
    }

    /// The subscription the computer's `POST /push` takes, as a browser's
    /// `PushSubscription.toJSON()` writes it.
    public func subscription(endpoint: URL) -> [String: Any] {
        [
            "endpoint": endpoint.absoluteString,
            "keys": ["p256dh": Base64URL.encode(publicKey), "auth": Base64URL.encode(auth)],
        ]
    }
}

/// The push keys in the Keychain, on this device only, in the access group
/// the app shares with its notification service extension.
public struct PushKeyStore: Sendable {
    let item: KeychainItem

    public init(accessGroup: String?) {
        item = KeychainItem(service: "com.arakun.ios.push", account: "keys", accessGroup: accessGroup)
    }

    public func load() -> PushKeys? {
        item.read().flatMap(PushKeys.init(stored:))
    }

    /// The keys kept, or new ones kept now. Nil when the Keychain would not keep them.
    public func loadOrMake() -> PushKeys? {
        if let keys = load() { return keys }
        let keys = PushKeys.generate()
        return item.write(keys.stored) ? keys : nil
    }

    /// Without the keys no notice sent to the old ones can be read.
    @discardableResult
    public func forget() -> Bool {
        item.write(nil)
    }
}

/// Where the cloud Worker takes this phone's notices for APNs.
public enum PushEndpoint {
    /// `<base>/push/v1/apns/<hex token>`, or `apns-sandbox` for a development
    /// build, whose token comes from APNs's sandbox. Nil unless `base` is
    /// https: the computer takes no other subscription.
    public static func url(base: URL, token: Data, sandbox: Bool) -> URL? {
        guard !token.isEmpty, base.scheme?.lowercased() == "https", base.host?.isEmpty == false else { return nil }
        let hex = token.map { String(format: "%02x", $0) }.joined()
        return base.appending(path: "push/v1/\(sandbox ? "apns-sandbox" : "apns")/\(hex)")
    }
}

/// What a notice says once decrypted, as `notify_phones` in
/// src/shell/phone.rs writes it.
public struct PushNotice: Equatable, Sendable {
    /// `done`, `input` or `error`; a newer computer may send another.
    public let kind: String
    /// The session it is about, to open on a tap.
    public let session: String
    public let title: String
    public let body: String

    public init(kind: String, session: String, title: String, body: String) {
        self.kind = kind
        self.session = session
        self.title = title
        self.body = body
    }

    public init?(json data: Data) {
        guard let j = JSON.parse(data), case .object = j, let session = j["session"].string, !session.isEmpty else {
            return nil
        }
        self.init(kind: j["kind"].str, session: session, title: j["title"].str, body: j["body"].str)
    }
}

/// Which notices this phone wants from a computer. Each computer keeps its
/// own choice, everything until the phone says otherwise.
public struct NoticeChoice: Equatable, Sendable {
    public var done: Bool
    public var input: Bool
    public var error: Bool

    public init(done: Bool = true, input: Bool = true, error: Bool = true) {
        self.done = done
        self.input = input
        self.error = error
    }

    /// The computer's `notices`; what it leaves out stays on.
    public init(_ j: JSON) {
        func on(_ key: String) -> Bool {
            if case .bool(let b) = j[key] { return b }
            return true
        }
        self.init(done: on("done"), input: on("input"), error: on("error"))
    }

    public var json: [String: Any] { ["done": done, "input": input, "error": error] }

    public subscript(_ kind: NoticeKind) -> Bool {
        get {
            switch kind {
            case .done: done
            case .input: input
            case .error: error
            }
        }
        set {
            switch kind {
            case .done: done = newValue
            case .input: input = newValue
            case .error: error = newValue
            }
        }
    }
}

/// The kinds of notice a phone can turn off one by one.
public enum NoticeKind: String, Sendable, CaseIterable {
    /// A turn finished.
    case done
    /// The agent needs a permission or an answer.
    case input
    /// A turn failed.
    case error
}

/// What the computer says about this phone's notices, in `GET /me` and in
/// the answer to `POST /push`.
public struct PushState: Equatable, Sendable {
    /// Whether the computer holds a subscription for this phone.
    public let notifying: Bool
    public let notices: NoticeChoice

    public init(notifying: Bool, notices: NoticeChoice) {
        self.notifying = notifying
        self.notices = notices
    }

    public init?(_ j: JSON) {
        guard case .object = j else { return nil }
        notifying = j["notifying"].bool
        notices = NoticeChoice(j["notices"])
    }
}

public enum WebPush {
    public enum Failure: Error, Equatable, CustomStringConvertible {
        case malformed(String)
        case doesNotDecrypt

        public var description: String {
            switch self {
            case .malformed(let why): "malformed notice: \(why)"
            case .doesNotDecrypt: "notice does not decrypt"
            }
        }
    }

    /// The plaintext of an aes128gcm body with one record (RFC 8188),
    /// encrypted to `keys` as RFC 8291 says. The computer sends no more than
    /// one record, and its padding is a 2 then zeros.
    public static func decrypt(_ body: Data, keys: PushKeys) throws -> Data {
        let b = Data(body)
        // The header: salt (16), record size (4), key id length (1), then the
        // key id, which RFC 8291 makes the sender's public key.
        guard b.count >= 21, let rs = b.readBigEndian(UInt32.self, at: 16) else { throw Failure.malformed("header") }
        guard b[20] == 65 else { throw Failure.malformed("key id") }
        let start = 21 + 65
        guard b.count >= start + 17 else { throw Failure.malformed("short") }
        guard b.count - start <= Int(rs) else { throw Failure.malformed("more than one record") }
        let salt = b.subdata(in: 0..<16)
        let senderKey = b.subdata(in: 21..<start)
        let record = b.subdata(in: start..<b.count)

        guard let sender = try? P256.KeyAgreement.PublicKey(x963Representation: senderKey),
              let shared = try? keys.privateKey.sharedSecretFromKeyAgreement(with: sender)
        else { throw Failure.malformed("sender key") }
        let ecdh = shared.withUnsafeBytes { Data($0) }

        // RFC 8291 §3.3 and §3.4: the auth secret salts the shared secret,
        // then the message's salt makes its key and nonce.
        var info = Data("WebPush: info\0".utf8)
        info.append(keys.publicKey)
        info.append(senderKey)
        let ikm = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: ecdh), salt: keys.auth, info: info, outputByteCount: 32)
        let cek = HKDF<SHA256>.deriveKey(inputKeyMaterial: ikm, salt: salt, info: Data("Content-Encoding: aes128gcm\0".utf8), outputByteCount: 16)
        let nonce = HKDF<SHA256>.deriveKey(inputKeyMaterial: ikm, salt: salt, info: Data("Content-Encoding: nonce\0".utf8), outputByteCount: 12)
            .withUnsafeBytes { Data($0) }

        let plain: Data
        do {
            let box = try AES.GCM.SealedBox(nonce: AES.GCM.Nonce(data: nonce), ciphertext: record.dropLast(16), tag: record.suffix(16))
            plain = try AES.GCM.open(box, using: cek)
        } catch {
            throw Failure.doesNotDecrypt
        }
        // The last record's delimiter is a 2, and only zeros may follow it.
        guard let end = plain.lastIndex(where: { $0 != 0 }), plain[end] == 2 else { throw Failure.malformed("padding") }
        return plain.subdata(in: 0..<end)
    }

    /// The notice in an APNs alert's `e`: the whole sealed body, in base64url.
    public static func open(_ e: String, keys: PushKeys) throws -> PushNotice {
        let trimmed = e.hasSuffix("=") ? String(e.prefix { $0 != "=" }) : e
        guard let body = Base64URL.decode(trimmed) else { throw Failure.malformed("e") }
        guard let notice = PushNotice(json: try decrypt(body, keys: keys)) else { throw Failure.malformed("notice") }
        return notice
    }
}
