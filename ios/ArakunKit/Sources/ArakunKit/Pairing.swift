import Foundation

/// Text the person scanned or pasted to pair: what it is, before anything is
/// sent. Through the relay the app signs in instead, so a pairing link from
/// before sign-in (`https://<label>.<relay>/#p=<code>&s=<secret>`) is refused
/// with a sentence. A link to the computer's own page (`http://<address>/?p=<code>`,
/// on this network or Tailscale) still pairs, in the browser.
public enum PairingText: Equatable, Sendable {
    case relayLink
    case direct(URL)
    case unknown

    public static let relayLinkSentence =
        "Relay links are no longer used. Sign in to reach your computers."

    /// On the sign-in screen after an update forgot a pairing made by link.
    public static let pairedByLinkSentence =
        "Since 0.19 the relay no longer takes pairing links, so this phone's pairing was removed. Sign in with the same GitHub account as Arakun on your computer, after updating it there. To use no account, pair below over Tailscale or the same Wi-Fi."

    public init(_ text: String) {
        guard let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = url.host, !host.isEmpty
        else {
            self = .unknown
            return
        }
        let fragment = url.fragment ?? ""
        let fields = Set(fragment.split(separator: "&").compactMap { pair in pair.split(separator: "=", maxSplits: 1).first.map { String($0) } })
        if fields.contains("p") && fields.contains("s") {
            self = .relayLink
            return
        }
        let code = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "p" })?.value ?? ""
        self = code.isEmpty ? .unknown : .direct(url)
    }
}

/// Joins the `{part: {body, more}}` pieces of a message too big for one relay
/// frame: given each piece in turn, it gives back the whole message with the
/// last one and nil before that.
/// A message over `most` bytes is dropped, as core/src/phone/relay.rs's
/// Joiner does, so the computer cannot grow it without end.
public struct Joiner {
    private var parts = Data()
    private var tooBig = false
    let most: Int

    public init(most: Int = 64 * 1024 * 1024) {
        self.most = most
    }

    public mutating func add(_ part: [String: Any]) -> Data? {
        let piece = Base64URL.decode(part["body"] as? String ?? "") ?? Data()
        let more = part["more"] as? Bool == true
        if tooBig || parts.count + piece.count > most {
            // Skip the rest of this message; the next one starts clean.
            tooBig = more
            parts = Data()
            return nil
        }
        parts.append(piece)
        if more { return nil }
        defer { parts = Data() }
        return parts
    }
}

/// Why a channel ended, from what the relay or the computer said.
public enum EndKind: Equatable, Sendable {
    /// The session is gone: sign in again.
    case signedOut
    /// The relay will not let this device in: `reason` as it said it, and
    /// `sentence` as the person reads it.
    case refused(reason: String, sentence: String)
    /// The computer behind the label is not the one the account lists.
    case untrusted
    /// The relay has had enough for now.
    case limit
    /// Anything else: try again.
    case gone

    /// `message` is the operator's text, for `disabled`; `accountHost` is
    /// where to sign in, for `update`.
    public static func of(reason: String, code: Int, message: String? = nil,
                          accountHost: String = AccountHost.base.host ?? "app.arakun.dev") -> EndKind {
        if reason == "signed_out" || (code == 4401 && reason.isEmpty) { return .signedOut }
        if Refusal.reasons.contains(reason) {
            return .refused(reason: reason, sentence: Refusal.sentence(reason, message: message, accountHost: accountHost))
        }
        if reason == "untrusted" || reason == "bad_hello" { return .untrusted }
        if code == 4429 || reason.hasPrefix("limit:") { return .limit }
        return .gone
    }
}

/// How long to stay away after the relay said a limit was reached. It answers
/// the same until the limit lifts, so dialing again at once only repeats it.
public enum LimitWait {
    static let first: TimeInterval = 120
    static let longest: TimeInterval = 1800
    static let day: TimeInterval = 86_400

    /// Which limit, from what the link said: `daily` from the relay's own
    /// message, `limit:daily` from the reason it closed with.
    public static func which(_ detail: String) -> String {
        detail.hasPrefix("limit:") ? String(detail.dropFirst("limit:".count)) : detail
    }

    /// Seconds until the next try, after `tries` refusals in a row before
    /// this one. The day's count starts over at midnight UTC, so nothing is
    /// tried before then, and a minute more for a clock that runs ahead. Any
    /// other limit: two minutes, doubling to half an hour.
    public static func seconds(which: String, tries: Int, now: Date = Date()) -> TimeInterval {
        if which == "daily" {
            let t = now.timeIntervalSince1970
            return (t / day).rounded(.down) * day + day - t + 60
        }
        var wait = first
        for _ in 0..<max(0, min(tries, 8)) { wait = min(wait * 2, longest) }
        return wait
    }
}

/// What the relay's refusals say, in the phone page's words (phone/relay.js).
public enum Refusal {
    /// `unpaired`: the computer's user removed this device there, so it is
    /// not dialed again.
    static let reasons: Set<String> = ["blocked", "not_yours", "no_machine", "update", "disabled", "unpaired"]

    public static func sentence(_ reason: String, message: String? = nil, accountHost: String) -> String {
        switch reason {
        case "blocked": blocked("")
        case "not_yours": "That computer is signed in to another GitHub account."
        case "no_machine": "That computer is no longer on your account. Sign in again in Arakun on it."
        case "unpaired": "That computer removed this phone. Pick it again to connect."
        case "update": "Update Arakun on the computer and sign in at \(accountHost)."
        case "disabled": (message ?? "").isEmpty ? "The relay is closed right now." : message ?? ""
        default: "The relay closed the connection."
        }
    }

    /// A blocked account, with the operator's reason when there is one.
    public static func blocked(_ reason: String) -> String {
        "This GitHub account is blocked from the relay." + (reason.isEmpty ? "" : " \(reason)")
    }
}
