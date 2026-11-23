import Foundation
import Testing
@testable import ArakunKit

/// Plays the account host: answers every request with `status` and `body`,
/// and keeps what was asked.
private final class FakeHost: @unchecked Sendable {
    private let lock = NSLock()
    private var asked: [URLRequest] = []
    let status: Int
    let body: String

    init(_ status: Int = 200, _ body: String = "{}") {
        self.status = status
        self.body = body
    }

    var requests: [URLRequest] { lock.withLock { asked } }

    var client: AccountClient { at(AccountHost.base) }

    func at(_ base: URL) -> AccountClient {
        AccountClient(base: base) { request in
            self.lock.withLock { self.asked.append(request) }
            return (self.status, Data(self.body.utf8))
        }
    }
}

/// A fixed key and the label it derives (phone/test/vectors/label.json).
private let vectorKey = Data([
    0x04, 0x29, 0xc2, 0x08, 0x5b, 0xf0, 0x6a, 0xb5, 0xb9, 0xd6, 0x84, 0x97, 0x45, 0x8f, 0xa7, 0x62,
    0xaf, 0xf9, 0x2c, 0x26, 0xc6, 0xbb, 0xc6, 0x8f, 0xec, 0xc1, 0x7f, 0x9c, 0x0e, 0xf6, 0x82, 0x42,
    0xfb, 0x4f, 0x49, 0x97, 0x0f, 0xb3, 0x1b, 0x57, 0xb8, 0xc4, 0x3e, 0xf1, 0x47, 0x7c, 0xd3, 0x89,
    0x12, 0xd1, 0x4b, 0xae, 0xbd, 0x14, 0x95, 0xf9, 0x3e, 0xb0, 0x96, 0x12, 0xcd, 0x9f, 0x97, 0xc3,
    0xe2,
])
private let vectorLabel = "5pmbdp76s6bol6ax"

private let token = SessionToken("sess_9f8e7d6c5b4a39281706f5e4d3c2b1a0")

struct AccountTests {
    @Test func theVectorKeyDerivesItsLabel() {
        #expect(E2E.label(of: vectorKey) == vectorLabel)
    }

