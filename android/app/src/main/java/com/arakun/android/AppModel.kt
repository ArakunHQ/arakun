package com.arakun.android

import android.app.Application
import android.os.Build
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.arakun.kit.Account
import com.arakun.kit.AccountClient
import com.arakun.kit.AccountError
import com.arakun.kit.AccountKeeper
import com.arakun.kit.AccountSession
import com.arakun.kit.ChangedTurn
import com.arakun.kit.DeviceCode
import com.arakun.kit.DiffRef
import com.arakun.kit.EndKind
import com.arakun.kit.FileDiff
import com.arakun.kit.Inbox
import com.arakun.kit.InboxSession
import com.arakun.kit.LimitWait
import com.arakun.kit.LinkKind
import com.arakun.kit.LiveSession
import com.arakun.kit.Machine
import com.arakun.kit.Me
import com.arakun.kit.Notice
import com.arakun.kit.PermissionAsk
import com.arakun.kit.Photos
import com.arakun.kit.Place
import com.arakun.kit.Poll
import com.arakun.kit.RelayLink
import com.arakun.kit.SessionCheck
import com.arakun.kit.SignedIn
import com.arakun.kit.SignedInDevice
import com.arakun.kit.Subagent
import com.arakun.kit.SubagentTranscript
import com.arakun.kit.WebPush
import com.arakun.kit.bool
import com.arakun.kit.get
import com.arakun.kit.parseJson
import com.arakun.kit.str
import com.arakun.kit.string
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject

/**
 * Everything the app shows, and the one link to the computer behind it. The
 * account says which computers there are; the one picked sends a snapshot when
 * the link opens and changes from then on. It all runs on the main thread.
 */
class AppModel(app: Application) : AndroidViewModel(app) {
    sealed interface Connection {
        /** No computer picked. */
        data object Idle : Connection
        data object Connecting : Connection
        data object Connected : Connection

        /** The relay is there, the computer is not (last seen `since`, ms). */
        data class ComputerAway(val since: Long?) : Connection

        /**
         * The relay said a limit was reached (`which`: `daily`, `phones`, …).
         * Nothing is tried before `until` (ms), unless the person asks.
         */
        data class Limited(val which: String, val until: Long) : Connection
    }

    /** How far signing in with GitHub or a code by email has come. */
    sealed interface SignIn {
        /** Asking GitHub for a code. */
        data object Asking : SignIn

        /** The code to enter at `uri`, while the app waits for GitHub. */
        data class Code(val userCode: String, val uri: String) : SignIn

        /** GitHub said yes; the account host makes the session. */
        data object Finishing : SignIn

        /** Asking the account host to mail a code to [email]. */
        data class Mailing(val email: String) : SignIn

        /** The code went to [email], and the box for it is on screen. */
        data class Mailed(val email: String, val busy: EmailBusy? = null) : SignIn
    }

    /** What the box for a mailed code waits on. */
    enum class EmailBusy {
        /** A new code for the same address. */
        RESENDING,

        /** The code typed, which the account host trades for a session. */
        CHECKING,
    }

    private val keeper = AccountKeeper(CredentialStore(app))

    /** The account's session on this phone; null shows the sign-in screen. */
    var account by mutableStateOf<AccountSession?>(null)
        private set

    /** This phone was paired by link before sign-in, and that was forgotten: the sign-in screen says why. */
    var pairedByLink by mutableStateOf(false)
        private set

    var me by mutableStateOf<Me?>(null)
        private set
    var machines by mutableStateOf<List<Machine>>(emptyList())
        private set

    /** The list came at least once since sign-in. */
    var machinesLoaded by mutableStateOf(false)
        private set

    /** Why the account host will not list the computers: the account is blocked. */
    var blocked by mutableStateOf<String?>(null)
        private set

    /** What went wrong signing in or listing the computers, for those screens. */
    var accountSaid by mutableStateOf<String?>(null)
        private set
    var signingIn by mutableStateOf<SignIn?>(null)
        private set

    /** The account's signed-in devices, once asked for. */
    var devices by mutableStateOf<List<SignedInDevice>?>(null)
        private set
    var devicesSaid by mutableStateOf<String?>(null)
        private set

    /** The computer picked; null shows the list. */
    var machine by mutableStateOf<Machine?>(null)
        private set
    var connection by mutableStateOf<Connection>(Connection.Idle)
        private set
    var host by mutableStateOf("")
        private set
    var inbox by mutableStateOf<List<InboxSession>>(emptyList())
        private set
    var places by mutableStateOf<List<Place>>(emptyList())
        private set

    /** The session on screen, kept live by the computer. */
    var session by mutableStateOf<LiveSession?>(null)
        private set
    var loadingOlder by mutableStateOf(false)
        private set

    /** The subagents of the session on screen, as the computer last listed them. */
    var subagents by mutableStateOf<List<Subagent>>(emptyList())
        private set

    /** The subagent whose transcript is open, kept live by the computer. */
    var subagent by mutableStateOf<SubagentTranscript?>(null)
        private set
    var loadingOlderSubagent by mutableStateOf(false)
        private set

    /** The files the session on screen changed, turn by turn, once asked for. */
    var changes by mutableStateOf<List<ChangedTurn>?>(null)
        private set

    /** The session's diffs that came, by the place each was asked by. */
    var diffs by mutableStateOf<Map<DiffRef, FileDiff>>(emptyMap())
        private set

