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
        /// Asking the account host to mail a code to the address.
        case mailing(String)
        /// The code went to the address, and the box for it is on screen.
        case mailed(String, busy: EmailBusy?)
    }

    /// What the box for a mailed code waits on.
    enum EmailBusy: Equatable {
        /// A new code for the same address.
        case resending
        /// The code typed, which the account host trades for a session.
        case checking
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
    /// The files the session on screen changed, turn by turn, once asked for.
    private(set) var changes: [ChangedTurn]?
    /// The session's diffs that came, by the place each was asked by.
    private(set) var diffs: [DiffRef: FileDiff] = [:]
    /// The subagents of the session on screen, as the computer last listed them.
    private(set) var subagents: [Subagent] = []
    /// The subagent whose transcript is open, kept live by the computer.
    private(set) var subagent: SubagentTranscript?
    var loadingOlderSubagent = false
    /// A message worth a moment on screen.
    var notice: String?
    /// A session asked for before the link was up (from a started session).
    private var openWhenLinked: String?
    /// A start sent and not yet answered with `started` or `error`. Here, not
    /// in the form, so closing and reopening it cannot send a second start.
    private(set) var starting = false

    /// Whether the person turned notifications on for this phone.
    private(set) var notifying = UserDefaults.standard.bool(forKey: PushSetup.wantedKey)
    /// What the system says about notifications for Arakun.
    private(set) var permission: PushPermission = .ask
    /// Which notices the computer picked sends this phone; nil until it says.
    private(set) var notices: NoticeChoice?
    /// The sessions the computer picked sends this phone no notices about.
    private(set) var muted: [String] = []
    /// Whether the computer knows muting; one from before has no `/mute`.
    private(set) var canMute = false
    /// APNs's token for this install. It comes again at every launch.
    private var pushToken: Data?
    /// A session a notification tap asked for, until a computer is picked.
    private var openAfterPick: String?

    private var link: RelayLink?
    /// The pictures of the session on screen. Each has its own slot to watch,
    /// so the model does not observe them as a whole.
    @ObservationIgnored let pictures = PictureCache()
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
    private static let pairedByLinkKey = "arakun.pairedByLink"

    /// This phone was paired by link before sign-in, and that pairing was
    /// forgotten: the sign-in screen says why, until a sign-in works.
    private(set) var pairedByLink = UserDefaults.standard.bool(forKey: AppModel.pairedByLinkKey)

    init(store: any SessionStore = Keychain.session) {
        #if DEBUG
        if AppModel.isDemo {
            keeper = AccountSession(store: DemoStore())
            fillDemo()
            return
        }
        #endif
        if Keychain.forgetOldPairing() {
            UserDefaults.standard.set(true, forKey: Self.pairedByLinkKey)
            pairedByLink = true
        }
        keeper = AccountSession(store: store)
        if !UserDefaults.standard.bool(forKey: Self.hasRunKey) {
            UserDefaults.standard.set(true, forKey: Self.hasRunKey)
            // Deleting the app must sign the phone out: installed again, it starts signed out.
            keeper.signOut()
            // The Keychain kept the push keys too, which no computer should still use.
            PushSetup.keys.forget()
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

    // ---- signing in by email ----------------------------------------------------

    /// Mails a six-digit code to `address`, then shows the box for it. From
    /// that box, it mails a new code to the same address.
    func emailCode(to address: String) {
        let address = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty else { return }
        switch signIn {
        case nil: signIn = .mailing(address)
        case .mailed(let shown, busy: nil)? where shown == address: signIn = .mailed(address, busy: .resending)
        default: return
        }
        signInError = nil
        pairSaid = nil
        #if DEBUG
        if Self.isDemo {
            signIn = .mailed(address, busy: nil)
            return
        }
        #endif
        signInAttempt += 1
        let attempt = signInAttempt
        let client = keeper.client
        signInTask = Task { [weak self] in
            do {
                try await client.sendEmailCode(email: address)
                guard let self, self.isCurrent(attempt) else { return }
                self.signInTask = nil
                self.signIn = .mailed(address, busy: nil)
            } catch {
                guard let self, self.isCurrent(attempt), !(error is CancellationError) else { return }
                self.emailFailed(error, address: address)
            }
        }
    }

    /// Trades the code typed in the box for the account's session.
    func checkCode(_ code: String) {
        guard case .mailed(let address, busy: nil)? = signIn else { return }
        signInError = nil
        #if DEBUG
        if Self.isDemo { return }
        #endif
        signIn = .mailed(address, busy: .checking)
        signInAttempt += 1
        let attempt = signInAttempt
        let keeper = self.keeper
        let name = deviceName
        signInTask = Task { [weak self] in
            do {
                // In a task of its own, which Cancel does not reach, as for GitHub.
                let signed = try await Task { try await keeper.signIn(email: address, code: code, name: name, kind: "ios") }.value
                guard let self, self.isCurrent(attempt) else {
                    if keeper.token == signed.session { keeper.signOut() }
                    return
                }
                self.tookSignIn(signed)
            } catch {
                guard let self, self.isCurrent(attempt), !(error is CancellationError) else { return }
                self.emailFailed(error, address: address)
            }
        }
    }

    /// A wrong code, or a new one that did not go, leaves the box on screen to
    /// try again; a first code that did not go goes back to the address.
    private func emailFailed(_ error: Error, address: String) {
        signInTask = nil
        if case .mailed(_, _)? = signIn { signIn = .mailed(address, busy: nil) } else { signIn = nil }
        signInError = (error as? AccountError).map(Self.emailSentence) ?? "Sign-in did not work. Try again."
    }

    /// The account host's own words about an email sign-in, as a sentence:
    /// "the code is wrong or has run out" says more than "Sign-in did not work".
    private nonisolated static func emailSentence(_ e: AccountError) -> String {
        guard case .refused(let why) = e, !why.isEmpty, !why.hasPrefix("HTTP ") else { return sentence(e) }
        let said = why.hasSuffix(".") ? String(why.dropLast()) : why
        return said.prefix(1).uppercased() + said.dropFirst() + "."
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
        UserDefaults.standard.removeObject(forKey: Self.pairedByLinkKey)
        pairedByLink = false
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
        forgetPush()
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
            if let m = list.first(where: { $0.label == last }) {
                pick(m)
            } else if openAfterPick != nil {
                openAfterPick = nil
                accountSaid = Self.tapSentence
            }
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
        if let id = openAfterPick {
            openAfterPick = nil
            open(id)
        }
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
        forgetChanges()
        starting = false
        notices = nil
        muted = []
        canMute = false
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
        loadingOlderSubagent = false
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
            // A new connection watches nothing: the open subagent is asked for again.
            if let t = subagent { link?.send(["type": "subagent", "session": t.session, "subagent": t.id]) }
            if let id = openWhenLinked {
                openWhenLinked = nil
                open(id)
            }
            linkedPush()
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
        case "subagents":
            guard session?.id == msg["id"].str else { return }
            subagents = Subagent.list(msg)
        case "subagent_entries":
            guard subagent?.owns(msg) == true else { return }
            if msg["older"].bool { loadingOlderSubagent = false }
            subagent?.apply(msg)
        case "changes":
            guard session?.id == msg["id"].str else { return }
            changes = ChangedTurn.list(msg)
        case "diff":
            guard session?.id == msg["id"].str, let d = FileDiff(msg) else { return }
            diffs[d.ref] = d
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
        forgetChanges()
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
        forgetChanges()
        send(["type": "close"])
    }

    /// Fetches a picture of the session on screen through the relay, as the
    /// phone page does, unless it is already here. Only the computer's own
    /// signed links: nothing the agent wrote is loaded from elsewhere.
    func loadPicture(_ picture: Picture) async {
        guard picture.served else { return }
        await pictures.load(picture.url) { [weak self] url in
            guard let l = self?.link, l.isReady,
                  let reply = try? await l.fetch(url), reply.status == 200
            else { return nil }
            return reply.body
        }
    }

    /// They belong to the session on screen and go with it.
    private func forgetChanges() {
        pictures.reset()
        changes = nil
        diffs = [:]
        subagents = []
        subagent = nil
        loadingOlderSubagent = false
    }

    /// Opens a subagent's transcript: its last entries, then every change, fill `subagent`.
    func openSubagent(_ id: String) {
        guard let s = session else { return }
        subagent = SubagentTranscript(session: s.id, id: id)
        loadingOlderSubagent = false
        send(["type": "subagent", "session": s.id, "subagent": id])
    }

    func closeSubagent() {
        guard let t = subagent else { return }
        subagent = nil
        loadingOlderSubagent = false
        send(["type": "subagent", "session": t.session])
    }

    func loadOlderSubagent() {
        guard let t = subagent, t.from > 0, !loadingOlderSubagent, canSend else { return }
        loadingOlderSubagent = true
        send(["type": "entries", "session": t.session, "subagent": t.id, "from": max(0, t.from - 50), "to": t.from])
    }

    /// Asks for the list of what the session changed; the answer fills `changes`.
    func loadChanges() {
        guard let s = session else { return }
        #if DEBUG
        if Self.isDemo {
            changes = demoChanges()
            return
        }
        #endif
        send(["type": "changes", "session": s.id])
    }

    /// Asks for the diffs of `refs` that have not come yet; the answers fill `diffs`.
    func loadDiffs(_ refs: [DiffRef]) {
        guard let s = session else { return }
        for r in refs where diffs[r] == nil {
            #if DEBUG
            if Self.isDemo {
                diffs[r] = demoDiff(r)
                continue
            }
            #endif
            send(["type": "diff", "session": s.id, "entry": r.entry, "index": r.index])
        }
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

    /// Sends what was typed for a secret, or declines it with no `values`.
    /// Only a private connection carries values; the card offers Save only then.
    /// Whether it went out: offline the card keeps what was typed.
    @discardableResult
    func answerSecret(_ ask: SecretAsk, values: [String]?) -> Bool {
        guard let s = session, canSend else { return false }
        var msg: [String: Any] = ["type": "secret", "session": s.id, "id": ask.id]
        if let values, ask.isPrivate { msg["values"] = values }
        send(msg)
        if session?.secret?.id == ask.id { session?.secret = nil }
        return true
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
        // Notifications may have been turned off or on in Settings meanwhile.
        if !Self.isDemoLaunch { Task { permission = await PushPermission.current() } }
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

    // ---- notifications --------------------------------------------------------

    /// What the list says after a tap it could not follow to a computer: a
    /// notice does not say which computer sent it.
    static let tapSentence = "Pick the computer that notification came from to open its session."

    /// At launch: APNs gives the token again, and a new one goes to the computer.
    func launched() {
        guard !Self.isDemoLaunch else { return }
        Task { permission = await PushPermission.current() }
        if notifying { UIApplication.shared.registerForRemoteNotifications() }
    }

    /// Notifications on or off for this phone, from Settings. On asks the
    /// system first; off tells the computer open now, and APNs then answers
    /// every other computer that the token is gone, so they forget it too.
    func setNotifying(_ on: Bool) {
        #if DEBUG
        if Self.isDemo {
            notifying = on
            return
        }
        #endif
        guard on else {
            wantNotifications(false)
            UIApplication.shared.unregisterForRemoteNotifications()
            pushToken = nil
            if let l = link, l.isReady, let body = Self.text(["subscription": NSNull()]) {
                Task { await post(body, on: l) }
            }
            return
        }
        Task {
            let granted = await PushPermission.request()
            permission = await PushPermission.current()
            // Refused: the switch stays off, and Settings says where to allow them.
            guard granted else { return }
            guard PushSetup.keys.loadOrMake() != nil else {
                notice = "Notifications could not be turned on: the Keychain did not keep their keys."
                return
            }
            wantNotifications(true)
            // The token comes back through tookPushToken, which tells the computer.
            UIApplication.shared.registerForRemoteNotifications()
        }
    }

    private func wantNotifications(_ on: Bool) {
        notifying = on
        UserDefaults.standard.set(on, forKey: PushSetup.wantedKey)
    }

    func tookPushToken(_ token: Data) {
        pushToken = token
        guard notifying, let l = link, l.isReady, let body = subscriptionBody() else { return }
        Task { await post(body, on: l) }
    }

    /// One kind of notice on or off; the computer keeps the choice.
    func setNotices(_ choice: NoticeChoice) {
        #if DEBUG
        if Self.isDemo {
            notices = choice
            return
        }
        #endif
        guard let before = notices, let l = link, l.isReady, let body = Self.text(["notices": choice.json]) else { return }
        notices = choice
        Task {
            let saved = await post(body, on: l)
            guard !saved, link === l else { return }
            notices = before
            notice = "Could not save that. Try again."
        }
    }

    /// Whether the computer sends no notices about the session `id`.
    func isMuted(_ id: String) -> Bool { muted.contains(id) }

    /// One session's notices off, or on again; the computer keeps the list
    /// and answers with all of it.
    func mute(session id: String, muted on: Bool) {
        #if DEBUG
        if Self.isDemo {
            muted.removeAll { $0 == id }
            if on { muted.append(id) }
            return
        }
        #endif
        guard let l = link, l.isReady, let body = Self.text(Mute.body(session: id, muted: on)) else {
            notice = "Could not save that. Try again."
            return
        }
        Task {
            guard let reply = try? await l.fetch("/mute", method: "POST", body: body), reply.status == 200,
                  let j = JSON.parse(reply.body), let list = Mute.list(j)
            else {
                if link === l { notice = "Could not save that. Try again." }
                return
            }
            if link === l { muted = list }
        }
    }

    /// Whether a notice about `id` would show what is already on screen.
    func isOnScreen(_ id: String?) -> Bool {
        guard let id else { return false }
        return session?.id == id
    }

    /// A tap on a notification: its session, on the computer open now. With
    /// none open yet, the one the app would open at launch, as phone/relay.js
    /// `tapRoute` does.
    func tapped(session id: String) {
        guard signedIn != nil, !id.isEmpty, !blocked else { return }
        if machine != nil { return open(id) }
        openAfterPick = id
        // Still loading: `loadAccount` picks a computer, which opens it.
        guard machinesLoaded else { return }
        let last = UserDefaults.standard.string(forKey: Self.lastMachineKey)
        if let m = machines.count == 1 ? machines.first : machines.first(where: { $0.label == last }) {
            pick(m)
        } else {
            openAfterPick = nil
            if !machines.isEmpty { accountSaid = Self.tapSentence }
        }
    }

    /// The link is up: the computer says which notices it sends, and learns
    /// where to send them, or that this phone wants none any more.
    private func linkedPush() {
        guard let l = link, !Self.isDemoLaunch else { return }
        Task {
            guard let reply = try? await l.fetch("/me"), reply.status == 200,
                  let me = JSON.parse(reply.body), let state = PushState(me), link === l
            else { return }
            notices = state.notices
            muted = state.muted
            canMute = state.canMute
            if notifying {
                if let body = subscriptionBody() { await post(body, on: l) }
            } else if state.notifying, let body = Self.text(["subscription": NSNull()]) {
                await post(body, on: l)
            }
        }
    }

    /// `{"subscription": …}` for `POST /push`: this phone's keys, and where
    /// the Worker takes its notices for APNs. Nil until APNs gave a token.
    private func subscriptionBody() -> String? {
        guard let token = pushToken, let keys = PushSetup.keys.loadOrMake(),
              let endpoint = PushEndpoint.url(base: keeper.client.base, token: token, sandbox: PushSetup.sandbox)
        else { return nil }
        return Self.text(["subscription": keys.subscription(endpoint: endpoint)])
    }

    /// `POST /push` to the computer of `l`. Its answer says which notices it
    /// sends. Whether it took the change.
    @discardableResult
    private func post(_ body: String, on l: RelayLink) async -> Bool {
        guard let reply = try? await l.fetch("/push", method: "POST", body: body), reply.status == 200 else { return false }
        if link === l, let j = JSON.parse(reply.body), let state = PushState(j) { notices = state.notices }
        return true
    }

    private nonisolated static func text(_ obj: [String: Any]) -> String? {
        (try? JSONSerialization.data(withJSONObject: obj)).map { String(decoding: $0, as: UTF8.self) }
    }

    /// Signed out: no computer may reach this phone any more. APNs tells them
    /// the token is gone, and notices already on the way can no longer be
    /// read. The next sign-in makes new keys.
    private func forgetPush() {
        notices = nil
        openAfterPick = nil
        guard !Self.isDemoLaunch else { return }
        if notifying {
            wantNotifications(false)
            UIApplication.shared.unregisterForRemoteNotifications()
        }
        pushToken = nil
        PushSetup.keys.forget()
    }
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
        notifying = true
        permission = .granted
        if defaults.bool(forKey: "demoMachines") { return }
        machine = machines[0]
        connection = .connected
        notices = NoticeChoice(done: false)
        muted = ["s3"]
        canMute = true
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
        // -demoSecret private|clear: the secret card in place of the permission.
        if let mode = UserDefaults.standard.string(forKey: "demoSecret"), line?.status != .working {
            s.permission = nil
            s.secret = SecretAsk(JSON.parse(Data("""
            {"id":1,"label":"Cloudflare API token","reason":"To deploy the relay with wrangler.","url":"https://dash.cloudflare.com/profile/api-tokens",
             "fields":[{"name":"token","label":"Token"}],"private":\(mode == "private")}
            """.utf8))!)
        }
        return s
    }

    /// What the sample session changed, as the computer would list it.
    func demoChanges() -> [ChangedTurn] {
        let at = Date().timeIntervalSince1970 - 600
        return ChangedTurn.list(JSON.parse(Data("""
        {"turns":[
         {"entry":0,"text":"Pairing links fail with limit:phones. Find out why and fix it.","at":\(at),"files":[
          {"path":"cloud/src/relay/relay.ts","name":"relay.ts","added":12,"removed":5,"diffs":[[6,0],[6,1]]},
          {"path":"cloud/test/relay.test.ts","name":"relay.test.ts","added":28,"removed":0,"diffs":[[7,0]]},
          {"path":"README.md","name":"README.md","added":2,"removed":2,"diffs":[[8,0]]}]}]}
        """.utf8))!)
    }

    /// One of the sample diffs; they differ only in where they are.
    func demoDiff(_ ref: DiffRef) -> FileDiff? {
        FileDiff(JSON.parse(Data("""
        {"entry":\(ref.entry),"index":\(ref.index),"path":"cloud/src/relay/relay.ts","name":"relay.ts","added":4,"removed":3,
         "new_file":false,"deleted":false,"cut":0,"lines":[
          "@@ -409,9 +409,10 @@ export class Relay {",
          [" ",409,"  acceptPhone(phone: Phone, code: string) {"],
          [" ",410,"    const pairing = this.pairings.get(code);"],
          [" ",411,"    if (!pairing) return refuse(phone, \\"no_pairing\\");"],
          ["-",412,"    this.admit(phone);"],
          ["-",413,"    this.pairings.delete(code);"],
          ["-",414,"    if (this.phones.size > this.limit) return refuse(phone, \\"limit:phones\\");"],
          ["+",412,"    // A refused phone keeps its code, so it can try again when there is room."],
          ["+",413,"    if (this.phones.size >= this.limit) return refuse(phone, \\"limit:phones\\");"],
          ["+",414,"    this.pairings.delete(code);"],
          ["+",415,"    this.admit(phone);"],
          [" ",416,"    return welcome(phone, pairing.host);"],
          [" ",417,"  }"]]}
        """.utf8))!)
    }
}
#endif
