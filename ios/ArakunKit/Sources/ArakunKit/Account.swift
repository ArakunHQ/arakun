// The account client, as core/src/cloud.rs is the desktop's: GitHub's device
// flow, then the Arakun service (`cloud/`) that turns a GitHub token into a
// session, and the account's routes (`/me`, `/machines`, `/sessions`). The
// session goes in a header, never in a URL, and lives in the Keychain only:
// every request that carries it goes through `AccountNetwork.session`, which
// keeps nothing on disk.
import Foundation
import Security

/// Where the app signs in and reaches the relay.
public enum AccountHost {
    public static let base = URL(string: "https://app.arakun.dev")!
    /// The GitHub OAuth App `base` checks tokens against: the desktop's, in
    /// src/arakun_teams/api.rs. Another host names its own at `/auth/config`.
    public static let clientID = "Ov23lipgmMeATCa5aTr3"

    /// Whether a session may go to `url`. It rides in a header on every call,
    /// so plain http only to this machine (`wrangler dev`).
    public static func carriesSessions(_ url: URL) -> Bool {
        guard let host = url.host?.lowercased(), !host.isEmpty, url.user == nil, url.password == nil else { return false }
        switch url.scheme?.lowercased() {
        case "https", "wss": return true
        case "http", "ws": return ["localhost", "127.0.0.1", "::1", "[::1]"].contains(host)
        default: return false
        }
    }
}

/// The one `URLSession` for everything that carries the session or GitHub's
/// token: the account's routes, the device flow and the relay's socket.
/// `URLSession.shared` caches answers on disk together with their requests,
/// `Authorization` header included, and the session must stay in the Keychain.
public enum AccountNetwork {
    public static let session = URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)

    /// Ephemeral: no cache, cookie or credential store on disk. The cache is
    /// then turned off in memory too, since no answer here may be reused.
    static var configuration: URLSessionConfiguration {
        let c = URLSessionConfiguration.ephemeral
        c.requestCachePolicy = .reloadIgnoringLocalCacheData
        c.urlCache = nil
        c.httpCookieStorage = nil
        c.httpShouldSetCookies = false
        c.urlCredentialStorage = nil
        return c
    }
}

/// No route asked here redirects. A redirect followed would send the body
/// again, GitHub's token in it, to wherever the answer pointed.
private final class NoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest) async -> URLRequest? {
        nil
    }
}

/// The account's session. Its `description` hides the token, so printing a
/// value that holds one never shows it.
public struct SessionToken: Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    public let value: String

    public init(_ value: String) {
        self.value = value
    }

    public var description: String { "SessionToken(hidden)" }
    public var debugDescription: String { description }
    public var customMirror: Mirror { Mirror(self, children: [:]) }

    var bearer: String { "Bearer \(value)" }
}

/// Who is signed in, as `GET /me` answers.
public struct Me: Decodable, Equatable, Sendable {
    public let githubID: Int64
    public let login: String
    public let flags: [String]
    public let plan: String
    public var notices: [Notice]

    enum CodingKeys: String, CodingKey {
        case githubID = "github_id", login, flags, plan, notices
    }

    public init(githubID: Int64, login: String, flags: [String] = [], plan: String = "", notices: [Notice] = []) {
        self.githubID = githubID
        self.login = login
        self.flags = flags
        self.plan = plan
        self.notices = notices
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        githubID = try c.decode(Int64.self, forKey: .githubID)
        login = try c.decode(String.self, forKey: .login)
        flags = try c.decodeIfPresent([String].self, forKey: .flags) ?? []
        plan = try c.decodeIfPresent(String.self, forKey: .plan) ?? ""
        notices = try c.decodeIfPresent([Notice].self, forKey: .notices) ?? []
    }
}

/// A message from the operator to this account, until it is read.
public struct Notice: Decodable, Equatable, Sendable, Identifiable {
    public let id: String
    public let text: String
    public let url: String?

    public init(id: String, text: String, url: String? = nil) {
        self.id = id
        self.text = text
        self.url = url
    }

    /// The link, only when it is https, as on the phone page and Android.
    public var link: URL? {
        guard let url, let u = URL(string: url), u.scheme?.lowercased() == "https", u.host != nil else { return nil }
        return u
    }
}

