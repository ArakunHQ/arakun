// One connection to the computer through the relay, as phone/relay.js makes
// it: a WebSocket to `/ws`, a handshake sealed end to end so the relay only
// passes bytes along, then the same JSON the phone page speaks. Inside the
// channel `{req}` asks what the page would ask over HTTP, the computer answers
// `{res}` in pieces, and any message too big for a relay frame comes as `{part}`.
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
import okhttp3.HttpUrl
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
    private val mode: Mode,
    private val scope: CoroutineScope,
    /** The HTTP client the socket goes through; tests swap it. */
    private val client: OkHttpClient = defaultClient,
    private val onEvent: (Event) -> Unit,
) {
    /**
     * Who answered: the device id the computer knows this phone by, the
     * phone's name as the computer keeps it, and the computer's name (`host`).
     */
    data class Who(val id: String, val name: String, val host: String)

    sealed interface Event {
        /** The channel is up. `credentials` are new ones when this was a pairing. */
        data class Open(val who: Who, val credentials: Credentials?) : Event

        /** A message from the computer, whole, as JSON. */
        class Message(val bytes: ByteArray) : Event

        /** The computer went away from the relay (`since` it was last seen, ms), or came back. */
        data class Computer(val online: Boolean, val since: Long?) : Event
        data class Closed(val kind: EndKind, val detail: String) : Event
    }

    sealed interface Mode {
        data class WithCredentials(val credentials: Credentials) : Mode
        data class Pairing(val link: PairingLink, val deviceName: String) : Mode
    }

    class ClosedException : Exception("closed")

    private val label: String
    private val host: String
    private val secure: Boolean
    private var credentials: Credentials? = null
    private var socket: WebSocket? = null
    private var channel = 0
    private var handshake: E2E.Handshake? = null
    private var session: E2E.Session? = null
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

    init {
        when (mode) {
            is Mode.WithCredentials -> {
                label = mode.credentials.label
                host = mode.credentials.host
                secure = mode.credentials.secure
                credentials = mode.credentials
            }
            is Mode.Pairing -> {
                label = mode.link.label
                host = mode.link.host
                secure = mode.link.secure
            }
        }
    }

    val isReady: Boolean get() = ready && !ended

    fun start() {
        val url = try {
            val b = HttpUrl.Builder()
                .scheme(if (secure) "https" else "http")
                .host(host.substringBefore(':'))
                .addPathSegment("ws")
            host.substringAfter(':', "").toIntOrNull()?.let(b::port)
            when (mode) {
                is Mode.WithCredentials -> b.addQueryParameter("d", mode.credentials.id)
                    .addQueryParameter("t", mode.credentials.ticket)
                    .addQueryParameter("s", PAGE)
                is Mode.Pairing -> b.addQueryParameter("p", mode.link.code)
            }
            b.build()
        } catch (_: IllegalArgumentException) {
            return end(EndKind.GONE, "bad address")
        }
        val now = System.currentTimeMillis()
        heard = now
        helloDeadline = now + HELLO_WITHIN
        socket = client.newWebSocket(Request.Builder().url(url).build(), Listener())
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
                end(EndKind.of(reason, code), reason.ifEmpty { "closed ($code)" })
            }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            scope.launch {
                if (socket !== webSocket) return@launch
                end(EndKind.GONE, t.message ?: "the connection failed")
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
                session = try {
                    h.finish(f.payload, label, channel)
                } catch (e: Exception) {
                    return end(EndKind.UNTRUSTED, e.message ?: "untrusted")
                }
                handshake = null
                val c = credentials
                if (c != null) {
                    say(buildJsonObject { put("auth", buildJsonObject { put("token", c.token) }) })
                } else if (mode is Mode.Pairing) {
                    say(buildJsonObject {
                        put("pair", buildJsonObject {
                            put("secret", mode.link.secret)
                            put("name", mode.deviceName)
                        })
                    })
                }
            }
            Frame.Kind.DATA -> sealed(f.payload)
        }
    }

    private fun greet() {
        session = null
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
                session = null
                ready = false
                failPending()
                onEvent(Event.Computer(online = false, since = msg["since"].double?.toLong()))
            }
            "pc_online" -> {
                onEvent(Event.Computer(online = true, since = null))
                greet()
            }
            "bye" -> {
                val reason = msg["reason"].str
                end(EndKind.of(reason, 0), reason)
            }
            "limit" -> end(EndKind.LIMIT, msg["which"].string ?: "limit")
        }
    }

    private fun sealed(payload: ByteArray) {
        val session = session ?: return
        var plain = try {
            session.open(payload)
        } catch (e: Exception) {
            return end(EndKind.UNTRUSTED, e.message ?: "untrusted")
        }
        var msg: JsonElement = parseJson(plain)?.takeIf { it.isObject } ?: return
        val part = msg["part"]
        if (part.isObject) {
            plain = joiner.add(part) ?: return
            msg = parseJson(plain)?.takeIf { it.isObject } ?: return
        }
        val paired = msg["paired"]
        val ok = msg["ok"]
        val res = msg["res"]
        when {
            paired.isObject -> {
                val who = Who(paired["id"].str, paired["name"].str, paired["host"].str)
                val c = Credentials(
                    host = host, label = label, secure = secure, id = who.id,
                    ticket = paired["ticket"].str, token = paired["token"].str, name = who.host,
                )
                credentials = c
                opened(who, c)
            }
            ok.isObject -> opened(Who(ok["id"].str, ok["name"].str, ok["host"].str), null)
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

    private fun opened(who: Who, new: Credentials?) {
        ready = true
        helloDeadline = null
        onEvent(Event.Open(who, new))
    }

    private fun say(obj: JsonObject) {
        val session = session
        val socket = socket
        if (ended || session == null || socket == null) return
        val payload = session.seal(obj.toString().encodeToByteArray())
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

    private fun end(kind: EndKind, detail: String) {
        if (ended) return
        hangUp()
        onEvent(Event.Closed(kind, detail))
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
