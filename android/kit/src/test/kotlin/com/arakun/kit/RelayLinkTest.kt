package com.arakun.kit

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import java.security.KeyPair
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import javax.crypto.KeyAgreement
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs

/**
 * The relay and the computer played by a local WebSocket server, as
 * cloud/src/relay/relay.ts and core/src/phone/e2e.rs do it: the socket with the
 * account's session, a message in parts, a request and a signed-out phone,
 * through a real socket.
 */
class RelayLinkTest {
    private val server = MockWebServer().apply { start() }
    private val identity: KeyPair = P256.generate()
    private val label = E2E.label(P256.x963(identity.public as ECPublicKey))
    private val thread = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val scope = CoroutineScope(SupervisorJob() + thread)
    private val events = Channel<RelayLink.Event>(Channel.UNLIMITED)

    private val client = OkHttpClient()
    private val base = "http://localhost:${server.port}"
    private val pub = Base64Url.encode(P256.x963(identity.public as ECPublicKey))
    private val machine = Machine(label, pub, "studio", "app", true, 0)

    private fun link(m: Machine = machine) =
        RelayLink(base, "tok-secret", m, scope, client) { events.trySend(it) }.also { it.start() }

    @AfterTest
    fun done() {
        scope.coroutineContext[kotlinx.coroutines.Job]?.cancel()
        thread.close()
        server.close()
    }

    /** The computer's side: what it got from the phone, in order. */
    private inner class Computer(private val channel: Int) : WebSocketListener() {
        val heard = LinkedBlockingQueue<JsonElement>()
        lateinit var socket: WebSocket
        private var session: E2E.Session? = null

        override fun onOpen(webSocket: WebSocket, response: Response) {
            socket = webSocket
            control(buildJsonObject {
                put("type", "open")
                put("channel", channel)
            })
        }

        fun control(obj: JsonObject) {
            socket.send(Frame(Frame.Kind.CONTROL, obj.toString().encodeToByteArray()).encoded.toByteString())
        }

        fun say(obj: JsonObject) {
            val sealed = session!!.seal(obj.toString().encodeToByteArray())
            socket.send(Frame(Frame.Kind.DATA, sealed).encoded.toByteString())
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            val f = Frame.decode(bytes.toByteArray())!!
            when (f.kind) {
                Frame.Kind.HELLO -> answerHello(parseJson(f.payload)!!)
                Frame.Kind.DATA -> heard.put(parseJson(session!!.open(f.payload))!!)
                Frame.Kind.CONTROL -> {}
            }
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(1000, null)
        }

        /** The key this computer answers with; a stand-in to play one the account does not name. */
        var answerAs: KeyPair = identity

        private fun answerHello(hello: JsonElement) {
            val identity = answerAs
            val label = E2E.label(P256.x963(identity.public as ECPublicKey))
            val ePhone = Base64Url.decode(hello["e"].str)!!
            val noncePhone = Base64Url.decode(hello["nonce"].str)!!
            val eph = P256.generate()
            val ePc = P256.x963(eph.public as ECPublicKey)
            val noncePc = ByteArray(16).also(E2E.random::nextBytes)
            val der = Signature.getInstance("SHA256withECDSA").run {
                initSign(identity.private)
                update(E2E.transcript(label, channel, ePhone, ePc, noncePhone, noncePc))
                sign()
            }
            val shared = KeyAgreement.getInstance("ECDH").run {
                init(eph.private)
                doPhase(P256.publicKey(ePhone), true)
                generateSecret()
            }
            val (phoneToPc, pcToPhone) = E2E.deriveKeys(shared, noncePhone, noncePc)
            session = E2E.Session(pcToPhone, phoneToPc)
            val reply = buildJsonObject {
                put("v", 1)
                put("pub", Base64Url.encode(P256.x963(identity.public as ECPublicKey)))
                put("e", Base64Url.encode(ePc))
                put("nonce", Base64Url.encode(noncePc))
                put("sig", Base64Url.encode(derToRaw(der)))
                put("arakun", "test")
            }
            socket.send(Frame(Frame.Kind.HELLO, reply.toString().encodeToByteArray()).encoded.toByteString())
        }
    }

    private suspend fun next(): RelayLink.Event = withTimeout(10_000) { events.receive() }