    /** A message worth a moment on screen. */
    var notice by mutableStateOf<String?>(null)

    /** The sessions the computer open sends this phone no notices about. */
    var muted by mutableStateOf<List<String>>(emptyList())
        private set

    /** Whether the computer open knows muting; one from before has no `/mute`. */
    var knowsMuting by mutableStateOf(false)
        private set

    /** Sample data instead of a computer (debug builds, launched with `demo`). */
    var demo: Demo? = null
        private set

    /** The demo asked for the New session sheet, not yet shown. */
    var demoNew = false

    /** A session asked for before the link was up (from a started session). */
    private var openWhenLinked: String? = null

    /** A notification tapped while no computer was open: its session opens on the one opened last. */
    private var tappedSession: String? = null

    /** The app is on screen, so the session it shows needs no notification. */
    private var foreground by mutableStateOf(false)

    /** A start sent and not yet answered with `started` or `error`. */
    var starting by mutableStateOf(false)
        private set

    /**
     * The session on screen when the start went out. The started one opens
     * only if the user is still there, not over a session they picked since.
     */
    private var startedFrom: String? = null

    /**
     * The link dropped while a start waited for its answer: the computer may
     * have started the session. Starting again then needs the user's say-so.
     */
    var startUnsure by mutableStateOf(false)
        private set

    private var link: RelayLink? = null
    private var retry = 500L
    private var retryJob: Job? = null

    /** Limits the relay answered with in a row, for the wait before the next try. */
    private var limitTries = 0
    private var signInJob: Job? = null
    private var listJob: Job? = null

    /** [accountSaid] says the list failed to load, so the next good answer clears it. */
    private var listFailed = false

    /** Open the computer opened last once the list comes: at launch and after sign-in. */
    private var reopen = false
    private var begun = false

    /** The link's own thread: the main one, posted to in order. */
    private val linkScope = CoroutineScope(viewModelScope.coroutineContext + Dispatchers.Main)

    /** The pictures of the session on screen; the computer's come through the link. */
    val pictures = Pictures(linkScope) { path ->
        val l = link?.takeIf { it.isReady } ?: return@Pictures null
        try {
            l.fetch(path)
        } catch (_: RelayLink.ClosedException) {
            null
        }
    }

    init {
        viewModelScope.launch {
            snapshotFlow { if (foreground) session?.id else null }.collect { id ->
                Push.onScreen = id
                if (id != null && demo == null) Push.seen(app, id)
            }
        }
        // FCM's new token goes to the computer linked now; the others get it when they link.
        viewModelScope.launch {
            Push.newToken.collect { token -> if (token != null) link?.let { tellPush(it, token) } }
        }
    }

    /** Once, when the first screen comes up. */
    fun begin(demo: Demo?) {
        if (begun) return
        begun = true
        if (demo != null) {
            this.demo = demo
            if (!demo.signIn) fillDemo(openFirst = !demo.machines)
            demo.open?.let(::open)
            demoNew = demo.new
            return
        }
        keeper.forgetOldPairing()
        pairedByLink = keeper.pairedByLink
        account = keeper.load()
        if (account != null) loadAccount(reopen = true)
    }

    /** The name this phone's session goes by in the account's list of signed-in devices. */
    private val sessionName: String
        get() = Build.MODEL?.trim()?.takeIf { it.isNotEmpty() }?.take(100) ?: "Android"

    val isLinked: Boolean get() = connection == Connection.Connected

    // ---- the account -------------------------------------------------------

    private suspend fun <T> io(block: () -> T): T = withContext(Dispatchers.IO) { block() }

    /**
     * Signs in with GitHub's device flow: the app shows a code, the person
     * enters it on GitHub, and the app polls until GitHub says yes.
     */
    fun signIn() {
        if (demo != null || signInJob?.isActive == true) return
        accountSaid = null
        signingIn = SignIn.Asking
        signInJob = viewModelScope.launch {
            val api = AccountClient(Account.HOST)
            try {
                val clientId = io { api.clientId() }
                val code = io { api.startDeviceFlow(clientId) }
                // Only GitHub's own page: the address came over the network.
                val uri = code.verificationUri.takeIf { it.startsWith("https://github.com/") } ?: Account.VERIFY_URL
                signingIn = SignIn.Code(code.userCode, uri)
                val token = waitForGitHub(api, clientId, code)
                if (token == null) {
                    signingIn = null
                    return@launch
                }
                signingIn = SignIn.Finishing
                // Not cut off by Cancel: a session made on the account host
                // must either be kept here or be signed out there.
                var made: SignedIn? = null
                try {
                    withContext(NonCancellable + Dispatchers.IO) { made = api.signIn(token, sessionName) }
                } finally {
                    if (!isActive) made?.let { forgetSession(api.base, it.session) }
                }
                val signed = made
                if (!isActive || signed == null) return@launch
                tookSignIn(api, signed)
            } catch (e: AccountError) {
                // Cancelled meanwhile: the person no longer waits for an answer.
                if (!isActive) return@launch
                signingIn = null
                accountSaid = if (e is AccountError.Unreachable) {
                    "Could not reach GitHub or ${Account.HOST.substringAfter("://")}. Check the connection and try again."
                } else {
                    sentence(e)
                }
            }
        }
    }

