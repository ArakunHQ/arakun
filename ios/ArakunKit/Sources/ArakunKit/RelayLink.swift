// One connection to one of the account's computers through the relay, as
// phone/relay.js makes it: a WebSocket to `/machines/<label>/ws` that the
// session in its Authorization header lets in, a handshake sealed end to end
// so the relay only passes bytes along and checked against the key the
// account lists, then the same JSON the phone page speaks. The relay admits
// only the account's own devices, so nothing more is proven inside the
// channel. A message too big for a relay frame comes as `{part}`.
import Foundation
import Security

@MainActor
public final class RelayLink {
    public enum Event {
        /// The channel is up: the computer proved it holds the account's key for it.
        case open
        /// A message from the computer, whole, as JSON.
        case message(Data)
        /// The computer went away from the relay (`since` it was last seen), or came back.
        case computer(online: Bool, since: Date?)
        case closed(EndKind, String)
    }

    /// The page pings the relay this often and the relay answers on its own; a
    /// link that hears nothing for two pings and some is dead.
    static let pingEvery: TimeInterval = 30
    static let silentAfter: TimeInterval = 75
    /// How long the computer may take to answer a hello.
    static let helloWithin: TimeInterval = 15

    /// This app's own name to the relay. It stays the same across reconnects,
    /// so a socket left behind by a network switch is replaced.
    nonisolated static let page: String = {
        var b = [UInt8](repeating: 0, count: 12)
        _ = SecRandomCopyBytes(kSecRandomDefault, b.count, &b)
        return b.map { String(format: "%02x", $0) }.joined()
    }()

    private let machine: Machine
    private let key: Data?
    private let request: URLRequest?
    private let accountHost: String
    private let onEvent: (Event) -> Void
    private var task: URLSessionWebSocketTask?
    private var channel: UInt32 = 0
    private var handshake: E2E.Handshake?
    private var session: E2E.Session?
    private var joiner = Joiner()
    private var ready = false
    private var ended = false
    private var heard = Date()
    private var helloDeadline: Date?
    private var timer: Task<Void, Never>?

    /// A link to `machine`, one of the account's computers as `/machines`
    /// lists it, on the relay at `base`.
    public init(machine: Machine, base: URL, session: SessionToken, onEvent: @escaping (Event) -> Void) {
        self.machine = machine
        self.onEvent = onEvent
        key = machine.trustedKey
        request = RelayLink.socketRequest(base: base, label: machine.label, page: RelayLink.page, session: session)
        accountHost = base.host ?? base.absoluteString
    }

    public var isReady: Bool { ready && !ended }

    /// The device's socket on the relay: the session in the Authorization
    /// header, never in the URL. Nil when the address could not carry it.
    public nonisolated static func socketRequest(base: URL, label: String, page: String, session: SessionToken) -> URLRequest? {
        guard AccountHost.carriesSessions(base), (try? AccountClient.pathPart(label)) != nil,
              var parts = URLComponents(url: base, resolvingAgainstBaseURL: false)
        else { return nil }
        // Plain `ws` only ever reaches a relay run by hand (`wrangler dev`).
        parts.scheme = ["https", "wss"].contains(parts.scheme?.lowercased() ?? "") ? "wss" : "ws"
        parts.path = "/machines/\(label)/ws"
        parts.queryItems = [URLQueryItem(name: "page", value: page)]
        parts.fragment = nil
        guard let url = parts.url else { return nil }
        var request = URLRequest(url: url)
        request.setValue(session.bearer, forHTTPHeaderField: "Authorization")
        return request
    }

    /// Whether a computer's hello names `key`, the one the account lists. The
    /// handshake then checks the hello is signed with it for this label.
    public nonisolated static func names(_ hello: Data, key: Data) -> Bool {
        guard let obj = try? JSONSerialization.jsonObject(with: hello) as? [String: Any],
              let pub = obj["pub"] as? String, let theirs = Base64URL.decode(pub)
        else { return false }
        return theirs == key
    }

