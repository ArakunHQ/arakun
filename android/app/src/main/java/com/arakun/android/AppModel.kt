package com.arakun.android

import android.app.Application
import android.os.Build
import android.provider.Settings
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.arakun.kit.Credentials
import com.arakun.kit.EndKind
import com.arakun.kit.Inbox
import com.arakun.kit.InboxSession
import com.arakun.kit.LiveSession
import com.arakun.kit.PairingLink
import com.arakun.kit.PermissionAsk
import com.arakun.kit.Place
import com.arakun.kit.RelayLink
import com.arakun.kit.bool
import com.arakun.kit.get
import com.arakun.kit.parseJson
import com.arakun.kit.str
import com.arakun.kit.string
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject

/**
 * Everything the app shows, and the one link to the computer behind it. The
 * computer sends a snapshot when the link opens and changes from then on.
 * It all runs on the main thread.
 */
class AppModel(app: Application) : AndroidViewModel(app) {
    sealed interface Connection {
        data object Unpaired : Connection
        data object Pairing : Connection
        data object Connecting : Connection
        data object Connected : Connection

        /** The relay is there, the computer is not (last seen `since`, ms). */
        data class ComputerAway(val since: Long?) : Connection

        /** Stopped for good until the user acts. */
        data class Failed(val why: String) : Connection
    }

    private val store = CredentialStore(app)

    var credentials by mutableStateOf<Credentials?>(null)
        private set
    var connection by mutableStateOf<Connection>(Connection.Unpaired)
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

    /** What went wrong pairing, for the pairing screen. */
    var pairError by mutableStateOf<String?>(null)
        private set

    /** A message worth a moment on screen. */
    var notice by mutableStateOf<String?>(null)

    /** Sample data instead of a computer (debug builds, launched with `demo`). */
    var demo: Demo? = null
        private set

    /** The demo asked for the New session sheet, not yet shown. */
    var demoNew = false

    /** A session asked for before the link was up (from a started session). */
    private var openWhenLinked: String? = null

    /** A start sent and not yet answered with `started` or `error`. */
    private var starting = false

    /**
     * The link dropped while a start waited for its answer: the computer may
     * have started the session. Starting again then needs the user's say-so.
     */
    var startUnsure by mutableStateOf(false)
        private set

    private var link: RelayLink? = null
    private var retry = 500L
    private var retryJob: Job? = null
    private var begun = false

    /** The link's own thread: the main one, posted to in order. */
    private val linkScope = CoroutineScope(viewModelScope.coroutineContext + Dispatchers.Main)

    /** Once, when the first screen comes up. */
    fun begin(demo: Demo?) {
        if (begun) return
        begun = true
        if (demo != null) {
            this.demo = demo
            if (!demo.unpaired) fillDemo()
            demo.open?.let(::open)
            demoNew = demo.new
            return
        }
        credentials = store.load()
        if (credentials != null) connect()
    }

    val deviceName: String
        get() = Settings.Global.getString(getApplication<Application>().contentResolver, Settings.Global.DEVICE_NAME)
            ?.takeIf { it.isNotBlank() } ?: Build.MODEL

    val isLinked: Boolean get() = connection == Connection.Connected

    // ---- connection --------------------------------------------------------

    fun connect() {
        val c = credentials
        if (c == null) {
            connection = Connection.Unpaired
            return
        }
        start(RelayLink.Mode.WithCredentials(c))
        connection = Connection.Connecting
    }

    /** A pairing link opened from elsewhere, waiting for the user's yes. */
    var opened by mutableStateOf<PairingLink?>(null)
        private set
    private var openedText: String? = null