    /** GitHub's token once the person said yes, or null with [accountSaid] set. */
    private suspend fun waitForGitHub(api: AccountClient, clientId: String, code: DeviceCode): String? {
        var interval = code.intervalMs
        val until = System.currentTimeMillis() + code.expiresInMs
        while (true) {
            delay(interval)
            if (System.currentTimeMillis() >= until) {
                accountSaid = "The code ran out before it was entered on GitHub. Try again."
                return null
            }
            val poll = try {
                io { api.pollOnce(clientId, code.deviceCode, interval) }
            } catch (_: AccountError.Unreachable) {
                // A phone changes networks; the code is good until it runs out.
                continue
            }
            when (poll) {
                is Poll.Wait -> interval = poll.intervalMs
                is Poll.Token -> return poll.token
                Poll.Expired -> {
                    accountSaid = "The code ran out before it was entered on GitHub. Try again."
                    return null
                }
                Poll.Denied -> {
                    accountSaid = "The sign-in was turned down on GitHub."
                    return null
                }
                is Poll.Failed -> {
                    accountSaid = poll.message
                    return null
                }
            }
        }
    }

    /** Keeps the session the account host made, and lists the account's computers. */
    private fun tookSignIn(api: AccountClient, signed: SignedIn) {
        val s = AccountSession(api.base, signed.session, signed.sessionId, signed.login)
        if (!keeper.save(s)) {
            notice = "Could not keep the sign-in on this phone. You may need to sign in again after the app restarts."
        }
        signingIn = null
        account = s
        keeper.pairedByLink = false
        pairedByLink = false
        loadAccount(reopen = true)
    }

    /**
     * Mails a six-digit code to `email`, then shows the box for it. From that
     * box, it mails a new code to the same address.
     */
    fun emailCode(email: String) {
        val address = email.trim()
        if (address.isEmpty() || signInJob?.isActive == true) return
        val step = signingIn
        signingIn = when {
            step == null -> SignIn.Mailing(address)
            step is SignIn.Mailed && step.busy == null && step.email == address -> step.copy(busy = EmailBusy.RESENDING)
            else -> return
        }
        accountSaid = null
        if (demo != null) {
            signingIn = SignIn.Mailed(address)
            return
        }
        signInJob = viewModelScope.launch {
            val api = AccountClient(Account.HOST)
            try {
                io { api.sendEmailCode(address) }
                signingIn = SignIn.Mailed(address)
            } catch (e: AccountError) {
                if (!isActive) return@launch
                emailFailed(e, address)
            }
        }
    }

    /** Trades the code typed in the box for the account's session. */
    fun checkCode(code: String) {
        val step = signingIn as? SignIn.Mailed ?: return
        if (step.busy != null || demo != null || signInJob?.isActive == true) return
        accountSaid = null
        signingIn = step.copy(busy = EmailBusy.CHECKING)
        signInJob = viewModelScope.launch {
            val api = AccountClient(Account.HOST)
            try {
                // Not cut off by Cancel, as for GitHub: a session made on the
                // account host must either be kept here or be signed out there.
                var made: SignedIn? = null
                try {
                    withContext(NonCancellable + Dispatchers.IO) { made = api.signInWithCode(step.email, code, sessionName) }
                } finally {
                    if (!isActive) made?.let { forgetSession(api.base, it.session) }
                }
                val signed = made
                if (!isActive || signed == null) return@launch
                tookSignIn(api, signed)
            } catch (e: AccountError) {
                if (!isActive) return@launch
                emailFailed(e, step.email)
            }
        }
    }

    /**
     * A wrong code, or a new one that did not go, leaves the box on screen to
     * try again; a first code that did not go goes back to the address.
     */
    private fun emailFailed(e: AccountError, email: String) {
        signingIn = if (signingIn is SignIn.Mailed) SignIn.Mailed(email) else null
        accountSaid = emailSentence(e)
    }

    /**
     * The account host's own words about an email sign-in, as a sentence:
     * "the code is wrong or has run out" says more than "the server answered".
     */
    private fun emailSentence(e: AccountError): String {
        val said = (e as? AccountError.Refused)?.said?.trimEnd('.')
        if (said.isNullOrEmpty() || said.startsWith("HTTP ")) return sentence(e)
        return said.replaceFirstChar { it.uppercase() } + "."
    }

    fun cancelSignIn() {
        signInJob?.cancel()
        signInJob = null
        signingIn = null
    }

    /**
     * Who is signed in and the account's computers. With `reopen`, the one
     * opened last (or the only one) opens straight away.
     */
    fun loadAccount(reopen: Boolean = false) {
        val s = account ?: return
        if (demo != null) return
        if (reopen) this.reopen = true
        // One at a time: the answer under way serves this ask too.
        if (listJob?.isActive == true) return
        listJob = viewModelScope.launch {
            val api = AccountClient(s.host)
            try {
                val who = io { api.me(s.token) }
                // Signed out meanwhile: this answer is for nobody.
                if (account != s) return@launch
                // Before the list: a blocked account has no list, and still
                // sees who is signed in and the operator's notices.
                me = who
                val list = io { api.machines(s.token) }
                if (account != s) return@launch
                machines = list
                machinesLoaded = true
                blocked = null
                // A refusal the relay sent us back with stays until the next pick.
                if (listFailed) accountSaid = null
                listFailed = false
                if (this@AppModel.reopen && machine == null) {
                    val last = keeper.machine
                    (list.singleOrNull() ?: list.firstOrNull { it.label == last })?.let(::pick)
                }
                this@AppModel.reopen = false
                // Not opened on a computer the person picks later: it may not be that one's.
                tappedSession = null
            } catch (e: AccountError) {
                if (account == s) accountFailed(e)
            }
        }
    }

