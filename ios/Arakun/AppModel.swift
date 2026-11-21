import ArakunKit
import Foundation
import Observation
import UIKit

/// Everything the app shows, and the one link to the computer behind it. The
/// computer sends a snapshot when the link opens and changes from then on.
@MainActor
@Observable
final class AppModel {
    enum Connection: Equatable {
        case unpaired
        case pairing
        case connecting
        case connected
        /// The relay is there, the computer is not (last seen `since`).
        case computerAway(since: Date?)
        /// Stopped for good until the user acts.
        case failed(String)
    }

    private(set) var credentials: Credentials? = Keychain.load()
    private(set) var connection: Connection = .unpaired
    private(set) var host = ""
    private(set) var inbox: [InboxSession] = []
    private(set) var places: [Place] = []
    /// The session on screen, kept live by the computer.
    var session: LiveSession?
    var loadingOlder = false
    /// What went wrong pairing, for the pairing screen.
    var pairError: String?
    /// A message worth a moment on screen.
    var notice: String?
    /// A session asked for before the link was up (from a started session).
    private var openWhenLinked: String?
    /// A start sent and not yet answered with `started` or `error`.
    private var starting = false

    private var link: RelayLink?
    private var retry: Duration = .milliseconds(500)
    private var retryTask: Task<Void, Never>?

    init() {
        #if DEBUG
        if Self.isDemo {
            if !UserDefaults.standard.bool(forKey: "demoUnpaired") { fillDemo() } else { credentials = nil }
            return
        }
        #endif
        if credentials != nil { connect() }
    }

    var deviceName: String { UIDevice.current.model }

    // ---- connection --------------------------------------------------------

    func connect() {
        guard let c = credentials else {
            connection = .unpaired
            return
        }
        start(.credentials(c))
        connection = .connecting
    }

    func pair(_ text: String) {
        guard let p = PairingLink(text) else {
            pairError = "That is not a pairing link. Open the Phone tab in Arakun and scan its code."
            return
        }
        pairError = nil
        connection = .pairing
        start(.pairing(p, deviceName: deviceName))
    }

    private func start(_ mode: RelayLink.Mode) {
        retryTask?.cancel()
        link?.close()
        let l = RelayLink(mode) { [weak self] event in self?.handle(event) }
        link = l
        l.start()
    }

    /// Back on screen or Retry: go now instead of waiting out the backoff.
    func reconnectNow() {
        guard credentials != nil else { return }
        switch connection {
        case .connected, .pairing: return
        default:
            retry = .milliseconds(500)
            connect()
        }
    }

    func unpair() {
        Task {
            _ = try? await link?.fetch("/unpair", method: "POST")
            forget()
        }
    }

    private func forget() {
        link?.close()
        link = nil
        retryTask?.cancel()
        credentials = nil
        Keychain.save(nil)
        inbox = []
        places = []
        session = nil
        openWhenLinked = nil
        loadingOlder = false
        starting = false
        connection = .unpaired
    }

    /// The link went down: what was asked of it will not be answered.
    private func interrupted() {
        // The computer answers an `entries` request at most once, so asking
        // again after a drop is safe.
        loadingOlder = false
        if starting {
            starting = false
            notice = "The connection dropped while the session was starting. Check the inbox before you start it again."
        }
    }