/// One of the account's computers on the relay, as `GET /machines` lists it.
public struct Machine: Decodable, Equatable, Sendable, Identifiable {
    public let label: String
    /// Its public key, base64url, `pub` on the wire.
    public let pub: String
    public let name: String
    /// `app` or `server`.
    public let kind: String
    public let online: Bool
    /// Unix seconds.
    public let lastSeenAt: Double?

    enum CodingKeys: String, CodingKey {
        case label, pub, name, kind, online
        case lastSeenAt = "last_seen_at"
    }

    public init(label: String, pub: String, name: String, kind: String, online: Bool, lastSeenAt: Double?) {
        self.label = label
        self.pub = pub
        self.name = name
        self.kind = kind
        self.online = online
        self.lastSeenAt = lastSeenAt
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        label = try c.decode(String.self, forKey: .label)
        pub = try c.decode(String.self, forKey: .pub)
        name = try c.decode(String.self, forKey: .name)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "app"
        online = try c.decodeIfPresent(Bool.self, forKey: .online) ?? false
        lastSeenAt = try c.decodeIfPresent(Double.self, forKey: .lastSeenAt)
    }

    public var id: String { label }

    /// The key the handshake must name: the listed one, only when it derives
    /// the label asked for. Anything else in the list is not trusted.
    public var trustedKey: Data? {
        guard let key = Base64URL.decode(pub), key.count == 65, E2E.label(of: key) == label else { return nil }
        return key
    }
}

/// A signed-in device of the account, as `GET /sessions` lists it.
public struct SignedInDevice: Decodable, Equatable, Sendable, Identifiable {
    public let id: String
    public let name: String
    /// `app`, `server`, `phone`, `ios` or `android`.
    public let kind: String
    /// Unix seconds.
    public let createdAt: Double
    public let lastUsedAt: Double
    /// The session that asked: this device.
    public let current: Bool

    enum CodingKeys: String, CodingKey {
        case id, name, kind, current
        case createdAt = "created_at"
        case lastUsedAt = "last_used_at"
    }

    public init(id: String, name: String, kind: String, createdAt: Double, lastUsedAt: Double, current: Bool) {
        self.id = id
        self.name = name
        self.kind = kind
        self.createdAt = createdAt
        self.lastUsedAt = lastUsedAt
        self.current = current
    }
}

/// What `POST /auth/github` answers. `expires_at` is left out: the server
/// extends a session that is used, and a 401 says when one has ended.
public struct SignedIn: Decodable, Equatable, Sendable {
    public let session: SessionToken
    public let sessionID: String
    public let githubID: Int64
    public let login: String

    enum CodingKeys: String, CodingKey {
        case session, login
        case sessionID = "session_id"
        case githubID = "github_id"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        session = SessionToken(try c.decode(String.self, forKey: .session))
        sessionID = try c.decode(String.self, forKey: .sessionID)
        githubID = try c.decode(Int64.self, forKey: .githubID)
        login = try c.decode(String.self, forKey: .login)
    }
}

/// GitHub's answer to the first step of the device flow.
public struct DeviceCode: Equatable, Sendable {
    public let userCode: String
    public let verificationURI: String
    public let deviceCode: String
    /// Seconds between polls.
    public let interval: TimeInterval
    public let expiresIn: TimeInterval

    public init(userCode: String, verificationURI: String, deviceCode: String, interval: TimeInterval, expiresIn: TimeInterval) {
        self.userCode = userCode
        self.verificationURI = verificationURI
        self.deviceCode = deviceCode
        self.interval = interval
        self.expiresIn = expiresIn
    }
}

public enum AccountError: Error, Equatable, Sendable {
    /// 401: the session is not valid (any more).
    case signedOut
    /// The operator blocked the account, with the reason they gave.
    case blocked(String)
    /// The request got no answer.
    case unreachable(String)
    /// Any other answer than the one expected, with what the server said.
    case refused(String)

    /// As the person reads it.
    public var message: String {
        switch self {
        case .signedOut: "The sign-in is no longer valid."
        case .blocked(let reason): Refusal.blocked(reason)
        case .unreachable(let why): "Could not reach the server (\(why))."
        case .refused(let m) where m.isEmpty: "The server refused the request."
        case .refused(let m): "The server answered: \(m.hasSuffix(".") ? String(m.dropLast()) : m)."
        }
    }
}