    /**
     * A link from outside the app pairs only after the user says yes, as on
     * iOS: any app or web page can open one, and pairing while paired would
     * replace the computer this phone trusts.
     */
    fun openLink(text: String) {
        val p = PairingLink.parse(text)
        val problem = when {
            p == null -> "That is not a pairing link. Open the Phone tab in Arakun and scan its code."
            !p.secure -> "That link does not use a secure connection. Scan the code in Arakun instead."
            else -> null
        }
        if (problem != null) {
            if (credentials == null) pairError = problem else notice = problem
            return
        }
        opened = p
        openedText = text
    }

    /** The answer to [opened]: pair with it, or forget it. */
    fun answerOpened(yes: Boolean) {
        val text = openedText
        opened = null
        openedText = null
        if (yes && text != null) pair(text)
    }

    /** Whether pairing started; a link that is not one leaves an error instead. */
    fun pair(text: String): Boolean {
        val p = PairingLink.parse(text)
        if (p == null) {
            pairError = "That is not a pairing link. Open the Phone tab in Arakun and scan its code."
            return false
        }
        pairError = null
        connection = Connection.Pairing
        start(RelayLink.Mode.Pairing(p, deviceName))
        return true
    }

    private fun start(mode: RelayLink.Mode) {
        retryJob?.cancel()
        link?.close()
        val l = RelayLink(mode, linkScope, onEvent = ::handle)
        link = l
        l.start()
    }

    /** Back on screen or Reconnect: go now instead of waiting out the backoff. */
    fun reconnectNow() {
        if (credentials == null || demo != null) return
        if (connection == Connection.Connected || connection == Connection.Pairing) return
        retry = 500
        connect()
    }

    /**
     * Pull to refresh. The inbox is live while connected and there is no
     * message that asks for it, so a fresh link brings a fresh snapshot; it
     * also replaces a socket that died without a word.
     */
    fun refresh() {
        if (credentials == null || connection == Connection.Pairing || demo != null) return
        retry = 500
        connect()
    }

    fun unpair() {
        viewModelScope.launch {
            try {
                link?.fetch("/unpair", method = "POST")
            } catch (_: Exception) {
                // Forgotten here all the same; the computer drops it on its next look.
            }
            forget()
        }
    }

    private fun forget() {
        link?.close()
        link = null
        retryJob?.cancel()
        credentials = null
        store.save(null)
        inbox = emptyList()
        places = emptyList()
        session = null
        openWhenLinked = null
        loadingOlder = false
        starting = false
        connection = Connection.Unpaired
    }

    /** The link went down: what was asked of it will not be answered. */
    private fun interrupted() {
        // The computer answers an `entries` request at most once, so asking
        // again after a drop is safe.
        loadingOlder = false
        if (starting) {
            starting = false
            startUnsure = true
            notice = "The connection dropped while the session was starting. Check the inbox before you start it again."
        }
    }

    private fun lost() {
        connection = if (connection == Connection.Unpaired) Connection.Unpaired else Connection.Connecting
        retryJob?.cancel()
        val wait = retry
        retry = minOf(retry * 2, 8_000)
        retryJob = viewModelScope.launch {
            delay(wait)
            connect()
        }
    }