    private func lost() {
        connection = connection == .unpaired ? .unpaired : .connecting
        retryTask?.cancel()
        let wait = retry
        retry = min(retry * 2, .seconds(8))
        retryTask = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            self?.connect()
        }
    }

    private func handle(_ event: RelayLink.Event) {
        switch event {
        case .open(let who, let new):
            if let new {
                credentials = new
                if !Keychain.save(new) {
                    notice = "Could not keep the pairing in the Keychain. You may need to pair again after the app restarts."
                }
            }
            host = who.host.isEmpty ? host : who.host
            interrupted()
            connection = .connected
            retry = .milliseconds(500)
            if let s = session { link?.send(["type": "open", "session": s.id]) }
            if let id = openWhenLinked {
                openWhenLinked = nil
                open(id)
            }
        case .message(let data):
            if let msg = JSON.parse(data) { receive(msg) }
        case .computer(let online, let since):
            if !online { interrupted() }
            connection = online ? .connecting : .computerAway(since: since)
        case .closed(let kind, let detail):
            link = nil
            interrupted()
            switch kind {
            case .unpaired:
                forget()
                pairError = "This phone is no longer paired. Pair it again from Arakun."
            case .pairing:
                connection = credentials == nil ? .unpaired : .connecting
                pairError = "That link is wrong or has expired. Make a new one in Arakun."
                if credentials != nil { lost() }
            case .untrusted:
                connection = .failed("This address does not answer with the key Arakun showed. Check the address in its Phone tab before trying again.")
            case .limit:
                notice = "The relay is busy (\(detail)). Trying again in a moment."
                lost()
            case .gone:
                if credentials == nil {
                    connection = .unpaired
                    pairError = "Could not reach your computer (\(detail))."
                } else {
                    lost()
                }
            }
        }
    }

    private func receive(_ msg: JSON) {
        switch msg["type"].str {
        case "inbox":
            let i = Inbox(msg)
            host = i.host
            inbox = i.sessions
            places = i.places
        case "session":
            guard session?.id == msg["id"].str else { return }
            session?.apply(session: msg)
        case "entries":
            guard session?.id == msg["id"].str else { return }
            if msg["older"].bool { loadingOlder = false }
            session?.apply(entries: msg)
        case "state":
            guard session?.id == msg["id"].str else { return }
            session?.apply(state: msg)
        case "started":
            starting = false
            open(msg["id"].str)
        case "error":
            starting = false
            notice = msg["message"].string
        default:
            break
        }
    }

    // ---- what the user does -------------------------------------------------

    private func send(_ msg: [String: Any]) {
        link?.send(msg)
    }

    /// Whether a message sent now reaches the computer; the link drops it otherwise.
    private var canSend: Bool { link?.isReady == true }

    func open(_ id: String) {
        let line = inbox.first { $0.id == id }
        session = LiveSession(id: id, title: line?.title ?? "", agent: line?.agent ?? "")
        #if DEBUG
        if Self.isDemo {
            session = demoSession(id)
            return
        }
        #endif
        if link?.isReady == true { send(["type": "open", "session": id]) } else { openWhenLinked = id }
    }

    func close() {
        session = nil
        openWhenLinked = nil
        loadingOlder = false
        send(["type": "close"])
    }

    func loadOlder() {
        guard let s = session, s.from > 0, !loadingOlder, canSend else { return }
        loadingOlder = true
        send(["type": "entries", "session": s.id, "from": max(0, s.from - 50), "to": s.from])
    }

    func say(_ text: String) {
        guard let s = session else { return }
        send(["type": "send", "session": s.id, "text": text])
    }

    func stop() {
        guard let s = session, canSend else { return }
        send(["type": "cancel", "session": s.id, "keep_queue": true])
    }

    func allow(_ option: PermissionAsk.Option) {
        // Offline the card stays, with what was picked in it.
        guard let s = session, canSend else { return }
        send(["type": "permission", "session": s.id, "option": option.id])
        session?.permission = nil
    }

    func answer(_ answers: [String]) {
        // Offline the card stays, with what was picked in it.
        guard let s = session, canSend else { return }
        send(["type": "answer", "session": s.id, "answers": answers])
        session?.question = nil
    }

    func skipQuestion() {
        // Offline the card stays, with what was picked in it.
        guard let s = session, canSend else { return }
        send(["type": "skip_question", "session": s.id])
        session?.question = nil
    }

    /// Whether the start went out; the form keeps its prompt when it did not.
    @discardableResult
    func startSession(in place: Place, text: String) -> Bool {
        guard canSend else { return false }
        starting = true
        send(["type": "start", "env": place.path, "agent": place.agent, "text": text])
        return true
    }

    /// The computer skips the push for the session on screen while the app shows it.
    func visible(_ yes: Bool) {
        send(["type": "visible", "visible": yes])
        if yes { reconnectNow() }
    }

    var isLinked: Bool { connection == .connected }
}

#if DEBUG
// A computer played here, for screenshots and design work without one:
// launch with `-demo` (and `-demoUnpaired YES` for the pairing screen).
extension AppModel {
    static var isDemo: Bool { ProcessInfo.processInfo.arguments.contains("-demo") }

