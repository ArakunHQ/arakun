import ArakunKit
import Foundation
import Observation
import UIKit

/// Everything the app shows: who is signed in, the account's computers, and
/// the one link to the computer picked. The computer sends a snapshot when the
/// link opens and changes from then on.
@MainActor
@Observable
final class AppModel {
    enum Connection: Equatable {
        /// No computer picked.
        case idle
        case connecting
        case connected
        /// The relay is there, the computer is not (last seen `since`).
        case computerAway(since: Date?)
        /// The relay said a limit was reached (`which`: `daily`, `phones`, …).
        /// Nothing is tried before `until`, unless the person asks.
        case limited(which: String, until: Date)
    }

    /// Where sign-in stands, while it is under way.
    enum SignIn: Equatable {
        /// Asking GitHub for a code.
        case asking
        /// The code is on screen, and the app polls GitHub until it is entered.
        case code(DeviceCode)
        /// GitHub said yes; the account host makes the session.
        case finishing
    }

    /// The session, and the only way it is written: the Keychain, through it.
    let keeper: AccountSession
    /// The account's session, as `keeper` holds it, for the views to watch.
    private(set) var signedIn: SessionToken?
    private(set) var me: Me?
    private(set) var machines: [Machine] = []
    /// Whether the list of computers came in since sign-in.
    private(set) var machinesLoaded = false
    /// The operator blocked the account: there is no list.
    private(set) var blocked = false
    /// A sentence for the list of computers: why it is on screen, or what went wrong.
    var accountSaid: String?
    /// The computer picked, as the account lists it.
    private(set) var machine: Machine?
    /// The account's signed-in devices, for Settings.
    private(set) var devices: [SignedInDevice] = []
    /// Why the signed-in devices did not load.
    private(set) var devicesError: String?
    private(set) var signIn: SignIn?
    /// Why the last sign-in did not work, for the sign-in screen.
    var signInError: String?
    /// What "Pair on this network" made of the last code or link.
    var pairSaid: String?
    /// Whether `pairSaid` tells of a page that opened, not of a problem.
    private(set) var pairOpened = false
    /// Whether `accountSaid` is about a load that failed, so the next list clears it.
    private var loadFailed = false
    /// The session `loadAccount` is asking for now, so two asks at once (the
    /// launch and the app coming on screen) do not both open a computer.
    private var loadingFor: SessionToken?

    private(set) var connection: Connection = .idle
    private(set) var host = ""
    private(set) var inbox: [InboxSession] = []
    private(set) var places: [Place] = []
    /// The session on screen, kept live by the computer.
    var session: LiveSession?
    var loadingOlder = false
    /// A message worth a moment on screen.
    var notice: String?
    /// A session asked for before the link was up (from a started session).
    private var openWhenLinked: String?
    /// A start sent and not yet answered with `started` or `error`. Here, not
    /// in the form, so closing and reopening it cannot send a second start.
    private(set) var starting = false

    private var link: RelayLink?
    private var retry: Duration = .milliseconds(500)
    private var retryTask: Task<Void, Never>?
    /// Limits the relay answered with in a row, for the wait before the next try.
    private var limitTries = 0
    private var signInTask: Task<Void, Never>?
    /// Counts sign-ins, so one cancelled or replaced never touches the next.
    private var signInAttempt = 0

    /// The computer opened last, by label, to open again at the next launch.
    /// Only the label: it is no secret, and the session stays in the Keychain.
    private static let lastMachineKey = "arakun.machine"
    /// Set at the first launch. UserDefaults go with the app when it is
    /// deleted and the Keychain does not, so a session found without this was
    /// left by an install that is gone.
    private static let hasRunKey = "arakun.hasRun"

