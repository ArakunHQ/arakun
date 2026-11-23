// One connection to one of the account's computers through the relay, as
// phone/relay.js makes it: a WebSocket to `/machines/<label>/ws` with the
// account's session, a handshake sealed end to end so the relay only passes
// bytes along, then the same JSON the phone page speaks. Inside the channel
// `{req}` asks what the page would ask over HTTP, the computer answers `{res}`
// in pieces, and any message too big for a relay frame comes as `{part}`.
package com.arakun.kit

import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/**
 * Everything it does runs on `scope`, which must run one thing at a time (the
 * main thread in the app): the socket's callbacks hop onto it in order.
 */
class RelayLink(
    /** The account host, as `https://app.arakun.dev`. */
    private val base: String,
    /** The account's session: it lets the socket in. */
    private val session: String,
    /** The computer, as `GET /machines` listed it: its key is checked against its label first. */
    private val machine: Machine,
    private val scope: CoroutineScope,
    /** The HTTP client the socket goes through; tests swap it. */
    private val client: OkHttpClient = defaultClient,
    private val onEvent: (Event) -> Unit,
) {
    sealed interface Event {
        /** The channel is up: the relay let only this account in, so it is the computer's client now. */
        data object Open : Event

        /** A message from the computer, whole, as JSON. */
        class Message(val bytes: ByteArray) : Event

        /** The computer went away from the relay (`since` it was last seen, ms), or came back. */
        data class Computer(val online: Boolean, val since: Long?) : Event

        /**
         * For [EndKind.REFUSED], `detail` is the sentence to show. `reason`
         * is the relay's own word for it, when it said one.
         */
        data class Closed(val kind: EndKind, val detail: String, val reason: String = "") : Event
    }

    class ClosedException : Exception("closed")

    private val label = machine.label
    private var socket: WebSocket? = null
    private var channel = 0
    private var handshake: E2E.Handshake? = null
    private var sealer: E2E.Session? = null
    private var joiner = Joiner()
    private var ready = false
    private var ended = false
    private var heard = 0L
    private var helloDeadline: Long? = null
    private var lastPing = 0L
    private var timer: Job? = null
    private var nextReq = 1
    private val pending = mutableMapOf<Int, PendingRequest>()

    private class PendingRequest(val done: CancellableContinuation<Pair<Int, ByteArray>>) {
        var status = 0
        var body = java.io.ByteArrayOutputStream()
    }

    val isReady: Boolean get() = ready && !ended

    fun start() {
        // The account host names the key; the label it derives is what the
        // handshake's signature covers, and what goes in the socket's path.
        // Said on the scope, as every other end is, not inside the caller's start().
        if (!Account.keyMatches(machine)) {
            scope.launch { end(EndKind.UNTRUSTED, "the account's key for this computer does not match its label") }
            return
        }
        val req = request()
        if (req == null) {
            scope.launch { end(EndKind.GONE, "the account host must use https") }
            return
        }
        open(req)
    }

    /** The computer's socket on the relay, as this app. */
    internal fun request(): Request? {
        // The session goes in a header, so only over TLS or to this machine.
        if (!Account.safeBase(base)) return null
        val url = base.trimEnd('/').toHttpUrlOrNull()?.newBuilder()
            ?.addPathSegment("machines")
            ?.addPathSegment(label)
            ?.addPathSegment("ws")
            ?.addQueryParameter("page", PAGE)
            ?.build() ?: return null
        return Request.Builder().url(url).header("Authorization", "Bearer $session").build()
    }

    private fun open(req: Request) {
        val now = System.currentTimeMillis()
        heard = now
        helloDeadline = now + HELLO_WITHIN
        socket = client.newWebSocket(req, Listener())
        timer = scope.launch {
            while (isActive) {
                tick()
                delay(1000)
            }
        }
    }

    private inner class Listener : WebSocketListener() {
        // Every callback hops onto the scope; a socket replaced or ended is ignored there.
        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            val data = bytes.toByteArray()
            scope.launch {
                if (socket !== webSocket || ended) return@launch
                heard = System.currentTimeMillis()
                frame(data)
            }
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            // The relay's "pong" is text: it only says it is there.
            scope.launch { if (socket === webSocket) heard = System.currentTimeMillis() }
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(1000, null)
            scope.launch {
                if (socket !== webSocket) return@launch
                refused(reason, code, null)
            }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            // The relay answers the upgrade itself only to refuse it; a 401 there
            // means the session is gone, and trying again would never work.
            val code = response?.code
            scope.launch {
                if (socket !== webSocket) return@launch
                if (code == 401) end(EndKind.SIGNED_OUT, "signed_out") else end(EndKind.GONE, t.message ?: "the connection failed")
            }
        }
    }

    private fun tick() {
        val socket = socket
        if (ended || socket == null) return
        val now = System.currentTimeMillis()
        helloDeadline?.let { if (now > it) return end(EndKind.GONE, "no answer from the computer") }
        if (now - heard > SILENT_AFTER) return end(EndKind.GONE, "the relay went quiet")
        if (now - lastPing >= PING_EVERY) {
            lastPing = now
            socket.send("ping")
        }
    }

    private fun frame(bytes: ByteArray) {
        val f = Frame.decode(bytes) ?: return
        when (f.kind) {
            Frame.Kind.CONTROL -> parseJson(f.payload)?.let { if (it.isObject) control(it) }
            Frame.Kind.HELLO -> {
                val h = handshake ?: return
                sealer = try {
                    if (!sameKey(f.payload)) throw E2E.Failure.BadHello("not the account's key")
                    h.finish(f.payload, label, channel)
                } catch (e: Exception) {
                    return end(EndKind.UNTRUSTED, e.message ?: "untrusted")
                }
                handshake = null
                // The relay let only this account in: nothing more to prove inside the channel.
                ready = true
                helloDeadline = null
                onEvent(Event.Open)
            }
            Frame.Kind.DATA -> sealed(f.payload)
        }
    }

    /** Whether the computer's hello names the key the account has for it. */
    private fun sameKey(hello: ByteArray): Boolean {
        val theirs = parseJson(hello)?.get("pub")?.string?.let(Base64Url::decode) ?: return false
        val ours = Base64Url.decode(machine.pub) ?: return false
        return theirs.contentEquals(ours)
    }

    private fun greet() {
        sealer = null
        ready = false
        // Pieces from before belong to a message that will not be finished.
        joiner = Joiner()
        val h = E2E.Handshake()
        handshake = h
        socket?.send(Frame(Frame.Kind.HELLO, h.hello).encoded.toByteString())
        helloDeadline = System.currentTimeMillis() + HELLO_WITHIN
    }

    private fun control(msg: JsonElement) {
        when (msg["type"].str) {
            "open" -> {
                channel = (msg["channel"].double ?: 0.0).toLong().toInt()
                greet()
            }
            "pc_offline" -> {
                // Nobody to answer until `pc_online`: no deadline meanwhile.
                helloDeadline = null
                sealer = null
                ready = false
                failPending()
                onEvent(Event.Computer(online = false, since = msg["since"].double?.toLong()))
            }
            "pc_online" -> {
                onEvent(Event.Computer(online = true, since = null))
                greet()
            }
            "bye" -> refused(msg["reason"].str, 0, msg["message"].string)
            "limit" -> end(EndKind.LIMIT, msg["which"].string ?: "limit")
        }
    }

    /** Ends with what the relay said: a refusal as its sentence, else the reason. */
    private fun refused(reason: String, code: Int, message: String?) {
        val kind = EndKind.of(reason, code)
        val host = base.substringAfter("://").substringBefore('/')
        end(kind, if (kind == EndKind.REFUSED) EndKind.sentence(reason, message, host) else reason.ifEmpty { "closed ($code)" }, reason)
    }

    private fun sealed(payload: ByteArray) {
        val sealer = sealer ?: return
        var plain = try {
            sealer.open(payload)
        } catch (e: Exception) {
            return end(EndKind.UNTRUSTED, e.message ?: "untrusted")
        }
        var msg: JsonElement = parseJson(plain)?.takeIf { it.isObject } ?: return
        val part = msg["part"]
        if (part.isObject) {
            plain = joiner.add(part) ?: return
            msg = parseJson(plain)?.takeIf { it.isObject } ?: return
        }
        val res = msg["res"]
        when {
            res.isObject -> {
                val id = res["id"].int ?: return
                val p = pending[id] ?: return
                res["status"].int?.let { p.status = it }
                p.body.write(Base64Url.decode(res["body"].str) ?: ByteArray(0))
                if (!res["more"].bool) {
                    pending.remove(id)
                    p.done.resume((if (p.status == 0) 500 else p.status) to p.body.toByteArray())
                }
            }
            else -> onEvent(Event.Message(plain))
        }
    }

    private fun say(obj: JsonObject) {
        val sealer = sealer
        val socket = socket
        if (ended || sealer == null || socket == null) return
        val payload = sealer.seal(obj.toString().encodeToByteArray())
        if (!socket.send(Frame(Frame.Kind.DATA, payload).encoded.toByteString())) {
            scope.launch { end(EndKind.GONE, "could not send") }
        }
    }

    /** A message to the computer, as the phone page sends it. */
    fun send(obj: JsonObject) {
        if (isReady) say(obj)
    }

    /** What the phone page would ask over HTTP: the status and body come back. */
    suspend fun fetch(path: String, method: String = "GET", body: String? = null): Pair<Int, ByteArray> {
        if (!isReady) throw ClosedException()
        val id = nextReq++
        return suspendCancellableCoroutine { done ->
            pending[id] = PendingRequest(done)
            done.invokeOnCancellation { scope.launch { pending.remove(id) } }
            say(buildJsonObject {
                put("req", buildJsonObject {
                    put("id", id)
                    put("method", method)
                    put("path", path)
                    put("body", body?.let(::JsonPrimitive) ?: JsonNull)
                })
            })
        }
    }

    private fun failPending() {
        val all = pending.values.toList()
        pending.clear()
        for (p in all) if (p.done.isActive) p.done.resumeWithException(ClosedException())
    }

    private fun end(kind: EndKind, detail: String, reason: String = "") {
        if (ended) return
        hangUp()
        onEvent(Event.Closed(kind, detail, reason))
    }

    /** Hangs up without a word; no event follows. */
    fun close() {
        if (ended) return
        hangUp()
    }

    private fun hangUp() {
        ended = true
        ready = false
        timer?.cancel()
        failPending()
        socket?.close(1000, null)
        socket = null
    }

    companion object {
        /**
         * The page pings the relay this often and the relay answers on its own;
         * a link that hears nothing for two pings and some is dead.
         */
        const val PING_EVERY = 30_000L
        const val SILENT_AFTER = 75_000L

        /** How long the computer may take to answer a hello. */
        const val HELLO_WITHIN = 15_000L

        /**
         * This app's own name to the relay. It stays the same across reconnects,
         * so a socket left behind by a network switch is replaced.
         */
        val PAGE: String = ByteArray(12).also(E2E.random::nextBytes).joinToString("") { "%02x".format(it) }

        private val defaultClient: OkHttpClient by lazy {
            OkHttpClient.Builder()
                // The link pings and watches for silence itself.
                .readTimeout(0, TimeUnit.MILLISECONDS)
                .build()
        }
    }
}