/// One answer to a poll of GitHub's token endpoint: a token or an error code.
public struct TokenAnswer: Decodable, Equatable, Sendable {
    public var accessToken: String?
    public var error: String?
    public var errorDescription: String?
    /// With `slow_down`, the interval GitHub wants from now on, in seconds.
    public var interval: Double?

    enum CodingKeys: String, CodingKey {
        case error, interval
        case accessToken = "access_token"
        case errorDescription = "error_description"
    }

    public init(accessToken: String? = nil, error: String? = nil, errorDescription: String? = nil, interval: Double? = nil) {
        self.accessToken = accessToken
        self.error = error
        self.errorDescription = errorDescription
        self.interval = interval
    }
}

/// What to do after a poll of the device flow.
public enum Poll: Equatable, Sendable {
    /// Ask again after this many seconds, and keep that pace.
    case wait(TimeInterval)
    case token(String)
    /// The code ran out before it was entered.
    case expired
    /// The person turned the request down on GitHub.
    case denied
    case failed(String)

    /// The next step from GitHub's answer, polling every `interval` so far.
    public static func next(_ answer: TokenAnswer, interval: TimeInterval) -> Poll {
        if let token = answer.accessToken, !token.isEmpty { return .token(token) }
        switch answer.error {
        case "authorization_pending"?: return .wait(interval)
        // GitHub adds 5 s on each slow_down and says the new total; take
        // whichever is longer, so we never poll faster than it asks.
        case "slow_down"?: return .wait(max(interval + 5, answer.interval ?? 0))
        case "expired_token"?: return .expired
        case "access_denied"?: return .denied
        case let code?:
            let said = answer.errorDescription ?? ""
            return .failed(said.isEmpty ? "GitHub answered \(code)." : said)
        case nil: return .failed("GitHub sent neither a token nor an error.")
        }
    }
}

/// Calls to the account host and to GitHub. `send` does the HTTP, so tests
/// can answer in its place.
public struct AccountClient: Sendable {
    public typealias Send = @Sendable (URLRequest) async throws -> (Int, Data)

    public let base: URL
    private let send: Send

    static let deviceCodeURL = URL(string: "https://github.com/login/device/code")!
    static let tokenURL = URL(string: "https://github.com/login/oauth/access_token")!
    static let deviceGrant = "urn:ietf:params:oauth:grant-type:device_code"
    /// The service keeps the account's primary address, so it can reach people
    /// about their account.
    static let scope = "user:email"
    /// Where the person enters the code.
    public static let githubDevicePage = URL(string: "https://github.com/login/device")!

    public init(base: URL = AccountHost.base, send: @escaping Send = AccountClient.overNetwork) {
        self.base = base
        self.send = send
    }

    public static let overNetwork: Send = { request in
        let (data, response) = try await AccountNetwork.session.data(for: request)
        return ((response as? HTTPURLResponse)?.statusCode ?? 0, data)
    }

    // ---- GitHub's device flow ------------------------------------------------

    /// The OAuth App to ask GitHub with: the desktop's for the default host,
    /// else what the host names at `GET /auth/config`.
    public func clientID() async throws -> String {
        if base == AccountHost.base { return AccountHost.clientID }
        struct Config: Decodable {
            let github_client_id: String?
        }
        let (status, body) = try await call("GET", "/auth/config")
        let config: Config = try Self.decode(status, body)
        guard let id = config.github_client_id, !id.isEmpty else {
            throw AccountError.refused("sign-in is not set up on this server")
        }
        return id
    }

    /// Asks GitHub for a code the person enters at `verificationURI`.
    public func startDeviceFlow(clientID: String) async throws -> DeviceCode {
        struct Wire: Decodable {
            let device_code: String
            let user_code: String
            let verification_uri: String
            let expires_in: Double
            let interval: Double?
        }
        let (status, body) = try await github(Self.deviceCodeURL, ["client_id": clientID, "scope": Self.scope])
        guard status == 200 else { throw AccountError.refused(Self.githubError(status, body)) }
        guard let w = try? JSONDecoder().decode(Wire.self, from: body) else { throw Self.unreadable }
        // A zero interval would poll as fast as the loop can.
        return DeviceCode(userCode: w.user_code, verificationURI: w.verification_uri, deviceCode: w.device_code,
                          interval: max(w.interval ?? 5, 1), expiresIn: w.expires_in)
    }

