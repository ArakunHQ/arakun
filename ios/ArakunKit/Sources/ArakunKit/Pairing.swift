import Foundation

/// The link Arakun's Phone tab shows (as a QR code) to pair a phone through the
/// relay: `https://<label>.<relay>/#p=<code>&s=<secret>`. The label is the
/// host's first part; the fragment holds the code the relay admits the phone
/// with and the secret it shows the computer inside the encrypted channel.
public struct PairingLink: Equatable, Sendable {
    public let host: String
    public let label: String
    public let code: String
    public let secret: String
    /// `ws` for a relay run by hand (`wrangler dev`), `wss` otherwise.
    public let secure: Bool

    public init?(_ text: String) {
        guard let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = url.host?.lowercased(), let fragment = url.fragment
        else { return nil }
        var fields: [String: String] = [:]
        for pair in fragment.split(separator: "&") {
            let kv = pair.split(separator: "=", maxSplits: 1).map(String.init)
            guard kv.count == 2 else { return nil }
            fields[kv[0]] = kv[1].removingPercentEncoding ?? kv[1]
        }
        guard let code = fields["p"], !code.isEmpty, let secret = fields["s"], !secret.isEmpty,
              let label = host.split(separator: ".").first.map(String.init), !label.isEmpty
        else { return nil }
        self.host = url.port.map { "\(host):\($0)" } ?? host
        self.label = label
        self.code = code
        self.secret = secret
        secure = scheme == "https"
    }
}

/// What the computer gave this phone when it paired, and where to find it.
public struct Credentials: Codable, Equatable, Sendable {
    public var host: String
    public var label: String
    public var secure: Bool
    public var id: String
    public var ticket: String
    public var token: String
    /// The computer's name, as it last said it.
    public var name: String

    public init(host: String, label: String, secure: Bool, id: String, ticket: String, token: String, name: String) {
        self.host = host
        self.label = label
        self.secure = secure
        self.id = id
        self.ticket = ticket
        self.token = token
        self.name = name
    }
}

/// Joins the `{part: {body, more}}` pieces of a message too big for one relay
/// frame: given each piece in turn, it gives back the whole message with the
/// last one and nil before that.
public struct Joiner {
    private var parts = Data()

    public init() {}

    public mutating func add(_ part: [String: Any]) -> Data? {
        parts.append(Base64URL.decode(part["body"] as? String ?? "") ?? Data())
        if part["more"] as? Bool == true { return nil }
        defer { parts = Data() }
        return parts
    }
}

/// Why a channel ended, from what the relay or the computer said.
public enum EndKind: Equatable, Sendable {
    /// The credentials are no longer good: pair again.
    case unpaired
    /// The pairing link did not work.
    case pairing
    /// The computer behind the address is not the one expected.
    case untrusted
    /// The relay has had enough for now.
    case limit
    /// Anything else: try again.
    case gone

    public static func of(reason: String, code: Int) -> EndKind {
        if code == 4401 || ["unauthorized", "revoked", "bad_ticket", "unpaired", "wrong_secret"].contains(reason) {
            return .unpaired
        }
        if ["no_pairing", "bad_code", "no_credentials"].contains(reason) { return .pairing }
        if reason == "untrusted" || reason == "bad_hello" { return .untrusted }
        if code == 4429 || reason.hasPrefix("limit:") { return .limit }
        return .gone
    }
}