    init(store: any SessionStore = Keychain.session) {
        #if DEBUG
        if AppModel.isDemo {
            keeper = AccountSession(store: DemoStore())
            fillDemo()
            return
        }
        #endif
        Keychain.forgetOldPairing()
        keeper = AccountSession(store: store)
        if !UserDefaults.standard.bool(forKey: Self.hasRunKey) {
            UserDefaults.standard.set(true, forKey: Self.hasRunKey)
            // Deleting the app must sign the phone out: installed again, it starts signed out.
            keeper.signOut()
        }
        signedIn = keeper.token
        if signedIn != nil { Task { await loadAccount(openOne: true) } }
    }

    /// What the account's list of signed-in devices calls this phone.
    var deviceName: String { UIDevice.current.name }

    /// A `-demo` launch, for screenshots: it asks nothing. Always false in a release build.
    nonisolated static var isDemoLaunch: Bool {
        #if DEBUG
        ProcessInfo.processInfo.arguments.contains("-demo")
        #else
        false
        #endif
    }

    // ---- signing in -----------------------------------------------------------

    /// GitHub's device flow: show the code, wait until it is entered on
    /// GitHub, then trade GitHub's token for the account's session.
    func startSignIn() {
        guard signIn == nil else { return }
        signInError = nil
        pairSaid = nil
        #if DEBUG
        if Self.isDemo {
            signIn = .code(DeviceCode(userCode: "WDJB-MJHT", verificationURI: "https://github.com/login/device",
                                      deviceCode: "", interval: 5, expiresIn: 900))
            return
        }
        #endif
        signIn = .asking
        signInAttempt += 1
        let attempt = signInAttempt
        let client = keeper.client
        let name = deviceName
        signInTask = Task { [weak self] in
            do {
                let id = try await client.clientID()
                let code = try await client.startDeviceFlow(clientID: id)
                guard let self, self.isCurrent(attempt) else { return }
                self.signIn = .code(code)
                let github = try await client.waitForToken(clientID: id, code: code)
                guard self.isCurrent(attempt) else { return }
                self.signIn = .finishing
                // In a task of its own, which Cancel does not reach: a request
                // cancelled after the host made the session would lose the
                // answer, and leave a session there that nobody holds.
                let keeper = self.keeper
                let signed = try await Task { try await keeper.signIn(githubToken: github, name: name, kind: "ios") }.value
                guard self.isCurrent(attempt) else {
                    // Cancelled while the session was made: nobody wants it,
                    // so it is forgotten here and ended on the server.
                    if keeper.token == signed.session { keeper.signOut() }
                    return
                }
                self.tookSignIn(signed)
            } catch {
                // A sign-in cancelled or replaced says nothing: the screen is another's now.
                guard let self, self.isCurrent(attempt), !(error is CancellationError) else { return }
                if let e = error as? AccountError {
                    self.signInFailed(AppModel.sentence(e))
                } else {
                    self.signInFailed("Sign-in did not work. Try again.")
                }
            }
        }
    }

    /// Whether sign-in `attempt` is still the one on screen.
    private func isCurrent(_ attempt: Int) -> Bool {
        !Task.isCancelled && signInAttempt == attempt
    }

    func cancelSignIn() {
        signInAttempt += 1
        signInTask?.cancel()
        signInTask = nil
        signIn = nil
    }

    /// GitHub's and the host's refusals, as a sentence for the sign-in screen.
    private nonisolated static func sentence(_ e: AccountError) -> String {
        guard case .refused(let why) = e else { return e.message }
        return "Sign-in did not work: \(why.hasSuffix(".") ? String(why.dropLast()) : why)."
    }

    private func signInFailed(_ why: String) {
        signInTask = nil
        signIn = nil
        signInError = why
    }

    private func tookSignIn(_ s: SignedIn) {
        signInTask = nil
        signIn = nil
        signedIn = keeper.token
        me = Me(githubID: s.githubID, login: s.login)
        Task { await loadAccount(openOne: true) }
    }