    /// One poll of GitHub's token endpoint, polling every `interval` so far.
    public func pollOnce(clientID: String, deviceCode: String, interval: TimeInterval) async throws -> Poll {
        let (status, body) = try await github(Self.tokenURL, [
            "client_id": clientID, "device_code": deviceCode, "grant_type": Self.deviceGrant,
        ])
        return try Self.pollAnswer(status, body, interval: interval)
    }

    /// Polls until GitHub gives a token, the code runs out, or the task is
    /// cancelled (the person pressed Cancel). Waits first: the person needs
    /// time to enter the code anyway.
    public func waitForToken(clientID: String, code: DeviceCode) async throws -> String {
        var pace = code.interval
        let deadline = Date().addingTimeInterval(code.expiresIn)
        while true {
            try await Task.sleep(for: .seconds(pace))
            guard Date() < deadline else { throw AccountError.refused(Self.expiredSentence) }
            let polled: Poll
            do {
                polled = try await pollOnce(clientID: clientID, deviceCode: code.deviceCode, interval: pace)
            } catch AccountError.unreachable(_) {
                // The phone lost its network for a moment: the code is still good.
                continue
            }
            switch polled {
            case .wait(let next): pace = next
            case .token(let token): return token
            case .expired: throw AccountError.refused(Self.expiredSentence)
            case .denied: throw AccountError.refused("the sign-in was turned down on GitHub")
            case .failed(let why): throw AccountError.refused(why)
            }
        }
    }

    static let expiredSentence = "the code ran out before it was entered on GitHub"

    /// GitHub's own trouble leaves the code valid: ask again at the same pace.
    static func pollAnswer(_ status: Int, _ body: Data, interval: TimeInterval) throws -> Poll {
        if status >= 500 { return .wait(interval) }
        let answer = try? JSONDecoder().decode(TokenAnswer.self, from: body)
        if let answer, status == 200 || answer.error != nil { return Poll.next(answer, interval: interval) }
        // A 200 that is not JSON (a proxy's page, say) is not an answer.
        if status == 200 { throw unreadable }
        throw AccountError.refused(githubError(status, body))
    }

    // ---- the account host ----------------------------------------------------

    /// Trades the GitHub token for a session. The server asks GitHub who the
    /// token belongs to and does not keep it. `name` and `kind` say what the
    /// session is on, for the list of signed-in devices.
    public func signIn(githubToken: String, name: String, kind: String = "ios") async throws -> SignedIn {
        var fields = ["access_token": githubToken, "kind": kind]
        // The server takes 1 to 100 characters, and the login when there is none.
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { fields["name"] = String(trimmed.prefix(100)) }
        let body = try JSONSerialization.data(withJSONObject: fields)
        let (status, answer) = try await call("POST", "/auth/github", body: body)
        do {
            return try Self.decode(status, answer)
        } catch AccountError.signedOut {
            // No session yet, so a 401 is about the GitHub token, not a stale sign-in.
            throw AccountError.refused("GitHub did not accept the sign-in. Try again.")
        }
    }

    public func me(session: SessionToken) async throws -> Me {
        let (status, body) = try await call("GET", "/me", session: session)
        return try Self.decode(status, body)
    }

    public func machines(session: SessionToken) async throws -> [Machine] {
        let (status, body) = try await call("GET", "/machines", session: session)
        return try Self.decode(status, body)
    }

    public func sessions(session: SessionToken) async throws -> [SignedInDevice] {
        let (status, body) = try await call("GET", "/sessions", session: session)
        return try Self.decode(status, body)
    }

    /// Ends one of the account's sessions, this one or another.
    public func signOutSession(session: SessionToken, id: String) async throws {
        let part = try Self.pathPart(id)
        let (status, body) = try await call("DELETE", "/sessions/\(part)", session: session)
        // 404: it ran out or was signed out elsewhere since the list loaded,
        // which is what was asked for. The phone page counts it as done too.
        guard (200...299).contains(status) || status == 404 else { throw Self.refusal(status, body) }
    }

