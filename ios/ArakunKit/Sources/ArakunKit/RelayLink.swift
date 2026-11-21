// One connection to the computer through the relay, as phone/relay.js makes
// it: a WebSocket to `/ws`, a handshake sealed end to end so the relay only
// passes bytes along, then the same JSON the phone page speaks. Inside the
// channel `{req}` asks what the page would ask over HTTP, the computer answers
// `{res}` in pieces, and any message too big for a relay frame comes as `{part}`.
import Foundation

@MainActor
public final class RelayLink {
    /// Who answered: the device id the computer knows this phone by, the
    /// phone's name as the computer keeps it, and the computer's name (`host`).
    public struct Who: Sendable {
        public let id: String
        public let name: String
        public let host: String
    }

    public enum Event {
        /// The channel is up. `credentials` are new ones when this was a pairing.
        case open(Who, credentials: Credentials?)
        /// A message from the computer, whole, as JSON.
        case message(Data)
        /// The computer went away from the relay (`since` it was last seen), or came back.
        case computer(online: Bool, since: Date?)
        case closed(EndKind, String)
    }

    public enum Mode: Sendable {
        case credentials(Credentials)
        case pairing(PairingLink, deviceName: String)
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

    private let mode: Mode
    private let label: String
    private let host: String
    private let secure: Bool
    private let onEvent: (Event) -> Void
    private var task: URLSessionWebSocketTask?
    private var channel: UInt32 = 0
    private var handshake: E2E.Handshake?
    private var session: E2E.Session?
    private var joiner = Joiner()
    private var credentials: Credentials?
    private var ready = false
    private var ended = false
    private var heard = Date()
    private var helloDeadline: Date?
    private var timer: Task<Void, Never>?
    private var nextReq = 1
    private var pending: [Int: Request] = [:]

    private struct Request {
        var status = 0
        var type = ""
        var body = Data()
        let done: CheckedContinuation<(Int, Data), Error>
    }

    public struct Closed: Error {}

    public init(_ mode: Mode, onEvent: @escaping (Event) -> Void) {
        self.mode = mode
        self.onEvent = onEvent
        switch mode {
        case .credentials(let c):
            label = c.label
            host = c.host
            secure = c.secure
            credentials = c
        case .pairing(let link, _):
            label = link.label
            host = link.host
            secure = link.secure
        }
    }

    public var isReady: Bool { ready && !ended }

    public func start() {
        var parts = URLComponents()
        parts.scheme = secure ? "wss" : "ws"
        parts.percentEncodedHost = host.split(separator: ":").first.map(String.init)
        if let port = host.split(separator: ":").dropFirst().first { parts.port = Int(port) }
        parts.path = "/ws"
        switch mode {
        case .credentials(let c):
            parts.queryItems = [.init(name: "d", value: c.id), .init(name: "t", value: c.ticket), .init(name: "s", value: Self.page)]
        case .pairing(let link, _):
            parts.queryItems = [.init(name: "p", value: link.code)]
        }
        guard let url = parts.url else { return end(.gone, "bad address") }
        let task = URLSession.shared.webSocketTask(with: url)
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
                    let reason = task.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? ""
                    let code = task.closeCode.rawValue
                    self.end(EndKind.of(reason: reason, code: code), reason.isEmpty ? "closed (\(code))" : reason)
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
            guard let h = handshake else { return }
            do {
                session = try h.finish(f.payload, label: label, channel: channel)
            } catch {
                return end(.untrusted, "\(error)")
            }
            handshake = nil
            if let c = credentials {
                say(["auth": ["token": c.token]])
            } else if case .pairing(let link, let name) = mode {
                say(["pair": ["secret": link.secret, "name": name]])
            }
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
            failPending()
            let since = (msg["since"] as? Double).map { Date(timeIntervalSince1970: $0 / 1000) }
            onEvent(.computer(online: false, since: since))
        case "pc_online":
            onEvent(.computer(online: true, since: nil))
            greet()
        case "bye":
            let reason = msg["reason"] as? String ?? ""
            end(EndKind.of(reason: reason, code: 0), reason)
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
        guard var msg = try? JSONSerialization.jsonObject(with: plain) as? [String: Any] else { return }
        if let part = msg["part"] as? [String: Any] {
            guard let whole = joiner.add(part) else { return }
            plain = whole
            guard let m = try? JSONSerialization.jsonObject(with: whole) as? [String: Any] else { return }
            msg = m
        }
        if let p = msg["paired"] as? [String: Any] {
            let who = Who(id: p["id"] as? String ?? "", name: p["name"] as? String ?? "", host: p["host"] as? String ?? "")
            let c = Credentials(host: host, label: label, secure: secure, id: who.id,
                                ticket: p["ticket"] as? String ?? "", token: p["token"] as? String ?? "", name: who.host)
            credentials = c
            opened(who, c)
        } else if let ok = msg["ok"] as? [String: Any] {
            opened(Who(id: ok["id"] as? String ?? "", name: ok["name"] as? String ?? "", host: ok["host"] as? String ?? ""), nil)
        } else if let r = msg["res"] as? [String: Any] {
            guard let id = r["id"] as? Int, var p = pending[id] else { return }
            if let s = r["status"] as? Int { p.status = s }
            if let t = r["type"] as? String { p.type = t }
            p.body.append(Base64URL.decode(r["body"] as? String ?? "") ?? Data())
            if r["more"] as? Bool == true {
                pending[id] = p
            } else {
                pending[id] = nil
                p.done.resume(returning: (p.status == 0 ? 500 : p.status, p.body))
            }
        } else {
            onEvent(.message(plain))
        }
    }

    private func opened(_ who: Who, _ new: Credentials?) {
        ready = true
        helloDeadline = nil
        onEvent(.open(who, credentials: new))
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

    /// What the phone page would ask over HTTP: the status and body come back.
    public func fetch(_ path: String, method: String = "GET", body: String? = nil) async throws -> (Int, Data) {
        guard isReady else { throw Closed() }
        let id = nextReq
        nextReq += 1
        return try await withCheckedThrowingContinuation { done in
            pending[id] = Request(done: done)
            say(["req": ["id": id, "method": method, "path": path, "body": body as Any? ?? NSNull()]])
        }
    }

    private func failPending() {
        for p in pending.values { p.done.resume(throwing: Closed()) }
        pending = [:]
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
        failPending()
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
    }
}