    /// Signs this phone out of the account, here and on the account host.
    func signOut() {
        guard signedIn != nil else { return }
        // The demo's keeper holds no session, so it asks nothing.
        let ending = keeper.signOut()
        signedOut()
        let host = keeper.client.base.host ?? "the account host"
        Task {
            let told = await ending.value
            // Signed out here either way. The session cannot be used without
            // its token, but the account lists it until it runs out.
            guard !told, signedIn == nil, signIn == nil else { return }
            signInError = "This phone is signed out, but \(host) could not be told. Until that sign-in runs out, your account still lists it under signed-in devices."
        }
    }

    /// The session is gone (signed out here or elsewhere): only sign-in is left.
    private func signedOut() {
        dropMachine()
        signInAttempt += 1
        signInTask?.cancel()
        signInTask = nil
        signIn = nil
        keeper.forget()
        UserDefaults.standard.removeObject(forKey: Self.lastMachineKey)
        signedIn = nil
        me = nil
        machines = []
        machinesLoaded = false
        blocked = false
        devices = []
        devicesError = nil
        accountSaid = nil
        loadFailed = false
        signInError = nil
        pairSaid = nil
    }

    // ---- the account ----------------------------------------------------------

    /// Who is signed in and their computers. `openOne` opens one at launch or
    /// after sign-in: the only one, or the one opened last.
    /// `quiet` is the list's own poll: it leaves the sentence on screen alone
    /// until an answer replaces it.
    func loadAccount(openOne: Bool = false, quiet: Bool = false) async {
        guard let token = signedIn, !Self.isDemoLaunch, loadingFor != token else { return }
        loadingFor = token
        defer { if loadingFor == token { loadingFor = nil } }
        // Loading again after it could not reach the host: the sentence goes.
        if !machinesLoaded && !quiet {
            accountSaid = nil
            loadFailed = false
        }
        do {
            let me = try await keeper.call { c, t in try await c.me(session: t) }
            guard signedIn == token else { return }
            self.me = me
            let list = try await keeper.call { c, t in try await c.machines(session: t) }
            guard signedIn == token else { return }
            took(list)
            guard openOne, machine == nil else { return }
            if list.count == 1 { return pick(list[0]) }
            let last = UserDefaults.standard.string(forKey: Self.lastMachineKey)
            if let m = list.first(where: { $0.label == last }) { pick(m) }
        } catch {
            accountFailed(error, token, quiet: quiet)
        }
    }

    /// The list's poll while it is on screen. After a start that did not
    /// reach the account (no network at launch), it is the whole start again:
    /// who is signed in, the list, and the computer to open.
    func poll() async {
        if me == nil || !machinesLoaded {
            await loadAccount(openOne: true, quiet: true)
        } else {
            await refreshMachines()
        }
    }

    /// The list again, for its online states.
    func refreshMachines() async {
        guard let token = signedIn, !Self.isDemoLaunch else { return }
        do {
            let list = try await keeper.call { c, t in try await c.machines(session: t) }
            guard signedIn == token else { return }
            took(list)
        } catch {
            accountFailed(error, token, quiet: true)
        }
    }

    private func took(_ list: [Machine]) {
        machines = list
        machinesLoaded = true
        blocked = false
        if loadFailed {
            loadFailed = false
            accountSaid = nil
        }
    }

    /// A 401 anywhere means signed out; a blocked account says why. Anything
    /// else only shows when there is no list yet to show instead.
    private func accountFailed(_ error: Error, _ token: SessionToken, quiet: Bool) {
        guard signedIn == token else { return }
        switch error as? AccountError {
        case .signedOut?:
            signedOut()
        case .blocked(let reason)?:
            dropMachine()
            blocked = true
            machines = []
            machinesLoaded = true
            accountSaid = Refusal.blocked(reason)
            loadFailed = false
        case .refused(let why)?:
            if !quiet && !machinesLoaded {
                accountSaid = AccountError.refused(why).message
                loadFailed = true
            }
        default:
            if !quiet && !machinesLoaded {
                accountSaid = "Could not reach \(keeper.client.base.host ?? "the account host"). Check the connection and try again."
                loadFailed = true
            }
        }
    }