    /** What any failed call to the account host means for the screens. */
    private fun accountFailed(e: AccountError) {
        when (e) {
            is AccountError.SignedOut -> signedOut(SIGNED_OUT)
            is AccountError.Blocked -> blocked = blockedSentence(e.reason)
            else -> {
                accountSaid = sentence(e)
                listFailed = true
            }
        }
    }

    private fun sentence(e: AccountError): String = when (e) {
        is AccountError.Unreachable -> "Could not reach ${Account.HOST.substringAfter("://")}. Check the connection and try again."
        is AccountError.Blocked -> blockedSentence(e.reason)
        else -> e.message ?: "Something went wrong. Try again."
    }

    private fun blockedSentence(reason: String) =
        EndKind.sentence("blocked", null, "") + if (reason.isNotEmpty()) " $reason" else ""

    /** Opens one of the account's computers, offline ones too: the relay says when it comes back. */
    fun pick(m: Machine) {
        reopen = false
        val tapped = tappedSession
        tappedSession = null
        if (machine?.label != m.label) dropMachine()
        machine = m
        accountSaid = null
        listFailed = false
        host = m.name
        if (demo != null) {
            fillDemoInbox()
            return
        }
        keeper.machine = m.label
        retry = 500
        limitTries = 0
        connect()
        tapped?.let(::open)
    }

    private fun forgetPicked() {
        if (demo == null) keeper.machine = null
    }

    /** Back to the list of computers, with a sentence when the relay sent us there. */
    fun leaveMachine(said: String? = null) {
        dropMachine()
        accountSaid = said
        listFailed = false
        loadAccount()
    }

    /** Lets go of the computer picked, and of all the app knew about it. */
    private fun dropMachine() {
        link?.close()
        link = null
        retryJob?.cancel()
        machine = null
        muted = emptyList()
        knowsMuting = false
        connection = Connection.Idle
        host = ""
        inbox = emptyList()
        places = emptyList()
        session = null
        openWhenLinked = null
        loadingOlder = false
        forgetChanges()
        starting = false
        startUnsure = false
    }

    /** The session is gone (signed out here or elsewhere): only sign-in is left. */
    private fun signedOut(said: String? = null) {
        dropMachine()
        listJob?.cancel()
        reopen = false
        cancelSignIn()
        account = null
        // Notices for this sign-in stop here; the next sign-in's first link gets a new token.
        if (demo == null) Push.dropToken()
        // The demo's account is not the one kept on this phone.
        if (demo == null && !keeper.save(null)) {
            // Left on disk, the next launch would be signed in again.
            notice = "Could not remove the sign-in from this phone. Restart the app and sign out again."
        }
        me = null
        machines = emptyList()
        machinesLoaded = false
        blocked = null
        devices = null
        devicesSaid = null
        accountSaid = said
        listFailed = false
    }

    /** Signs this phone out of the account; the session ends on the account host too. */
    fun signOut() {
        val s = account
        signedOut()
        if (s == null || demo != null) return
        forgetSession(s.host, s.token)
    }

    /**
     * Ends a session on the account host, best effort. Not on the view
     * model's scope: that one is over when the activity finishes, which is
     * when a sign-in cut off half way needs this most.
     */
    private fun forgetSession(host: String, token: String) {
        background.launch {
            try {
                AccountClient(host).logout(token)
            } catch (_: AccountError) {
                // Offline: forgotten here all the same, and it runs out on the host.
            }
        }
    }

    fun loadDevices() {
        val s = account ?: return
        if (demo != null) return
        devicesSaid = null
        viewModelScope.launch {
            try {
                val list = io { AccountClient(s.host).sessions(s.token) }
                if (account == s) devices = list
            } catch (e: AccountError) {
                if (account != s) return@launch
                if (e is AccountError.SignedOut) signedOut(SIGNED_OUT) else devicesSaid = sentence(e)
            }
        }
    }

    /** Signs out another of the account's devices; this one signs out all the way. */
    fun signOutDevice(d: SignedInDevice) {
        if (d.current) return signOut()
        val s = account ?: return
        if (demo != null) {
            devices = devices?.filter { it.id != d.id }
            return
        }
        viewModelScope.launch {
            try {
                io { AccountClient(s.host).signOutSession(s.token, d.id) }
                if (account == s) devices = devices?.filter { it.id != d.id }
            } catch (e: AccountError) {
                if (account != s) return@launch
                if (e is AccountError.SignedOut) signedOut(SIGNED_OUT) else devicesSaid = sentence(e)
            }
        }
    }

    /** Hides a notice now and marks it read on the account host. */
    fun dismissNotice(n: Notice) {
        val who = me ?: return
        me = who.copy(notices = who.notices.filter { it.id != n.id })
        val s = account ?: return
        if (demo != null) return
        viewModelScope.launch {
            try {
                io { AccountClient(s.host).readNotice(s.token, n.id) }
            } catch (e: AccountError) {
                // Otherwise it shows again next time; nothing else depends on it.
                if (account == s && e is AccountError.SignedOut) signedOut(SIGNED_OUT)
            }
        }
    }