    public func start() {
        guard key != nil else { return end(.untrusted, "the account lists a key for this computer that is not its label's") }
        guard let request else { return end(.gone, "bad address") }
        // Not `URLSession.shared`: the request carries the session.
        let task = AccountNetwork.session.webSocketTask(with: request)
        self.task = task
        task.resume()
        heard = Date()
        helloDeadline = Date().addingTimeInterval(Self.helloWithin)
        receive(task)
        timer = Task { [weak self] in
            while !Task.isCancelled {
                self?.tick()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    private var lastPing = Date.distantPast

    private func tick() {
        guard !ended, let task else { return }
        let now = Date()
        if let d = helloDeadline, now > d { return end(.gone, "no answer from the computer") }
        if now.timeIntervalSince(heard) > Self.silentAfter { return end(.gone, "the relay went quiet") }
        if now.timeIntervalSince(lastPing) >= Self.pingEvery {
            lastPing = now
            task.send(.string("ping")) { _ in }
        }
    }

    private func receive(_ task: URLSessionWebSocketTask) {
        Task { [weak self] in
            while true {
                let msg: URLSessionWebSocketTask.Message
                do {
                    msg = try await task.receive()
                } catch {
                    guard let self, self.task === task else { return }
                    // An upgrade answered with plain HTTP. A 401 is about the
                    // session, which the app then asks the account about. Any
                    // other status (a 500 while the relay's database is down)
                    // has no close code, so it ends as `gone` and is tried again.
                    if (task.response as? HTTPURLResponse)?.statusCode == 401 {
                        self.end(.signedOut, "the relay answered 401")
                        return
                    }
                    let reason = task.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? ""
                    let code = task.closeCode.rawValue
                    self.refused(reason: reason, code: code, message: nil)
                    return
                }
                guard let self, !self.ended else { return }
                self.heard = Date()
                // The relay's "pong" is text: it only says it is there.
                if case .data(let bytes) = msg { self.frame(bytes) }
            }
        }
    }

    private func frame(_ bytes: Data) {
        guard let f = Frame(decoding: bytes) else { return }
        switch f.kind {
        case .control:
            if let msg = try? JSONSerialization.jsonObject(with: f.payload) as? [String: Any] { control(msg) }
        case .hello:
            guard let h = handshake, let key else { return }
            // The key the account has for this computer, and the label it
            // derives, which the hello's signature covers.
            guard Self.names(f.payload, key: key) else { return end(.untrusted, "bad hello: not the account's key") }
            do {
                session = try h.finish(f.payload, label: machine.label, channel: channel)
            } catch {
                return end(.untrusted, "\(error)")
            }
            handshake = nil
            ready = true
            helloDeadline = nil
            onEvent(.open)
        case .data:
            sealed(f.payload)
        }
    }

    private func greet() {
        session = nil
        ready = false
        // Pieces from before belong to a message that will not be finished.
        joiner = Joiner()
        let h = E2E.Handshake()
        handshake = h
        task?.send(.data(Frame(.hello, h.hello).encoded)) { _ in }
        helloDeadline = Date().addingTimeInterval(Self.helloWithin)
    }

    private func control(_ msg: [String: Any]) {
        switch msg["type"] as? String {
        case "open":
            channel = UInt32(truncatingIfNeeded: msg["channel"] as? Int ?? 0)
            greet()
        case "pc_offline":
            // Nobody to answer until `pc_online`: no deadline meanwhile.
            helloDeadline = nil
            session = nil
            ready = false
            let since = (msg["since"] as? Double).map { Date(timeIntervalSince1970: $0 / 1000) }
            onEvent(.computer(online: false, since: since))
        case "pc_online":
            onEvent(.computer(online: true, since: nil))
            greet()
        case "bye":
            refused(reason: msg["reason"] as? String ?? "", code: 0, message: msg["message"] as? String)
        case "limit":
            end(.limit, msg["which"] as? String ?? "limit")
        default:
            break
        }
    }

    private func sealed(_ payload: Data) {
        guard let session else { return }
        var plain: Data
        do {
            plain = try session.open(payload)
        } catch {
            return end(.untrusted, "\(error)")
        }
        guard let msg = try? JSONSerialization.jsonObject(with: plain) as? [String: Any] else { return }
        if let part = msg["part"] as? [String: Any] {
            guard let whole = joiner.add(part) else { return }
            plain = whole
        }
        onEvent(.message(plain))
    }

    private func say(_ obj: [String: Any]) {
        guard !ended, let session, let task,
              let json = try? JSONSerialization.data(withJSONObject: obj),
              let payload = try? session.seal(json)
        else { return }
        task.send(.data(Frame(.data, payload).encoded)) { [weak self] error in
            guard error != nil else { return }
            Task { @MainActor in self?.end(.gone, "could not send") }
        }
    }

    /// A message to the computer, as the phone page sends it.
    public func send(_ obj: [String: Any]) {
        if isReady { say(obj) }
    }

    /// Ends with what the relay said: a refusal as its sentence, else the reason.
    private func refused(reason: String, code: Int, message: String?) {
        let kind = EndKind.of(reason: reason, code: code, message: message, accountHost: accountHost)
        if case .refused(_, let sentence) = kind { return end(kind, sentence) }
        end(kind, reason.isEmpty ? "closed (\(code))" : reason)
    }

    private func end(_ kind: EndKind, _ detail: String) {
        guard !ended else { return }
        hangUp()
        onEvent(.closed(kind, detail))
    }

    /// Hangs up without a word; no event follows.
    public func close() {
        guard !ended else { return }
        hangUp()
    }

    private func hangUp() {
        ended = true
        ready = false
        timer?.cancel()
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
    }
}