    @Test
    fun theSocketCarriesTheSessionInAHeaderAndNeedsNothingMoreInside() = runBlocking {
        val computer = Computer(channel = 7)
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        val relay = withContext(thread) { link() }
        assertEquals(RelayLink.Event.Open, next())
        val req = server.takeRequest()
        assertEquals("/machines/$label/ws?page=${RelayLink.PAGE}", req.target)
        assertEquals("Bearer tok-secret", req.headers["Authorization"])
        assertFalse("tok-secret" in req.target)

        // A message too big for one frame comes in pieces and leaves whole.
        val whole = """{"type":"inbox","host":"studio","sessions":[]}""".encodeToByteArray()
        for ((i, piece) in whole.toList().chunked(10).withIndex()) {
            computer.say(buildJsonObject {
                put("part", buildJsonObject {
                    put("body", Base64Url.encode(piece.toByteArray()))
                    if ((i + 1) * 10 < whole.size) put("more", true)
                })
            })
        }
        assertEquals(whole.decodeToString(), assertIs<RelayLink.Event.Message>(next()).bytes.decodeToString())

        // What the page asks over HTTP, answered in two pieces. It is the
        // first thing the computer hears: no {pair} or {auth} came before it.
        val fetched = async(thread) { relay.fetch("/me") }
        val req2 = computer.heard.poll(10, TimeUnit.SECONDS)!!
        assertEquals(setOf("req"), (req2 as? JsonObject)?.keys)
        assertEquals("/me", req2["req"]["path"].str)
        assertEquals("GET", req2["req"]["method"].str)
        computer.say(buildJsonObject {
            put("res", buildJsonObject {
                put("id", req2["req"]["id"].int)
                put("status", 200)
                put("body", Base64Url.encode("o".encodeToByteArray()))
                put("more", true)
            })
        })
        computer.say(buildJsonObject {
            put("res", buildJsonObject {
                put("id", req2["req"]["id"].int)
                put("body", Base64Url.encode("k".encodeToByteArray()))
            })
        })
        val (status, body) = withTimeout(10_000) { fetched.await() }
        assertEquals(200, status)
        assertEquals("ok", body.decodeToString())

        // The relay rechecks the session; one signed out elsewhere is closed.
        computer.control(buildJsonObject { put("type", "bye"); put("reason", "signed_out") })
        assertEquals(EndKind.SIGNED_OUT, assertIs<RelayLink.Event.Closed>(next()).kind)
        assertEquals(false, relay.isReady)
    }

    @Test
    fun aComputerWithAnotherKeyThanTheAccountsIsNotTrusted() = runBlocking {
        val computer = Computer(channel = 1)
        // A key the account does not name, though it signs for its own label.
        computer.answerAs = P256.generate()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        withContext(thread) { link() }
        val closed = assertIs<RelayLink.Event.Closed>(next())
        assertEquals(EndKind.UNTRUSTED, closed.kind)
        assertEquals(true, "account's key" in closed.detail)
    }

    @Test
    fun aKeyThatDoesNotNameItsLabelIsRefusedBeforeAnySocket() = runBlocking {
        withContext(thread) { link(machine.copy(label = "aaaaaaaaaaaaaaaa")) }
        assertEquals(EndKind.UNTRUSTED, assertIs<RelayLink.Event.Closed>(next()).kind)
        assertEquals(0, server.requestCount)
    }

    @Test
    fun theSessionNeverGoesOverPlainHttpToAnotherMachine() = runBlocking {
        withContext(thread) {
            RelayLink("http://192.168.1.5:8787", "tok-secret", machine, scope, client) { events.trySend(it) }.start()
        }
        assertEquals(EndKind.GONE, assertIs<RelayLink.Event.Closed>(next()).kind)
        assertEquals(0, server.requestCount)
    }

    @Test
    fun aSessionRefusedAtTheUpgradeIsSignedOut() = runBlocking {
        server.enqueue(MockResponse.Builder().code(401).body("""{"error":"signed out"}""").build())
        withContext(thread) { link() }
        assertEquals(EndKind.SIGNED_OUT, assertIs<RelayLink.Event.Closed>(next()).kind)
    }

    /** The relay's database is down: a failure to try again, never a refusal or a sign-out. */
    @Test
    fun aRelayThatFailsAtTheUpgradeIsTriedAgain() = runBlocking {
        for (status in listOf(500, 503)) {
            server.enqueue(MockResponse.Builder().code(status).build())
            withContext(thread) { link() }
            assertEquals(EndKind.GONE, assertIs<RelayLink.Event.Closed>(next()).kind, "$status")
        }
    }

    /** The relay refuses a device as its socket opens, before any channel. */
    @Test
    fun aRefusalAtAdmissionIsHeardAsTheSocketOpens() = runBlocking {
        val admitted = mapOf("signed_out" to EndKind.SIGNED_OUT, "blocked" to EndKind.REFUSED, "no_machine" to EndKind.REFUSED, "not_yours" to EndKind.REFUSED)
        for ((reason, kind) in admitted) {
            server.enqueue(MockResponse.Builder().webSocketUpgrade(object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    val bye = buildJsonObject { put("type", "bye"); put("reason", reason) }
                    webSocket.send(Frame(Frame.Kind.CONTROL, bye.toString().encodeToByteArray()).encoded.toByteString())
                    webSocket.close(4403, reason)
                }
            }).build())
            withContext(thread) { link() }
            val closed = assertIs<RelayLink.Event.Closed>(next())
            assertEquals(kind, closed.kind, reason)
            assertEquals(reason, closed.reason)
        }
    }

    @Test
    fun aRefusalIsSaidInPlainWords() = runBlocking {
        val computer = Computer(channel = 5)
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        withContext(thread) { link() }
        assertEquals(RelayLink.Event.Open, next())
        computer.control(buildJsonObject { put("type", "bye"); put("reason", "disabled"); put("message", "Back at noon.") })
        assertEquals(RelayLink.Event.Closed(EndKind.REFUSED, "Back at noon.", "disabled"), next())
    }

    @Test
    fun theComputerGoingAwayAndBackIsAnEvent() = runBlocking {
        val computer = Computer(channel = 2)
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        withContext(thread) { link() }
        assertEquals(RelayLink.Event.Open, next())
        computer.control(buildJsonObject {
            put("type", "pc_offline")
            put("since", 1_700_000_000_000)
        })
        assertEquals(RelayLink.Event.Computer(online = false, since = 1_700_000_000_000), next())
        computer.control(buildJsonObject { put("type", "bye"); put("reason", "limit:daily") })
        assertEquals(EndKind.LIMIT, assertIs<RelayLink.Event.Closed>(next()).kind)
    }
}