    /** A link to a computer's own page, waiting for the person's yes to open it in the browser. */
    var directLink by mutableStateOf<DirectLink?>(null)
        private set

    class DirectLink(val url: String, val host: String)

    /**
     * A link opened from elsewhere. Through the relay the app pairs by
     * account now; a link to the computer's own page (Same Wi-Fi or
     * Tailscale) opens in the browser, which pairs there, once the person
     * says yes. Any app or web page can open one, so it always asks.
     */
    fun openLink(text: String) {
        val url = text.trim()
        val host = if (LinkKind.of(url) == LinkKind.DIRECT) {
            try {
                java.net.URI(url).let { if (it.port >= 0) "${it.host}:${it.port}" else it.host }
            } catch (_: Exception) {
                null
            }
        } else {
            null
        }
        if (host == null) {
            notice = LinkKind.of(url).sentence
            return
        }
        directLink = DirectLink(url, host)
    }

    /** The person answered the question about [directLink]. */
    fun answerDirectLink() {
        directLink = null
    }

    // ---- connection --------------------------------------------------------

    fun connect() {
        val s = account
        val m = machine
        if (s == null || m == null) {
            connection = Connection.Idle
            return
        }
        retryJob?.cancel()
        link?.close()
        val l = RelayLink(s.host, s.token, m, linkScope, onEvent = ::handle)
        link = l
        connection = Connection.Connecting
        l.start()
    }

    /** The person asks (Reconnect, Try again): go now instead of waiting out the backoff, or a limit. */
    fun reconnectNow() {
        if (machine == null || demo != null) return
        if (connection == Connection.Connected) return
        retry = 500
        connect()
    }

    /**
     * Pull to refresh. The inbox is live while connected and there is no
     * message that asks for it, so a fresh link brings a fresh snapshot; it
     * also replaces a socket that died without a word.
     */
    fun refresh() {
        if (machine == null || demo != null) return
        retry = 500
        connect()
    }

    /** The link went down: what was asked of it will not be answered. */
    private fun interrupted() {
        // The computer answers an `entries` request at most once, so asking
        // again after a drop is safe.
        loadingOlder = false
        loadingOlderSubagent = false
        if (starting) {
            starting = false
            startUnsure = true
            notice = "The connection dropped while the session was starting. Check the inbox before you start it again."
        }
    }

    private fun lost() {
        connection = Connection.Connecting
        retryJob?.cancel()
        val wait = retry
        retry = minOf(retry * 2, 8_000)
        retryJob = viewModelScope.launch {
            delay(wait)
            connect()
        }
    }

    /**
     * The relay said a limit was reached. It says so again at every try until
     * the limit lifts, so the banner says it once and the next try waits
     * minutes, or for the day to turn.
     */
    private fun limited(which: String) {
        val wait = LimitWait.millis(which, limitTries)
        limitTries++
        connection = Connection.Limited(which, System.currentTimeMillis() + wait)
        retryJob?.cancel()
        retryJob = viewModelScope.launch {
            delay(wait)
            connect()
        }
    }

    /**
     * The relay turned the session down at its socket. That alone is no proof
     * the session is over: a header lost on the way to the relay reads the
     * same. So the account is asked, as the desktop app does, and only its 401
     * signs out.
     */
    private fun relayRefusedSession() {
        val s = account ?: return
        val m = machine ?: return
        connection = Connection.Connecting
        retryJob?.cancel()
        // The retry's job: picking another computer or asking again drops the question.
        retryJob = viewModelScope.launch {
            val check = io { AccountClient(s.host).stillStands(s.token) }
            if (account != s) return@launch
            if (check == SessionCheck.OVER) return@launch signedOut(SIGNED_OUT)
            if (machine?.label != m.label || link != null) return@launch
            if (check == SessionCheck.STANDS) {
                // Dialing again would get the same answer, now and at the
                // next launch, so it is said once and not reopened.
                forgetPicked()
                leaveMachine(
                    "${m.name} could not be reached with this sign-in: the relay turned it down, though your account accepts it. " +
                        "Try again later, or sign out and sign in again.",
                )
            } else {
                // The account did not answer either: nothing is known yet.
                lost()
            }
        }
    }

    private fun handle(event: RelayLink.Event) {
        when (event) {
            RelayLink.Event.Open -> {
                interrupted()
                connection = Connection.Connected
                retry = 500
                limitTries = 0
                // One open per session: a session opened while the link was
                // down is the one waiting, so it is not asked for twice.
                val waiting = openWhenLinked
                openWhenLinked = null
                if (waiting != null) {
                    open(waiting)
                } else {
                    session?.let { send("type" to "open", "session" to it.id) }
                }
                // A new connection watches nothing: the open subagent is asked for again.
                subagent?.let { send("type" to "subagent", "session" to it.session, "subagent" to it.id) }
                link?.let {
                    tellPush(it)
                    loadMuted(it)
                }
            }
            is RelayLink.Event.Message -> parseJson(event.bytes)?.let(::receive)
            is RelayLink.Event.Computer -> {
                if (!event.online) interrupted()
                connection = if (event.online) Connection.Connecting else Connection.ComputerAway(event.since)
            }
            is RelayLink.Event.Closed -> {
                link = null
                interrupted()
                when (event.kind) {
                    EndKind.SIGNED_OUT -> relayRefusedSession()
                    EndKind.REFUSED -> {
                        // Gone from the account: the next launch shows the list instead.
                        if (event.reason == "no_machine") forgetPicked()
                        leaveMachine(event.detail)
                    }
                    EndKind.UNTRUSTED -> {
                        // Not reopened on the next launch: it is not the computer the account names.
                        forgetPicked()
                        leaveMachine(
                            "${machine?.name ?: "The computer"} did not answer with the key your account has for it. " +
                                "Sign in again in Arakun on that computer.",
                        )
                    }
                    EndKind.LIMIT -> limited(LimitWait.which(event.detail))
                    EndKind.GONE -> lost()
                }
            }
        }
    }