    /// Marks a notice read, so `/me` stops listing it.
    public func readNotice(session: SessionToken, id: String) async throws {
        let part = try Self.pathPart(id)
        try await act("POST", "/notices/\(part)/read", session: session)
    }

    /// Ends the session on the server.
    public func logout(session: SessionToken) async throws {
        try await act("POST", "/auth/logout", session: session)
    }

    // ---- plumbing --------------------------------------------------------------

    /// A label or id to put in a path. These come from the server, so anything
    /// that could reach another route (`/`, `..`, `%`) is refused unsent.
    static func pathPart(_ part: String) throws -> String {
        let ok = (1...128).contains(part.utf8.count) && part.utf8.allSatisfy { b in
            (b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5A) || (b >= 0x61 && b <= 0x7A) || b == 0x2D || b == 0x5F
        }
        guard ok else { throw AccountError.refused("\"\(part)\" is not a valid name") }
        return part
    }

    /// One request to the account host. Plain http is refused unless the host
    /// is this machine: the session and GitHub's token go in it.
    func call(_ method: String, _ path: String, session: SessionToken? = nil, body: Data? = nil) async throws -> (Int, Data) {
        guard AccountHost.carriesSessions(base) else {
            throw AccountError.refused("\(base.absoluteString) must start with https:// (http:// only for localhost)")
        }
        let root = base.absoluteString.hasSuffix("/") ? String(base.absoluteString.dropLast()) : base.absoluteString
        guard let url = URL(string: root + path) else { throw AccountError.refused("bad address") }
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let session { request.setValue(session.bearer, forHTTPHeaderField: "Authorization") }
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = body
        }
        return try await run(request)
    }

    /// A form post to GitHub, which answers JSON when asked to.
    private func github(_ url: URL, _ form: [String: String]) async throws -> (Int, Data) {
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data(Self.formEncoded(form).utf8)
        return try await run(request)
    }

    private func run(_ request: URLRequest) async throws -> (Int, Data) {
        do {
            return try await send(request)
        } catch {
            if Task.isCancelled { throw CancellationError() }
            throw AccountError.unreachable(error.localizedDescription)
        }
    }

    /// A call that answers nothing but its status.
    private func act(_ method: String, _ path: String, session: SessionToken) async throws {
        let (status, body) = try await call(method, path, session: session)
        guard (200...299).contains(status) else { throw Self.refusal(status, body) }
    }

    static func formEncoded(_ form: [String: String]) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return form.sorted { $0.key < $1.key }
            .map { "\($0.key)=\($0.value.addingPercentEncoding(withAllowedCharacters: allowed) ?? "")" }
            .joined(separator: "&")
    }

    static func decode<T: Decodable>(_ status: Int, _ body: Data) throws -> T {
        guard (200...299).contains(status) else { throw refusal(status, body) }
        guard let value = try? JSONDecoder().decode(T.self, from: body) else { throw unreadable }
        return value
    }

    static let unreadable = AccountError.refused("an answer that could not be read")

    /// The server's errors are `{"error": "..."}`; a blocked account's also
    /// carries the operator's `reason`.
    static func refusal(_ status: Int, _ body: Data) -> AccountError {
        let obj = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        let said = obj?["error"] as? String ?? ""
        switch status {
        case 401: return .signedOut
        case 403 where said == "blocked": return .blocked(obj?["reason"] as? String ?? "")
        case 429: return .refused("too many requests; try again in a minute")
        default: return .refused(said.isEmpty ? "HTTP \(status)" : said)
        }
    }

    /// GitHub's errors are `{"error": "...", "error_description": "..."}`.
    static func githubError(_ status: Int, _ body: Data) -> String {
        let obj = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        return obj?["error_description"] as? String ?? obj?["error"] as? String ?? "HTTP \(status)"
    }
}

/// The account's session on this device, and the one way it is written:
/// through `store`. Signing in keeps it there, signing out and any 401 clear
/// it, so no other place ever holds it.
@MainActor
public final class AccountSession {
    public let client: AccountClient
    private let store: any SessionStore
    public private(set) var token: SessionToken?

    public init(client: AccountClient = AccountClient(), store: any SessionStore) {
        self.client = client
        self.store = store
        token = store.load()
    }