    func loadDevices() async {
        guard let token = signedIn, !Self.isDemoLaunch else { return }
        devicesError = nil
        do {
            let list = try await keeper.call { c, t in try await c.sessions(session: t) }
            guard signedIn == token else { return }
            devices = list
        } catch {
            accountFailed(error, token, quiet: true)
            guard signedIn == token else { return }
            devicesError = (error as? AccountError)?.message ?? "Could not load the signed-in devices."
        }
    }

    /// Signs one of the account's devices out. This one signs out here too.
    func signOut(_ device: SignedInDevice) {
        if device.current { return signOut() }
        guard let token = signedIn else { return }
        devices.removeAll { $0.id == device.id }
        guard !Self.isDemoLaunch else { return }
        let id = device.id
        Task {
            do {
                try await keeper.call { c, t in try await c.signOutSession(session: t, id: id) }
            } catch {
                accountFailed(error, token, quiet: true)
                if signedIn == token { notice = "Could not sign out \(device.name). Try again." }
            }
            await loadDevices()
        }
    }

    /// Puts an operator's notice away; the account host stops listing it.
    func dismiss(_ n: Notice) {
        me?.notices.removeAll { $0.id == n.id }
        guard let token = signedIn, !Self.isDemoLaunch else { return }
        let id = n.id
        Task {
            do {
                try await keeper.call { c, t in try await c.readNotice(session: t, id: id) }
            } catch {
                // Only a 401 matters here: it signs out. The notice comes back otherwise.
                accountFailed(error, token, quiet: true)
            }
        }
    }

    // ---- pairing on this network ------------------------------------------------

    /// What was scanned or pasted under "Pair on this network". A link to the
    /// computer's own page comes back to open in the browser: that page pairs
    /// the browser, and this app reaches computers only by signing in.
    func pair(_ text: String) -> URL? {
        switch PairingText(text) {
        case .relayLink:
            pairOpened = false
            pairSaid = PairingText.relayLinkSentence
            return nil
        case .direct(let url):
            pairOpened = true
            pairSaid = "The computer's page opened in the browser, which pairs there. This app reaches your computers when you sign in."
            return url
        case .unknown:
            pairOpened = false
            pairSaid = "That is not a pairing link. Scan the code in Arakun's Phone tab."
            return nil
        }
    }

    /// A link another app opened. Nothing happens on its own: a link to a
    /// computer's page comes back for the person to confirm, and anything
    /// else only says what it is.
    func opened(_ text: String) -> URL? {
        let said: String
        switch PairingText(text) {
        case .direct(let url):
            return url
        case .relayLink:
            said = PairingText.relayLinkSentence
        case .unknown:
            said = "That is not a pairing link. Scan the code in Arakun's Phone tab."
        }
        if signedIn == nil {
            pairOpened = false
            pairSaid = said
        } else {
            notice = said
        }
        return nil
    }

    // ---- the computer picked ---------------------------------------------------

    /// Connects to one of the account's computers, offline ones too: the relay
    /// says when it comes back.
    func pick(_ m: Machine) {
        dropMachine()
        machine = m
        accountSaid = nil
        loadFailed = false
        UserDefaults.standard.set(m.label, forKey: Self.lastMachineKey)
        host = m.name
        retry = .milliseconds(500)
        limitTries = 0
        connect()
    }

    /// Back to the list of computers, with a sentence when the relay sent us there.
    func leaveMachine(_ said: String? = nil) {
        dropMachine()
        UserDefaults.standard.removeObject(forKey: Self.lastMachineKey)
        accountSaid = said
        loadFailed = false
        Task { await refreshMachines() }
    }

    /// Lets go of the computer picked, and of all the app knew about it.
    private func dropMachine() {
        link?.close()
        link = nil
        retryTask?.cancel()
        machine = nil
        connection = .idle
        host = ""
        inbox = []
        places = []
        session = nil
        openWhenLinked = nil
        loadingOlder = false
        starting = false
    }