    private fun receive(msg: JsonElement) {
        val id = msg["id"].str
        when (msg["type"].str) {
            "inbox" -> {
                val i = Inbox.of(msg)
                host = i.host
                inbox = i.sessions
                places = i.places
            }
            "session" -> if (session?.id == id) session = session?.applySession(msg)
            "entries" -> if (session?.id == id) {
                if (msg["older"].bool) loadingOlder = false
                session = session?.applyEntries(msg)
            }
            "state" -> if (session?.id == id) session = session?.applyState(msg)
            "subagents" -> if (session?.id == id) subagents = Subagent.list(msg)
            "subagent_entries" -> if (subagent?.session == id && subagent?.id == msg["subagent"].str) {
                if (msg["older"].bool) loadingOlderSubagent = false
                subagent = subagent?.apply(msg)
            }
            "changes" -> if (session?.id == id) changes = ChangedTurn.list(msg)
            "diff" -> if (session?.id == id) FileDiff.of(msg)?.let { diffs = diffs + (it.ref to it) }
            "started" -> {
                starting = false
                startUnsure = false
                if (session?.id == startedFrom) {
                    open(id)
                } else {
                    notice = "The new session started. It is in the inbox."
                }
            }
            "error" -> {
                starting = false
                notice = msg["message"].string
            }
        }
    }

    // ---- what the user does -------------------------------------------------

    private fun send(vararg fields: Pair<String, Any?>) {
        if (demo != null) return
        link?.send(message(*fields))
    }

    /** Whether a message sent now reaches the computer; the link drops it otherwise. */
    val canSend: Boolean get() = demo != null || link?.isReady == true

    fun open(id: String) {
        val line = inbox.firstOrNull { it.id == id }
        forgetChanges()
        demo?.let {
            session = demoSession(id)
            return
        }
        session = LiveSession(id, line?.title ?: "", line?.agent ?: "")
        if (link?.isReady == true) send("type" to "open", "session" to id) else openWhenLinked = id
    }

    fun close() {
        session = null
        openWhenLinked = null
        loadingOlder = false
        forgetChanges()
        send("type" to "close")
    }

    /** They belong to the session on screen and go with it. */
    private fun forgetChanges() {
        pictures.clear()
        changes = null
        diffs = emptyMap()
        subagents = emptyList()
        subagent = null
        loadingOlderSubagent = false
    }

    /** Opens a subagent's transcript: its last entries, then every change, fill [subagent]. */
    fun openSubagent(id: String) {
        val s = session ?: return
        subagent = SubagentTranscript(s.id, id)
        loadingOlderSubagent = false
        send("type" to "subagent", "session" to s.id, "subagent" to id)
    }

    fun closeSubagent() {
        val t = subagent ?: return
        subagent = null
        loadingOlderSubagent = false
        send("type" to "subagent", "session" to t.session)
    }

    fun loadOlderSubagent() {
        val t = subagent ?: return
        if (t.from <= 0 || loadingOlderSubagent || !canSend) return
        loadingOlderSubagent = true
        send("type" to "entries", "session" to t.session, "subagent" to t.id, "from" to maxOf(0, t.from - 50), "to" to t.from)
    }

    /** Asks for the list of what the session changed; the answer fills [changes]. */
    fun loadChanges() {
        val s = session ?: return
        demo?.let {
            changes = ChangedTurn.list(parseJson(Demo.changes(System.currentTimeMillis() / 1000 - 600))!!)
            return
        }
        send("type" to "changes", "session" to s.id)
    }

    /** Asks for the diffs of `refs` that have not come yet; the answers fill [diffs]. */
    fun loadDiffs(refs: List<DiffRef>) {
        val s = session ?: return
        val missing = refs.filter { it !in diffs }
        demo?.let {
            diffs = diffs + missing.mapNotNull { r -> FileDiff.of(parseJson(Demo.diff(r))!!)?.let { d -> r to d } }
            return
        }
        missing.forEach { send("type" to "diff", "session" to s.id, "entry" to it.entry, "index" to it.index) }
    }

    fun loadOlder() {
        val s = session ?: return
        if (s.from <= 0 || loadingOlder || !canSend) return
        loadingOlder = true
        send("type" to "entries", "session" to s.id, "from" to maxOf(0, s.from - 50), "to" to s.from)
    }