    @Test func readsMe() async throws {
        let host = FakeHost(200, #"""
        {"github_id": 583231, "login": "octocat", "flags": ["beta"], "plan": "free",
         "notices": [{"id": "n1", "text": "The relay moves tonight.", "url": "https://arakun.dev/news"}, {"id": "n2", "text": "Hi", "url": null}],
         "new_field": true}
        """#)
        let me = try await host.client.me(session: token)
        #expect(me.githubID == 583231)
        #expect(me.login == "octocat")
        #expect(me.flags == ["beta"])
        #expect(me.plan == "free")
        #expect(me.notices == [Notice(id: "n1", text: "The relay moves tonight.", url: "https://arakun.dev/news"), Notice(id: "n2", text: "Hi")])
        #expect(me.notices[0].link == URL(string: "https://arakun.dev/news"))
        #expect(me.notices[1].link == nil)
        // Only https becomes a link.
        for bad in ["http://arakun.dev/news", "javascript:alert(1)", "https:nohost", "ftp://arakun.dev/"] {
            #expect(Notice(id: "n", text: "t", url: bad).link == nil, "\(bad)")
        }

        let r = try #require(host.requests.first)
        #expect(r.httpMethod == "GET")
        #expect(r.url?.absoluteString == "https://app.arakun.dev/me")
        #expect(r.value(forHTTPHeaderField: "Authorization") == "Bearer \(token.value)")
    }

    @Test func readsMeWithoutTheOptionalFields() async throws {
        let me = try await FakeHost(200, #"{"github_id": 1, "login": "a"}"#).client.me(session: token)
        #expect(me == Me(githubID: 1, login: "a"))
    }

    @Test func readsMachines() async throws {
        let host = FakeHost(200, """
        [{"label": "\(vectorLabel)", "pub": "\(Base64URL.encode(vectorKey))", "name": "studio", "kind": "app", "online": true, "last_seen_at": 1763856000},
         {"label": "aaaaaaaaaaaaaaaa", "pub": "\(Base64URL.encode(vectorKey))", "name": "box", "kind": "server", "online": false, "last_seen_at": null}]
        """)
        let list = try await host.client.machines(session: token)
        #expect(list.count == 2)
        #expect(list[0] == Machine(label: vectorLabel, pub: Base64URL.encode(vectorKey), name: "studio", kind: "app", online: true, lastSeenAt: 1763856000))
        #expect(list[0].trustedKey == vectorKey)
        // A key that does not derive the label it is listed under is not trusted.
        #expect(list[1].trustedKey == nil)
        #expect(list[1].lastSeenAt == nil)
        #expect(host.requests.first?.url?.path == "/machines")
    }

    @Test func readsSignedInDevices() async throws {
        let host = FakeHost(200, #"""
        [{"id": "a1", "name": "Rik's iPhone", "kind": "ios", "created_at": 1763000000, "last_used_at": 1763856000, "current": true}]
        """#)
        let list = try await host.client.sessions(session: token)
        #expect(list == [SignedInDevice(id: "a1", name: "Rik's iPhone", kind: "ios", createdAt: 1763000000, lastUsedAt: 1763856000, current: true)])
    }

    @Test func a401IsSignedOut() async {
        await #expect(throws: AccountError.signedOut) {
            try await FakeHost(401, #"{"error": "not signed in"}"#).client.machines(session: token)
        }
    }

    @Test func aBlockedAccountSaysWhy() async {
        await #expect(throws: AccountError.blocked("Too many sign-ins.")) {
            try await FakeHost(403, #"{"error": "blocked", "reason": "Too many sign-ins."}"#).client.me(session: token)
        }
        #expect(AccountError.blocked("Too many sign-ins.").message == "This GitHub account is blocked from the relay. Too many sign-ins.")
    }

    @Test func otherAnswersAreRefusedWithTheServersWords() async {
        await #expect(throws: AccountError.refused("no such machine")) {
            try await FakeHost(404, #"{"error": "no such machine"}"#).client.me(session: token)
        }
        await #expect(throws: AccountError.refused("HTTP 502")) {
            try await FakeHost(502, "<html>").client.me(session: token)
        }
    }

    @Test func signInSendsTheNameAndKind() async throws {
        let host = FakeHost(200, #"""
        {"session": "\#(token.value)", "session_id": "a1", "expires_at": 1, "github_id": 7, "login": "octocat", "flags": [], "plan": "free"}
        """#)
        let signed = try await host.client.signIn(githubToken: "gho_abc", name: "Rik's iPhone", kind: "ios")
        #expect(signed.session == token)
        #expect(signed.sessionID == "a1")
        #expect(signed.login == "octocat")
        let r = try #require(host.requests.first)
        #expect(r.httpMethod == "POST")
        #expect(r.url?.absoluteString == "https://app.arakun.dev/auth/github")
        #expect(r.value(forHTTPHeaderField: "Authorization") == nil)
        let sent = try JSONSerialization.jsonObject(with: try #require(r.httpBody)) as? [String: String]
        #expect(sent == ["access_token": "gho_abc", "name": "Rik's iPhone", "kind": "ios"])
    }

    @Test func aTokenGitHubDidNotAcceptIsNotAStaleSession() async {
        await #expect(throws: AccountError.refused("GitHub did not accept the sign-in. Try again.")) {
            try await FakeHost(401, #"{"error": "invalid token"}"#).client.signIn(githubToken: "gho_x", name: "n")
        }
    }

    @Test func theRoutesThatAnswerNothing() async throws {
        let host = FakeHost(204, "")
        try await host.client.signOutSession(session: token, id: "a1")
        try await host.client.readNotice(session: token, id: "n_1")
        try await host.client.logout(session: token)
        #expect(host.requests.map { "\($0.httpMethod ?? "") \($0.url?.path ?? "")" } == [
            "DELETE /sessions/a1", "POST /notices/n_1/read", "POST /auth/logout",
        ])
        #expect(host.requests.allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == "Bearer \(token.value)" })
    }

    @Test func aDeviceThatIsAlreadyGoneCountsAsSignedOut() async throws {
        try await FakeHost(404, #"{"error": "not found"}"#).client.signOutSession(session: token, id: "a1")
        await #expect(throws: AccountError.refused("HTTP 500")) {
            try await FakeHost(500, "").client.signOutSession(session: token, id: "a1")
        }
        await #expect(throws: AccountError.signedOut) {
            try await FakeHost(401, "").client.signOutSession(session: token, id: "a1")
        }
    }

    /// The session is in the request's header, and `URLSession.shared` keeps
    /// requests in a cache on disk: nothing that carries it may be stored.
    @Test func theNetworkSessionKeepsNothing() {
        let c = AccountNetwork.configuration
        #expect(c.urlCache == nil)
        #expect(c.requestCachePolicy == .reloadIgnoringLocalCacheData)
        #expect(c.httpCookieStorage == nil)
        #expect(c.urlCredentialStorage == nil)
    }

    @Test func aLimitIsWaitedOutNotDialedAgainAtOnce() {
        #expect(LimitWait.which("daily") == "daily")
        #expect(LimitWait.which("limit:phones") == "phones")
        // Minutes, doubling to half an hour.
        #expect(LimitWait.seconds(which: "phones", tries: 0) == 120)
        #expect(LimitWait.seconds(which: "phones", tries: 1) == 240)
        #expect(LimitWait.seconds(which: "burst", tries: 3) == 960)
        #expect(LimitWait.seconds(which: "phones", tries: 4) == 1800)
        #expect(LimitWait.seconds(which: "phones", tries: 1000) == 1800)
        // The day's count: nothing before the day turns in UTC, however often it was tried.
        let tenPast = Date(timeIntervalSince1970: 1_763_856_000 + 600)
        #expect(LimitWait.seconds(which: "daily", tries: 0, now: tenPast) == 86_400 - 600 + 60)
        #expect(LimitWait.seconds(which: "daily", tries: 9, now: tenPast) == 86_400 - 600 + 60)
        let lastSecond = Date(timeIntervalSince1970: 1_763_856_000 + 86_399)
        #expect(LimitWait.seconds(which: "daily", tries: 0, now: lastSecond) == 61)
    }

    @Test func anIdThatCouldLeaveItsPathIsRefusedUnsent() async {
        let host = FakeHost(204, "")
        for id in ["../me", "a/b", "a%2Fb", "", String(repeating: "a", count: 129)] {
            await #expect(throws: AccountError.self) { try await host.client.signOutSession(session: token, id: id) }
            await #expect(throws: AccountError.self) { try await host.client.readNotice(session: token, id: id) }
        }
        #expect(host.requests.isEmpty)
    }

    @Test func aSessionNeverGoesOverPlainHttpToAnotherHost() async throws {
        let host = FakeHost(200, #"{"github_id": 1, "login": "a"}"#)
        await #expect(throws: AccountError.self) {
            try await host.at(URL(string: "http://arakun.example")!).me(session: token)
        }
        #expect(host.requests.isEmpty)
        _ = try await host.at(URL(string: "http://localhost:8787")!).me(session: token)
        #expect(host.requests.first?.url?.absoluteString == "http://localhost:8787/me")
    }

    @Test func theClientIDComesFromAnotherHost() async throws {
        #expect(try await FakeHost().client.clientID() == AccountHost.clientID)
        let host = FakeHost(200, #"{"github_client_id": "Ov23abc"}"#)
        let other = host.at(URL(string: "https://relay.example.dev")!)
        #expect(try await other.clientID() == "Ov23abc")
        #expect(host.requests.first?.url?.absoluteString == "https://relay.example.dev/auth/config")
        await #expect(throws: AccountError.self) {
            try await FakeHost(200, #"{"github_client_id": null}"#).at(URL(string: "https://relay.example.dev")!).clientID()
        }
    }

    @Test func theDeviceFlowAsksForTheEmailScope() async throws {
        let host = FakeHost(200, #"""
        {"device_code": "dc", "user_code": "WDJB-MJHT", "verification_uri": "https://github.com/login/device", "expires_in": 900, "interval": 5}
        """#)
        let code = try await host.client.startDeviceFlow(clientID: "Ov23abc")
        #expect(code == DeviceCode(userCode: "WDJB-MJHT", verificationURI: "https://github.com/login/device", deviceCode: "dc", interval: 5, expiresIn: 900))
        let r = try #require(host.requests.first)
        #expect(r.url == AccountClient.deviceCodeURL)
        #expect(String(decoding: try #require(r.httpBody), as: UTF8.self) == "client_id=Ov23abc&scope=user%3Aemail")
    }

    @Test func pollingKeepsGitHubsPace() throws {
        func next(_ json: String, _ interval: TimeInterval = 5) -> Poll {
            Poll.next(try! JSONDecoder().decode(TokenAnswer.self, from: Data(json.utf8)), interval: interval)
        }
        #expect(next(#"{"error": "authorization_pending"}"#) == .wait(5))
        #expect(next(#"{"error": "slow_down"}"#) == .wait(10))
        #expect(next(#"{"error": "slow_down", "interval": 15}"#) == .wait(15))
        #expect(next(#"{"error": "slow_down", "interval": 3}"#) == .wait(10))
        #expect(next(#"{"access_token": "gho_x", "token_type": "bearer"}"#) == .token("gho_x"))
        #expect(next(#"{"error": "expired_token"}"#) == .expired)
        #expect(next(#"{"error": "access_denied"}"#) == .denied)
        #expect(next(#"{"error": "incorrect_device_code", "error_description": "The device_code provided is not valid."}"#)
            == .failed("The device_code provided is not valid."))
        // GitHub's own trouble leaves the code valid.
        #expect(try AccountClient.pollAnswer(503, Data(), interval: 5) == .wait(5))
        #expect(throws: AccountError.self) { try AccountClient.pollAnswer(200, Data("<html>".utf8), interval: 5) }
    }

    @Test func theSocketCarriesTheSessionInItsHeaderOnly() throws {
        let r = try #require(RelayLink.socketRequest(base: AccountHost.base, label: vectorLabel, page: "0a1b2c3d4e5f60718293a4b5", session: token))
        #expect(r.url?.absoluteString == "wss://app.arakun.dev/machines/\(vectorLabel)/ws?page=0a1b2c3d4e5f60718293a4b5")
        #expect(r.value(forHTTPHeaderField: "Authorization") == "Bearer \(token.value)")
        #expect(r.url?.absoluteString.contains(token.value) == false)

        let local = RelayLink.socketRequest(base: URL(string: "http://localhost:8787")!, label: vectorLabel, page: "ab12cd34", session: token)
        #expect(local?.url?.absoluteString == "ws://localhost:8787/machines/\(vectorLabel)/ws?page=ab12cd34")
        // Not over plain http to another host, and not to a label that could leave its path.
        #expect(RelayLink.socketRequest(base: URL(string: "http://relay.example")!, label: vectorLabel, page: "ab12cd34", session: token) == nil)
        #expect(RelayLink.socketRequest(base: AccountHost.base, label: "../me", page: "ab12cd34", session: token) == nil)
    }

    @Test func theHandshakeMustNameTheAccountsKey() throws {
        let hello = try JSONSerialization.data(withJSONObject: ["v": 1, "pub": Base64URL.encode(vectorKey)])
        #expect(RelayLink.names(hello, key: vectorKey))
        var other = vectorKey
        other[other.count - 1] ^= 1
        #expect(!RelayLink.names(hello, key: other))
        #expect(!RelayLink.names(Data("not json".utf8), key: vectorKey))
    }

    @Test func theTokenNeverShowsWhenPrinted() async throws {
        let body = #"{"session": "\#(token.value)", "session_id": "a1", "github_id": 7, "login": "octocat"}"#
        let signed = try await FakeHost(200, body).client.signIn(githubToken: "gho_x", name: "n")
        var dumped = ""
        dump(signed, to: &dumped)
        for shown in [String(describing: signed), String(reflecting: signed), "\(token)", String(reflecting: token), dumped] {
            #expect(!shown.contains(token.value))
        }
    }

    /// The real Keychain, only when asked for (`ARAKUN_KEYCHAIN_TESTS=1`): a
    /// CI runner may have no keychain to write to.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["ARAKUN_KEYCHAIN_TESTS"] == "1"))
    func theKeychainStoreKeepsAndForgetsTheSession() throws {
        let store = KeychainSessionStore(service: "com.arakun.ios.account.test.\(UUID().uuidString)", account: "session")
        defer { store.save(nil) }
        #expect(store.load() == nil)
        #expect(store.save(token))
        #expect(store.load() == token)
        #expect(store.save(nil))
        #expect(store.load() == nil)
    }
}

/// A store that keeps what it is given and remembers every write.
private final class RecordingStore: SessionStore, @unchecked Sendable {
    private let lock = NSLock()
    private var held: SessionToken?
    private var written: [SessionToken?] = []
    let works: Bool

    init(holding: SessionToken? = nil, works: Bool = true) {
        held = holding
        self.works = works
    }

    var writes: [SessionToken?] { lock.withLock { written } }

    func load() -> SessionToken? { lock.withLock { held } }

    @discardableResult
    func save(_ token: SessionToken?) -> Bool {
        lock.withLock {
            written.append(token)
            if works { held = token }
            return works
        }
    }
}

private let signInBody = #"{"session": "\#(token.value)", "session_id": "a1", "github_id": 7, "login": "octocat"}"#

/// Signing in and out, and a 401, write the session only through the store
/// (the Keychain in the app), and nowhere else.
@MainActor
struct AccountSessionTests {
    private func inDefaults() -> Bool {
        UserDefaults.standard.dictionaryRepresentation().values.contains { "\($0)".contains(token.value) }
    }

    @Test func signingInAndOutWritesOnlyTheStore() async throws {
        let store = RecordingStore()
        let host = FakeHost(200, signInBody)
        let keeper = AccountSession(client: host.client, store: store)
        #expect(keeper.token == nil)
        try await keeper.signIn(githubToken: "gho_x", name: "n")
        #expect(store.writes == [token])
        #expect(keeper.token == token)
        #expect(store.load() == token)
        let told = await keeper.signOut().value
        #expect(told)
        #expect(store.writes == [token, nil])
        #expect(keeper.token == nil)
        #expect(store.load() == nil)
        #expect(host.requests.last?.url?.path == "/auth/logout")
        #expect(!inDefaults())
    }

    @Test func aSignOutTheHostDidNotHearSaysSo() async {
        // Forgotten here either way; only the answer differs.
        for (status, heard) in [(204, true), (401, true), (500, false), (429, false)] {
            let store = RecordingStore(holding: token)
            let keeper = AccountSession(client: FakeHost(status, "").client, store: store)
            let told = await keeper.signOut().value
            #expect(told == heard, "\(status)")
            #expect(keeper.token == nil)
            #expect(store.writes == [nil])
        }
    }

    /// The relay turning the session down at its socket proves nothing: only
    /// the account's own 401 may clear the Keychain.
    @Test func aSessionTheRelayTurnedDownEndsOnlyWhenTheAccountSaysSo() async {
        let me = #"{"github_id": 1, "login": "a"}"#
        let cases: [(Int, String, SessionCheck, Bool)] = [
            (200, me, .stands, true), (401, "", .over, false), (500, "", .unknown, true),
            (503, "", .unknown, true), (403, #"{"error": "blocked"}"#, .unknown, true),
        ]
        for (status, body, expected, kept) in cases {
            let store = RecordingStore(holding: token)
            let host = FakeHost(status, body)
            let keeper = AccountSession(client: host.client, store: store)
            let check = await keeper.stillStands()
            #expect(check == expected, "\(status)")
            #expect((keeper.token == token) == kept, "\(status)")
            #expect(store.writes == (kept ? [] : [nil]), "\(status)")
            #expect(host.requests.map { $0.url?.path ?? "" } == ["/me"])
        }
    }

    @Test func aStoredSessionIsReadBackAtStart() {
        let keeper = AccountSession(client: FakeHost().client, store: RecordingStore(holding: token))
        #expect(keeper.token == token)
    }

    @Test func a401ClearsTheStore() async {
        let store = RecordingStore(holding: token)
        let keeper = AccountSession(client: FakeHost(401, #"{"error": "not signed in"}"#).client, store: store)
        await #expect(throws: AccountError.signedOut) {
            try await keeper.call { c, t in try await c.machines(session: t) }
        }
        #expect(store.writes == [nil])
        #expect(keeper.token == nil)
        #expect(store.load() == nil)
    }

    @Test func otherErrorsKeepTheSession() async {
        let store = RecordingStore(holding: token)
        let keeper = AccountSession(client: FakeHost(500, "").client, store: store)
        await #expect(throws: AccountError.refused("HTTP 500")) {
            try await keeper.call { c, t in try await c.me(session: t) }
        }
        #expect(store.writes.isEmpty)
        #expect(keeper.token == token)
    }

    @Test func aSessionTheStoreCannotKeepIsEndedOnTheServer() async {
        let store = RecordingStore(works: false)
        let host = FakeHost(200, signInBody)
        let keeper = AccountSession(client: host.client, store: store)
        await #expect(throws: AccountError.self) {
            try await keeper.signIn(githubToken: "gho_x", name: "n")
        }
        #expect(keeper.token == nil)
        #expect(host.requests.map { $0.url?.path ?? "" } == ["/auth/github", "/auth/logout"])
    }
}

/// Answers GitHub's token endpoint from a script, one entry per poll; the
/// last entry repeats. `nil` is a network that is not there.
private final class ScriptedGitHub: @unchecked Sendable {
    private let lock = NSLock()
    private var script: [String?]
    private var count = 0

    init(_ script: [String?]) {
        self.script = script
    }

    var asked: Int { lock.withLock { count } }

    var client: AccountClient {
        AccountClient { _ in
            let next: String? = self.lock.withLock {
                self.count += 1
                return self.script.count > 1 ? self.script.removeFirst() : self.script[0]
            }
            guard let next else { throw URLError(.notConnectedToInternet) }
            return (200, Data(next.utf8))
        }
    }
}

private let pending = #"{"error": "authorization_pending"}"#

struct WaitForTokenTests {
    private func code(every interval: TimeInterval, for expiry: TimeInterval) -> DeviceCode {
        DeviceCode(userCode: "WDJB-MJHT", verificationURI: "https://github.com/login/device", deviceCode: "dc",
                   interval: interval, expiresIn: expiry)
    }

    @Test func carriesOnThroughALostNetwork() async throws {
        let github = ScriptedGitHub([nil, pending, #"{"access_token": "gho_x"}"#])
        let got = try await github.client.waitForToken(clientID: "c", code: code(every: 0.01, for: 60))
        #expect(got == "gho_x")
        #expect(github.asked == 3)
    }

    @Test func stopsWhenTheCodeRunsOut() async {
        let github = ScriptedGitHub([pending])
        await #expect(throws: AccountError.refused(AccountClient.expiredSentence)) {
            try await github.client.waitForToken(clientID: "c", code: code(every: 0.02, for: 0.1))
        }
    }

    @Test func stopsWhenGitHubSaysTheCodeExpired() async {
        let github = ScriptedGitHub([#"{"error": "expired_token"}"#])
        await #expect(throws: AccountError.refused(AccountClient.expiredSentence)) {
            try await github.client.waitForToken(clientID: "c", code: code(every: 0.01, for: 60))
        }
        #expect(github.asked == 1)
    }

    @Test func stopsWhenCancelled() async {
        let github = ScriptedGitHub([pending])
        let client = github.client
        let slow = code(every: 30, for: 900)
        let waiting = Task { try await client.waitForToken(clientID: "c", code: slow) }
        waiting.cancel()
        await #expect(throws: CancellationError.self) { try await waiting.value }
        #expect(github.asked == 0)
    }
}