    func fillDemo() {
        let now = Date().timeIntervalSince1970
        func j(_ s: String) -> JSON { JSON.parse(Data(s.utf8))! }
        credentials = Credentials(host: "lmx5lt63vsxoyes4.arakun.dev", label: "lmx5lt63vsxoyes4", secure: true,
                                  id: "demo", ticket: "", token: "", name: "studio")
        connection = .connected
        let inboxJSON = """
        {"host":"studio","sessions":[
         {"id":"s1","title":"Fix the relay pairing limit","agent":"Opus 5.5","status":"needs_input","updated":\(now - 40),"added":42,"removed":7,"project":"arakun","env":"env/brave-otter","place":"/w/a","pr":"#631","asks":{"kind":"permission","text":"cargo test -p arakun relay"}},
         {"id":"s2","title":"Port the phone page to SwiftUI","agent":"Opus 5.5","status":"working","updated":\(now - 120),"added":1204,"removed":0,"project":"arakun","env":"env/quick-dune","place":"/w/b"},
         {"id":"s3","title":"Speed up the inbox query","agent":"Droid","status":"idle","updated":\(now - 3600),"added":18,"removed":31,"project":"arakun","env":"main","place":"/w/c","pr":"#627"},
         {"id":"s4","title":"Landing page hero copy","agent":"Sonnet 5.5","status":"error","updated":\(now - 7200),"added":0,"removed":0,"project":"site","env":"env/calm-heron","place":"/w/d"},
         {"id":"s5","title":"Release notes for 0.13","agent":"Haiku 5.5","status":"idle","updated":\(now - 86400),"added":64,"removed":2,"project":"site","env":"main","place":"/w/e"}],
         "places":[{"path":"/w/a","project":"arakun","env":"env/brave-otter","branch":"env/brave-otter","agent":"claude"},{"path":"/w/e","project":"site","env":"main","branch":"main","agent":"claude"}]}
        """
        let i = Inbox(j(inboxJSON))
        host = i.host
        inbox = i.sessions
        places = i.places
    }

    func demoSession(_ id: String) -> LiveSession {
        let line = inbox.first { $0.id == id }
        var s = LiveSession(id: id, title: line?.title ?? "", agent: line?.agent ?? "")
        let at = Date().timeIntervalSince1970 - 600
        s.apply(session: JSON.parse(Data("""
        {"from":0,"count":7,"status":"\(line?.status == .working ? "working" : "needs_input")","usage":{"summary":"48k tokens · $0.41"},
         "changes":{"files":3,"added":42,"removed":7},"elapsed_ms":95000,
         "entries":[
          {"type":"user","text":"Pairing links fail with limit:phones. Find out why and fix it.","at":\(at),"elapsed_ms":null,"pastes":[]},
          {"type":"thought","text":"The relay spends the code before it checks the phone limit, so a refused phone can never retry."},
          {"type":"tool","id":"t1","title":"Read relay/src/relay.ts","kind":"read","status":"completed","diffs":[],"added":0,"removed":0},
          {"type":"tool","id":"t2","title":"rg -n admit relay/src","kind":"search","status":"completed","command":"rg -n admit relay/src","output":"relay/src/relay.ts:412:  admit(phone) {\\nrelay/src/relay.ts:498:    this.admit(p)","diffs":[],"added":0,"removed":0},
          {"type":"agent","text":"Found it. `acceptPhone` deletes the pairing code in `admit()` **before** the limit check. When the check refuses the phone, the code is already gone, so every retry gets `no_pairing`."},
          {"type":"plan","entries":[{"content":"Check the limit before spending the code","status":"completed"},{"content":"Add a test for a refused phone that retries","status":"in_progress"},{"content":"Run the relay tests","status":"pending"}]},
          {"type":"tool","id":"t3","title":"Edit relay/src/relay.ts","kind":"edit","status":"completed","diffs":[{"path":"relay/src/relay.ts","added":12,"removed":5}],"added":12,"removed":5}],
         "permission":{"tool_call_id":"p1","title":"cargo test -p arakun relay","options":[{"id":"a","name":"Allow","kind":"allow_once"},{"id":"b","name":"Always allow","kind":"allow_always"},{"id":"c","name":"Deny","kind":"reject_once"}]}}
        """.utf8))!)
        if line?.status == .working { s.permission = nil }
        return s
    }
}
#endif