    /**
     * Whether the prompt went out; the composer keeps it when it did not.
     * `now` stops the running turn and sends it as soon as it stopped.
     */
    fun say(text: String, now: Boolean = false, shots: List<Shot> = emptyList()): Boolean {
        val s = session ?: return false
        if (!canSend || (text.isBlank() && shots.isEmpty())) return false
        val fields = mutableListOf<Pair<String, Any?>>("type" to "send", "session" to s.id, "text" to Photos.textFor(text, shots.size))
        if (shots.isNotEmpty()) fields += "uploads" to upload(s.id, shots)
        if (now) fields += "now" to true
        send(*fields.toTypedArray())
        return true
    }

    /**
     * Sends `shots` in pieces, before the prompt that names them; their ids
     * come back for its `uploads`. Before a session exists `session` is empty:
     * the computer keeps an upload by its id alone.
     */
    private fun upload(session: String, shots: List<Shot>): List<String> = shots.mapIndexed { i, shot ->
        Photos.newId(i).also { id ->
            if (demo == null) Photos.pieces(session, id, shot.name, shot.jpeg).forEach { link?.send(it) }
        }
    }

    /**
     * Takes queued prompt `index` out of the queue; true when it went out, so
     * the composer can take its text. `text` lets the computer find one that moved.
     */
    fun takeBack(index: Int, text: String): Boolean {
        val s = session ?: return false
        if (!canSend) return false
        demo?.let {
            session = s.copy(queue = s.queue.filterIndexed { i, _ -> i != index })
            return true
        }
        send("type" to "unqueue", "session" to s.id, "index" to index, "text" to text)
        return true
    }

    /** Lets the queue a Stop paused go again. */
    fun resumeQueue() {
        val s = session ?: return
        if (!canSend) return
        demo?.let {
            session = s.copy(queuePaused = false)
            return
        }
        send("type" to "resume_queue", "session" to s.id)
    }

    fun stop() {
        val s = session ?: return
        if (!canSend) return
        send("type" to "cancel", "session" to s.id, "keep_queue" to true)
    }

    fun allow(option: PermissionAsk.Option) {
        // Offline the card stays, with what was picked in it.
        val s = session ?: return
        if (!canSend) return
        send("type" to "permission", "session" to s.id, "option" to option.id)
        session = s.copy(permission = null)
    }

    fun answer(answers: List<String>) {
        val s = session ?: return
        if (!canSend) return
        send("type" to "answer", "session" to s.id, "answers" to answers)
        session = s.copy(question = null)
    }

    fun skipQuestion() {
        val s = session ?: return
        if (!canSend) return
        send("type" to "skip_question", "session" to s.id)
        session = s.copy(question = null)
    }

    /**
     * Sends what was typed for the secret card. Only over a private
     * connection: the computer would refuse it otherwise, so the card offers
     * only [declineSecret] then. Whether it went out; the card keeps the
     * fields when it did not.
     */
    fun saveSecret(values: List<String>): Boolean {
        val s = session ?: return false
        val r = s.secret ?: return false
        if (!canSend || !r.private || values.size != r.fields.size) return false
        send("type" to "secret", "session" to s.id, "id" to r.id, "values" to values)
        session = s.copy(secret = null)
        return true
    }

    /** Declines the secret card: an answer without `values`. */
    fun declineSecret() {
        val s = session ?: return
        val r = s.secret ?: return
        if (!canSend) return
        send("type" to "secret", "session" to s.id, "id" to r.id)
        session = s.copy(secret = null)
    }

    /** Whether the start went out; the form keeps its prompt when it did not. */
    fun startSession(place: Place, text: String, shots: List<Shot> = emptyList()): Boolean {
        // One start at a time: the sheet can close and open again while one waits.
        if (!canSend || starting) return false
        if (demo != null) {
            // No computer answers a demo: open one as if it had started.
            session = demoSession("s2").copy(id = "demo-new", title = text.lineSequence().first().take(80))
            return true
        }
        starting = true
        startUnsure = false
        startedFrom = session?.id
        val fields = mutableListOf<Pair<String, Any?>>("type" to "start", "env" to place.path, "agent" to place.agent)
        fields += "text" to Photos.textFor(text, shots.size)
        if (shots.isNotEmpty()) fields += "uploads" to upload("", shots)
        send(*fields.toTypedArray())
        return true
    }

    /** The computer skips the push for the session on screen while the app shows it. */
    fun visible(yes: Boolean) {
        foreground = yes
        send("type" to "visible", "visible" to yes)
        if (!yes) return
        // Coming back on screen is not the person asking: a limit is waited out.
        val limit = connection as? Connection.Limited
        if (limit == null || System.currentTimeMillis() >= limit.until) reconnectNow()
        // The list's online states, fresh.
        if (machine == null) loadAccount()
    }

    // ---- notifications -------------------------------------------------------

    /**
     * A tap on a notification. The notice does not say which computer it
     * came from, so as on the phone page the session opens on the computer
     * open, or on the one opened last.
     */
    fun openFromNotice(id: String) {
        if (account == null) return
        if (machine != null || demo != null) return open(id)
        tappedSession = id
        loadAccount(reopen = true)
    }

    /**
     * Tells the computer behind `l` where to send notices and which, while
     * notifications are on: at every link, so a computer the phone links to
     * later and a token that changed are covered alike.
     */
    private fun tellPush(l: RelayLink, token: String? = null) {
        val s = account ?: return
        if (demo != null || !Push.on || !Push.available) return
        linkScope.launch {
            val keys = io { Push.keys() } ?: return@launch
            val t = token ?: Push.token() ?: return@launch
            val endpoint = WebPush.fcmEndpoint(s.host, t) ?: return@launch
            if (link !== l || account != s || !Push.on) return@launch
            ask(l, WebPush.subscribeBody(endpoint, keys, Push.wanted))
        }
    }

