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
import okhttp3.Dns
import okhttp3.OkHttpClient
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import java.net.InetAddress
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
import kotlin.test.assertIs

/**
 * The relay and the computer played by a local WebSocket server, as
 * relay/src/relay.ts and core/src/phone/e2e.rs do it: the whole pairing, a
 * message in parts, a request and a revoked phone, through a real socket.
 */
class RelayLinkTest {
    private val server = MockWebServer().apply { start() }
    private val identity: KeyPair = P256.generate()
    private val label = E2E.label(P256.x963(identity.public as ECPublicKey))
    private val thread = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val scope = CoroutineScope(SupervisorJob() + thread)
    private val events = Channel<RelayLink.Event>(Channel.UNLIMITED)

    /** Every name is this machine, so `<label>.localhost` reaches the server. */
    private val client = OkHttpClient.Builder()
        .dns(object : Dns {
            override fun lookup(hostname: String) = listOf(InetAddress.getLoopbackAddress())
        })
        .build()

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

        private fun answerHello(hello: JsonElement) {
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
    fun pairsFetchesJoinsPartsAndHearsARevoke() = runBlocking {
        val computer = Computer(channel = 7)
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        val link = PairingLink.parse("http://$label.localhost:${server.port}/#p=code1&s=secret1")!!
        val relay = withContext(thread) {
            RelayLink(RelayLink.Mode.Pairing(link, "Pixel"), scope, client) { events.trySend(it) }.also { it.start() }
        }

        // The phone shows the secret, sealed, and the computer answers with credentials.
        val pair = computer.heard.poll(10, TimeUnit.SECONDS)!!
        assertEquals("secret1", pair["pair"]["secret"].str)
        assertEquals("Pixel", pair["pair"]["name"].str)
        assertEquals("/ws?p=code1", server.takeRequest().target)
        computer.say(buildJsonObject {
            put("paired", buildJsonObject {
                put("id", "d1")
                put("name", "Pixel")
                put("host", "studio")
                put("ticket", "t1")
                put("token", "k1")
            })
        })
        val open = assertIs<RelayLink.Event.Open>(next())
        assertEquals("studio", open.who.host)
        assertEquals(Credentials("$label.localhost:${server.port}", label, false, "d1", "t1", "k1", "studio"), open.credentials)

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

        // What the page asks over HTTP, answered in two pieces.
        val fetched = async(thread) { relay.fetch("/unpair", method = "POST") }
        val req = computer.heard.poll(10, TimeUnit.SECONDS)!!["req"]
        assertEquals("/unpair", req["path"].str)
        assertEquals("POST", req["method"].str)
        computer.say(buildJsonObject {
            put("res", buildJsonObject {
                put("id", req["id"].int)
                put("status", 200)
                put("body", Base64Url.encode("o".encodeToByteArray()))
                put("more", true)
            })
        })
        computer.say(buildJsonObject {
            put("res", buildJsonObject {
                put("id", req["id"].int)
                put("body", Base64Url.encode("k".encodeToByteArray()))
            })
        })
        val (status, body) = withTimeout(10_000) { fetched.await() }
        assertEquals(200, status)
        assertEquals("ok", body.decodeToString())

        // The relay closes a revoked phone's socket: pair again.
        computer.socket.close(4401, "revoked")
        val closed = assertIs<RelayLink.Event.Closed>(next())
        assertEquals(EndKind.UNPAIRED, closed.kind)
        assertEquals(false, relay.isReady)
    }

    @Test
    fun aComputerWithTheWrongKeyIsNotTrusted() = runBlocking {
        val computer = Computer(channel = 1)
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        // The address names another key than the one that answers.
        val link = PairingLink.parse("http://aaaaaaaaaaaaaaaa.localhost:${server.port}/#p=c&s=s")!!
        withContext(thread) {
            RelayLink(RelayLink.Mode.Pairing(link, "Pixel"), scope, client) { events.trySend(it) }.start()
        }
        val closed = assertIs<RelayLink.Event.Closed>(next())
        assertEquals(EndKind.UNTRUSTED, closed.kind)
    }

    @Test
    fun aRelayRunByHandSaysItsLabel() = runBlocking {
        val computer = Computer(channel = 3)
        // `wrangler dev` has no subdomains, so the link carries no label.
        server.enqueue(MockResponse.Builder().body("""{"label":"$label"}""").build())
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        val link = PairingLink.parse("http://127.0.0.1:${server.port}/#p=c&s=s")!!
        withContext(thread) {
            RelayLink(RelayLink.Mode.Pairing(link, "Pixel"), scope, client) { events.trySend(it) }.start()
        }
        assertEquals("s", computer.heard.poll(10, TimeUnit.SECONDS)!!["pair"]["secret"].str)
        assertEquals("/_relay", server.takeRequest().target)
        assertEquals("/ws?p=c", server.takeRequest().target)
    }

    @Test
    fun theComputerGoingAwayAndBackIsAnEvent() = runBlocking {
        val computer = Computer(channel = 2)
        server.enqueue(MockResponse.Builder().webSocketUpgrade(computer).build())
        val creds = Credentials("$label.localhost:${server.port}", label, false, "d1", "t1", "k1", "studio")
        withContext(thread) {
            RelayLink(RelayLink.Mode.WithCredentials(creds), scope, client) { events.trySend(it) }.start()
        }
        val auth = computer.heard.poll(10, TimeUnit.SECONDS)!!
        assertEquals("k1", auth["auth"]["token"].str)
        assertEquals("/ws?d=d1&t=t1&s=${RelayLink.PAGE}", server.takeRequest().target)
        computer.say(buildJsonObject { put("ok", buildJsonObject { put("host", "studio") }) })
        assertIs<RelayLink.Event.Open>(next())
        computer.control(buildJsonObject {
            put("type", "pc_offline")
            put("since", 1_700_000_000_000)
        })
        assertEquals(RelayLink.Event.Computer(online = false, since = 1_700_000_000_000), next())
        computer.control(buildJsonObject { put("type", "bye"); put("reason", "limit:daily") })
        assertEquals(EndKind.LIMIT, assertIs<RelayLink.Event.Closed>(next()).kind)
    }
}