    func connect() {
        guard let token = signedIn, let m = machine else {
            connection = .idle
            return
        }
        #if DEBUG
        if Self.isDemo {
            connection = .connected
            return
        }
        #endif
        retryTask?.cancel()
        link?.close()
        connection = .connecting
        let l = RelayLink(machine: m, base: keeper.client.base, session: token) { [weak self] event in self?.handle(event) }
        link = l
        l.start()
    }

    /// The person asks (Reconnect, Try again): go now instead of waiting out
    /// the backoff, or a limit.
    func reconnectNow() {
        guard machine != nil, connection != .connected else { return }
        retry = .milliseconds(500)
        connect()
    }

    /// Pull to refresh. The inbox is live while connected and there is no
    /// message that asks for it, so a fresh link brings a fresh snapshot; it
    /// also replaces a socket that died without a word.
    func refresh() {
        guard machine != nil else { return }
        retry = .milliseconds(500)
        connect()
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
        guard machine != nil else { return }
        connection = .connecting
        retryTask?.cancel()
        let wait = retry
        retry = min(retry * 2, .seconds(8))
        retryTask = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            self?.connect()
        }
    }

    /// The relay said a limit was reached. It says so again at every try
    /// until the limit lifts, so the banner says it once and the next try
    /// waits minutes, or for the day to turn.
    private func limited(_ which: String) {
        guard machine != nil else { return }
        let wait = LimitWait.seconds(which: which, tries: limitTries)
        limitTries += 1
        connection = .limited(which: which, until: Date().addingTimeInterval(wait))
        retryTask?.cancel()
        retryTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(wait))
            guard !Task.isCancelled else { return }
            self?.connect()
        }
    }

    /// The relay turned the session down at its socket. That alone is no
    /// proof the session is over: a header lost on the way to the relay reads
    /// the same, and signing out on it would delete a good sign-in again and
    /// again. So the account is asked, as the desktop app does, and only its
    /// 401 signs out.
    private func relayRefusedSession() {
        guard let token = signedIn, let m = machine else { return }
        connection = .connecting
        retryTask?.cancel()
        Task { [weak self] in
            guard let self else { return }
            let check = await self.keeper.stillStands()
            guard self.signedIn == token else { return }
            if check == .over { return self.signedOut() }
            // The person picked another computer or asked again meanwhile.
            guard self.machine?.label == m.label, self.link == nil else { return }
            if check == .stands {
                // Dialing again would get the same answer, so it is said once.
                self.leaveMachine("\(m.name) could not be reached with this sign-in: the relay turned it down, though your account accepts it. Try again later, or sign out and sign in again.")
            } else {
                // The account did not answer either: nothing is known yet.
                self.lost()
            }
        }
    }

    private func handle(_ event: RelayLink.Event) {
        switch event {
        case .open:
            interrupted()
            connection = .connected
            retry = .milliseconds(500)
            limitTries = 0
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
            case .signedOut:
                relayRefusedSession()
            case .refused(_, let sentence):
                // A block's reason comes with the list, which leaving reloads.
                leaveMachine(sentence)
            case .untrusted:
                let name = machine?.name ?? "The computer"
                leaveMachine("\(name) did not answer with the key your account has for it. Sign in again in Arakun on that computer.")
            case .limit:
                limited(LimitWait.which(detail))
            case .gone:
                lost()
            }
        }
    }

    private func receive(_ msg: JSON) {
        switch msg["type"].str {
        case "inbox":
            let i = Inbox(msg)
            host = i.host.isEmpty ? machine?.name ?? host : i.host
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

    /// Whether it went out; the composer keeps the text when it did not.
    @discardableResult
    func say(_ text: String) -> Bool {
        guard let s = session, canSend else { return false }
        send(["type": "send", "session": s.id, "text": text])
        return true
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
        guard yes else { return }
        guard machine != nil else {
            // A start that never reached the account is made again.
            if signedIn != nil, me == nil || !machinesLoaded { Task { await poll() } }
            return
        }
        // Coming back on screen is not the person asking: a limit is waited out.
        if case .limited(_, let until) = connection, Date() < until { return }
        reconnectNow()
    }

    var isLinked: Bool { connection == .connected }
}

#if DEBUG
// An account and a computer played here, for screenshots and design work
// without them: launch with `-demo`. `-demoSignIn YES` shows the sign-in
// screen, `-demoMachines YES` the list of computers.
/// The demo keeps nothing: it must not touch a real sign-in in the Keychain.
private struct DemoStore: SessionStore {
    func load() -> SessionToken? { nil }
    func save(_ token: SessionToken?) -> Bool { true }
}

extension AppModel {
    static var isDemo: Bool { ProcessInfo.processInfo.arguments.contains("-demo") }

    func fillDemo() {
        let defaults = UserDefaults.standard
        if defaults.bool(forKey: "demoSignIn") { return }
        let now = Date().timeIntervalSince1970
        func j(_ s: String) -> JSON { JSON.parse(Data(s.utf8))! }
        signedIn = SessionToken("demo")
        me = Me(githubID: 1, login: "octocat", plan: "free",
                notices: [Notice(id: "n1", text: "The relay restarts tonight at 22:00 UTC for a few minutes.")])
        machines = [
            Machine(label: "lmx5lt63vsxoyes4", pub: "", name: "studio", kind: "app", online: true, lastSeenAt: now),
            Machine(label: "q3x7m2kd5tfa6wbe", pub: "", name: "build-box", kind: "server", online: false, lastSeenAt: now - 7200),
        ]
        machinesLoaded = true
        devices = [
            SignedInDevice(id: "d1", name: "Rik's iPhone", kind: "ios", createdAt: now - 86400, lastUsedAt: now, current: true),
            SignedInDevice(id: "d2", name: "studio", kind: "app", createdAt: now - 864000, lastUsedAt: now - 60, current: false),
            SignedInDevice(id: "d3", name: "build-box", kind: "server", createdAt: now - 864000, lastUsedAt: now - 7200, current: false),
        ]
        if defaults.bool(forKey: "demoMachines") { return }
        machine = machines[0]
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
          {"type":"tool","id":"t1","title":"Read cloud/src/relay/relay.ts","kind":"read","status":"completed","diffs":[],"added":0,"removed":0},
          {"type":"tool","id":"t2","title":"rg -n admit cloud/src/relay","kind":"search","status":"completed","command":"rg -n admit cloud/src/relay","output":"cloud/src/relay/relay.ts:412:  admit(phone) {\\ncloud/src/relay/relay.ts:498:    this.admit(p)","diffs":[],"added":0,"removed":0},
          {"type":"agent","text":"Found it. `acceptPhone` deletes the pairing code in `admit()` **before** the limit check. When the check refuses the phone, the code is already gone, so every retry gets `no_pairing`."},
          {"type":"plan","entries":[{"content":"Check the limit before spending the code","status":"completed"},{"content":"Add a test for a refused phone that retries","status":"in_progress"},{"content":"Run the relay tests","status":"pending"}]},
          {"type":"tool","id":"t3","title":"Edit cloud/src/relay/relay.ts","kind":"edit","status":"completed","diffs":[{"path":"cloud/src/relay/relay.ts","added":12,"removed":5}],"added":12,"removed":5}],
         "permission":{"tool_call_id":"p1","title":"cargo test -p arakun relay","options":[{"id":"a","name":"Allow","kind":"allow_once"},{"id":"b","name":"Always allow","kind":"allow_always"},{"id":"c","name":"Deny","kind":"reject_once"}]}}
        """.utf8))!)
        if line?.status == .working { s.permission = nil }
        return s
    }
}
#endif