    private fun handle(event: RelayLink.Event) {
        when (event) {
            is RelayLink.Event.Open -> {
                event.credentials?.let {
                    // A new computer (its label is its key): nothing open on
                    // the old one exists here, so the inbox starts over.
                    if (credentials?.label != it.label) {
                        session = null
                        openWhenLinked = null
                        loadingOlder = false
                        inbox = emptyList()
                        places = emptyList()
                    }
                    credentials = it
                    if (!store.save(it)) {
                        notice = "Could not keep the pairing on this phone. You may need to pair again after the app restarts."
                    }
                }
                if (event.who.host.isNotEmpty()) host = event.who.host
                interrupted()
                connection = Connection.Connected
                retry = 500
                // One open per session: a session opened while the link was
                // down is the one waiting, so it is not asked for twice.
                val waiting = openWhenLinked
                openWhenLinked = null
                if (waiting != null) {
                    open(waiting)
                } else {
                    session?.let { send("type" to "open", "session" to it.id) }
                }
            }
            is RelayLink.Event.Message -> parseJson(event.bytes)?.let(::receive)
            is RelayLink.Event.Computer -> {
                if (!event.online) interrupted()
                connection = if (event.online) Connection.Connecting else Connection.ComputerAway(event.since)
            }
            // A link opened while paired: the pairing it would replace stays
            // until the new one works.
            is RelayLink.Event.Closed -> if (connection == Connection.Pairing && credentials != null) {
                link = null
                interrupted()
                notice = when (event.kind) {
                    EndKind.PAIRING, EndKind.UNPAIRED ->
                        "That link is wrong or has expired. This phone stays paired with the computer it had."
                    EndKind.UNTRUSTED ->
                        "That address does not answer with the key Arakun showed. This phone stays paired with the computer it had."
                    else ->
                        "Could not pair with the new computer (${event.detail}). This phone stays paired with the one it had."
                }
                lost()
            } else {
                link = null
                interrupted()
                when (event.kind) {
                    EndKind.UNPAIRED -> {
                        forget()
                        pairError = "This phone is no longer paired. Pair it again from Arakun."
                    }
                    EndKind.PAIRING -> {
                        connection = if (credentials == null) Connection.Unpaired else Connection.Connecting
                        pairError = "That link is wrong or has expired. Make a new one in Arakun."
                        if (credentials != null) lost()
                    }
                    EndKind.UNTRUSTED -> connection = Connection.Failed(
                        "This address does not answer with the key Arakun showed. Check the address in its Phone tab before trying again.",
                    )
                    EndKind.LIMIT -> if (credentials == null) {
                        // Pairing has nothing to retry with; a pairing link is used once.
                        connection = Connection.Unpaired
                        pairError = "The relay is busy (${event.detail}). Try the link again in a moment."
                    } else {
                        notice = "The relay is busy (${event.detail}). Trying again in a moment."
                        lost()
                    }
                    EndKind.GONE -> if (credentials == null) {
                        connection = Connection.Unpaired
                        pairError = "Could not reach your computer (${event.detail})."
                    } else {
                        lost()
                    }
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
            "started" -> {
                starting = false
                open(id)
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
    private val canSend: Boolean get() = demo != null || link?.isReady == true

    fun open(id: String) {
        val line = inbox.firstOrNull { it.id == id }
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
        send("type" to "close")
    }

    fun loadOlder() {
        val s = session ?: return
        if (s.from <= 0 || loadingOlder || !canSend) return
        loadingOlder = true
        send("type" to "entries", "session" to s.id, "from" to maxOf(0, s.from - 50), "to" to s.from)
    }

    /** Whether the prompt went out; the composer keeps it when it did not. */
    fun say(text: String): Boolean {
        val s = session ?: return false
        if (!canSend) return false
        send("type" to "send", "session" to s.id, "text" to text)
        return true
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

    /** Whether the start went out; the form keeps its prompt when it did not. */
    fun startSession(place: Place, text: String): Boolean {
        if (!canSend) return false
        if (demo != null) {
            // No computer answers a demo: open one as if it had started.
            session = demoSession("s2").copy(id = "demo-new", title = text.lineSequence().first().take(80))
            return true
        }
        starting = true
        startUnsure = false
        send("type" to "start", "env" to place.path, "agent" to place.agent, "text" to text)
        return true
    }

    /** The computer skips the push for the session on screen while the app shows it. */
    fun visible(yes: Boolean) {
        send("type" to "visible", "visible" to yes)
        if (yes) reconnectNow()
    }

    override fun onCleared() {
        link?.close()
    }

    // ---- demo ----------------------------------------------------------------

    private fun fillDemo() {
        credentials = Credentials("lmx5lt63vsxoyes4.arakun.dev", "lmx5lt63vsxoyes4", true, "demo", "", "", "studio")
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