    /// Trades GitHub's token for a session and keeps it. A session that
    /// cannot be kept is ended on the server, since nobody would hold it.
    @discardableResult
    public func signIn(githubToken: String, name: String, kind: String = "ios") async throws -> SignedIn {
        let signed = try await client.signIn(githubToken: githubToken, name: name, kind: kind)
        guard store.save(signed.session) else {
            try? await client.logout(session: signed.session)
            throw AccountError.refused("could not keep the sign-in in the Keychain")
        }
        token = signed.session
        return signed
    }

    /// One account call with the session. A 401 forgets the session here.
    public func call<T: Sendable>(_ body: @Sendable (AccountClient, SessionToken) async throws -> T) async throws -> T {
        guard let token else { throw AccountError.signedOut }
        do {
            return try await body(client, token)
        } catch AccountError.signedOut {
            if self.token == token { forget() }
            throw AccountError.signedOut
        }
    }

    /// Whether the session still stands, asked of the account when the relay
    /// turned it down at its socket. The relay's word alone never ends a
    /// session: a header lost on the way to it would read the same. Only the
    /// account's own 401 does, and that forgets the session here.
    public func stillStands() async -> SessionCheck {
        do {
            _ = try await call { c, t in try await c.me(session: t) }
            return .stands
        } catch AccountError.signedOut {
            return .over
        } catch {
            return .unknown
        }
    }

    /// Signs this device out: forgotten here at once, then ended on the
    /// server. The task says whether the server heard it; a 401 counts, since
    /// the session is over there too.
    @discardableResult
    public func signOut() -> Task<Bool, Never> {
        guard let token else { return Task { true } }
        forget()
        let client = client
        return Task {
            do {
                try await client.logout(session: token)
                return true
            } catch AccountError.signedOut {
                return true
            } catch {
                return false
            }
        }
    }

    /// Forgets the session here only.
    public func forget() {
        store.save(nil)
        token = nil
    }
}

/// What the account answered about a session the relay turned down.
public enum SessionCheck: Equatable, Sendable {
    /// The account takes it: only the relay did not.
    case stands
    /// The account says 401 too: the session is over.
    case over
    /// No answer either way (no network, the host in trouble): ask again later.
    case unknown
}

/// Where the session is kept. The app's is the Keychain; tests pass their own.
public protocol SessionStore: Sendable {
    func load() -> SessionToken?
    /// Keeps `token`, or forgets the session when it is nil. Whether it worked.
    @discardableResult
    func save(_ token: SessionToken?) -> Bool
}

/// The session in the Keychain, on this device only: what lets anyone holding
/// it into the account's computers. Never in UserDefaults or a log.
public struct KeychainSessionStore: SessionStore {
    let service: String
    let account: String

    public init(service: String = "com.arakun.ios.account", account: String = "session") {
        self.service = service
        self.account = account
    }

    public func load() -> SessionToken? {
        guard let data = KeychainItem(service: service, account: account).read(),
              let value = String(data: data, encoding: .utf8), !value.isEmpty
        else { return nil }
        return SessionToken(value)
    }

    @discardableResult
    public func save(_ token: SessionToken?) -> Bool {
        KeychainItem(service: service, account: account).write(token.map { Data($0.value.utf8) })
    }
}

/// One generic password in the Keychain, readable after the first unlock and
/// never synced or restored to another device, as the pairing credentials were.
public struct KeychainItem: Sendable {
    public let service: String
    public let account: String

    public init(service: String, account: String) {
        self.service = service
        self.account = account
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    public func read() -> Data? {
        var q = query
        q[kSecReturnData as String] = true
        var out: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess else { return nil }
        return out as? Data
    }

    /// Stores `data`, or deletes the item when it is nil. New data replaces the
    /// old in place, so a failed write keeps the old.
    @discardableResult
    public func write(_ data: Data?) -> Bool {
        guard let data else {
            let status = SecItemDelete(query as CFDictionary)
            return status == errSecSuccess || status == errSecItemNotFound
        }
        let fields: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let status = SecItemUpdate(query as CFDictionary, fields as CFDictionary)
        guard status == errSecItemNotFound else { return status == errSecSuccess }
        return SecItemAdd(query.merging(fields) { $1 } as CFDictionary, nil) == errSecSuccess
    }
}