    /** Which sessions the computer behind `l` keeps quiet, from `GET /me`. */
    private fun loadMuted(l: RelayLink) {
        if (demo != null) return
        linkScope.launch {
            try {
                val (status, bytes) = l.fetch("/me")
                if (status != 200 || link !== l) return@launch
                val me = parseJson(bytes) ?: return@launch
                muted = WebPush.mutedOf(me)
                knowsMuting = WebPush.knowsMuting(me)
            } catch (_: RelayLink.ClosedException) {
            }
        }
    }

    /** Whether the computer sends no notices about the session `id`. */
    fun isMuted(id: String): Boolean = id in muted

    /**
     * One session's notices off, or on again; the computer keeps the list and
     * answers with all of it.
     */
    fun mute(id: String, on: Boolean) {
        if (demo != null) {
            muted = muted.filter { it != id } + if (on) listOf(id) else emptyList()
            return
        }
        val l = link
        if (l == null || !l.isReady) {
            notice = "Could not save that. Try again."
            return
        }
        linkScope.launch {
            val list = try {
                val (status, bytes) = l.fetch("/mute", "POST", WebPush.muteBody(id, on))
                if (status == 200) parseJson(bytes)?.let(WebPush::muteReply) else null
            } catch (_: RelayLink.ClosedException) {
                null
            }
            // A reply from a computer the person has left says nothing about this one.
            if (link !== l) return@launch
            if (list == null) notice = "Could not save that. Try again." else muted = list
        }
    }

    /** `POST /push` to the computer behind `l`; whether it took it. */
    private suspend fun ask(l: RelayLink, body: String): Boolean = try {
        l.isReady && l.fetch("/push", "POST", body).first == 200
    } catch (_: RelayLink.ClosedException) {
        false
    }

    /** Notifications on or off, from the account sheet; Android's permission is already asked for. */
    fun notifications(on: Boolean) {
        if (demo != null || !Push.available) return
        val l = link
        if (!on) {
            Push.turnOff()
            if (l != null) linkScope.launch { ask(l, WebPush.unsubscribeBody()) }
            return
        }
        Push.turnOn()
        viewModelScope.launch {
            val keys = io { Push.keys() }
            val token = Push.token()
            if (keys == null || token == null) {
                Push.turnOff()
                notice = "Could not turn notifications on. Check the connection and try again."
                return@launch
            }
            link?.let { tellPush(it, token) }
        }
    }

    /** Which notices to be told about; the computer open hears now, the others when they link. */
    fun want(w: WebPush.Wanted) {
        if (demo != null) return
        Push.want(w)
        val l = link ?: return
        if (Push.on) linkScope.launch { ask(l, WebPush.wantedBody(w)) }
    }

    override fun onCleared() {
        link?.close()
    }

    // ---- demo ----------------------------------------------------------------

    /** Signed in to a sample account, with its first computer open if `openFirst`. */
    private fun fillDemo(openFirst: Boolean) {
        val now = System.currentTimeMillis() / 1000
        account = AccountSession(Account.HOST, "", "demo", "octocat")
        me = Me(1, "octocat", emptyList(), "free", listOf(Demo.notice))
        machines = Demo.machines(now)
        machinesLoaded = true
        devices = Demo.devices(now)
        knowsMuting = true
        if (openFirst) pick(machines.first())
    }

    private fun fillDemoInbox() {
        connection = Connection.Connected
        val i = Inbox.of(parseJson(Demo.inbox(System.currentTimeMillis() / 1000))!!)
        host = i.host
        inbox = i.sessions
        places = i.places
    }

    private fun demoSession(id: String): LiveSession {
        val line = inbox.firstOrNull { it.id == id }
        // A new session (not in the inbox) is working, as a start leaves it.
        val status = line?.status ?: com.arakun.kit.SessionStatus.WORKING
        val s = LiveSession(id, line?.title ?: "", line?.agent ?: "")
            .applySession(parseJson(Demo.session(System.currentTimeMillis() / 1000 - 600, status))!!)
        // Only a session that needs input is asking.
        return if (status == com.arakun.kit.SessionStatus.NEEDS_INPUT) s else s.copy(permission = null)
    }

    private companion object {
        /** Said when the account host or the relay ended the session, not the person here. */
        const val SIGNED_OUT = "This phone was signed out. Sign in again to reach your computers."

        /** For what must still happen when the view model is gone; it lives as long as the process. */
        val background = CoroutineScope(SupervisorJob() + Dispatchers.IO)

        fun message(vararg fields: Pair<String, Any?>): JsonObject = buildJsonObject {
            for ((k, v) in fields) {
                put(
                    k,
                    when (v) {
                        is String -> JsonPrimitive(v)
                        is Number -> JsonPrimitive(v)
                        is Boolean -> JsonPrimitive(v)
                        is List<*> -> buildJsonArray { v.forEach { add(JsonPrimitive(it.toString())) } }
                        else -> kotlinx.serialization.json.JsonNull
                    },
                )
            }
        }
    }
}
