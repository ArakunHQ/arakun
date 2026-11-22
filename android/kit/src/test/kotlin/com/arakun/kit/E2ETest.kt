package com.arakun.kit

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import java.io.File
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import javax.crypto.KeyAgreement
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull

/** The vectors core/src/phone/e2e.rs writes, which phone/e2e.js and the iOS app are checked against too. */
private fun load(name: String): JsonElement =
    parseJson(File(System.getProperty("vectors"), name).readText())!!

private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

class E2ETest {
    private val v = load("e2e.json")
    private fun bytes(name: String) = Base64Url.decode(v[name].str)!!
    private val label = v["label"].str
    private val channel = v["channel"].int!!

    @Test
    fun labelMatchesRustAndTheWorker() {
        val l = load("label.json")
        assertEquals(l["label"].str, E2E.label(hex(l["public_key_hex"].str)))
        assertEquals(label, E2E.label(bytes("pc_pub")))
    }

    @Test
    fun trustsTheComputersHello() {
        val hello: JsonElement = parseJson(v["hello_pc"].str)!!
        val fields = hello.jsonObject
        val pc = E2E.verifyPc(hello, label, channel, bytes("e_phone"), bytes("nonce_phone"))
        assertContentEquals(bytes("e_pc"), pc.e)
        assertContentEquals(bytes("nonce_pc"), pc.nonce)
        assertEquals("0.0.0-vectors", pc.arakun)

        fun rejects(
            h: JsonElement, why: String, label: String = this.label, channel: Int = this.channel,
            ePhone: String = "e_phone", noncePhone: String = "nonce_phone",
        ) {
            val e = assertFailsWith<E2E.Failure.BadHello> {
                E2E.verifyPc(h, label, channel, bytes(ePhone), bytes(noncePhone))
            }
            assertEquals(why, e.why)
        }
        rejects(hello, "label", label = "aaaaaaaaaaaaaaaa")
        rejects(hello, "sig", channel = channel + 1)
        rejects(hello, "sig", ePhone = "e_pc")
        rejects(hello, "sig", noncePhone = "nonce_pc")
        rejects(JsonObject(fields + ("v" to JsonPrimitive(2))), "version")
        val sig = Base64Url.decode(hello["sig"].str)!!
        sig[3] = (sig[3].toInt() xor 1).toByte()
        rejects(JsonObject(fields + ("sig" to JsonPrimitive(Base64Url.encode(sig)))), "sig")
    }

    @Test
    fun derivesTheSameKeys() {
        val (phoneToPc, pcToPhone) = E2E.deriveKeys(bytes("shared"), bytes("nonce_phone"), bytes("nonce_pc"))
        assertContentEquals(bytes("key_phone_to_pc"), phoneToPc)
        assertContentEquals(bytes("key_pc_to_phone"), pcToPhone)
    }

    @Test
    fun framesSealToTheSameBytes() {
        val phone = E2E.Session(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"))
        val pc = E2E.Session(bytes("key_pc_to_phone"), bytes("key_phone_to_pc"))
        for (f in v["frames"].array) {
            val sealed = Base64Url.decode(f["sealed"].str)!!
            val plain = f["plain"].str.encodeToByteArray()
            val (from, to) = if (f["from"].str == "pc") pc to phone else phone to pc
            assertContentEquals(sealed, from.seal(plain))
            assertContentEquals(plain, to.open(sealed))
        }
    }

    @Test
    fun framesOutOfOrderOrChangedDoNotOpen() {
        val a = E2E.Session(bytes("key_pc_to_phone"), bytes("key_phone_to_pc"))
        val b = E2E.Session(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"))
        val first = a.seal("one".encodeToByteArray())
        val second = a.seal("two".encodeToByteArray())
        assertFailsWith<E2E.Failure.OutOfOrder> { b.open(second) }
        assertEquals("one", b.open(first).decodeToString())
        assertFailsWith<E2E.Failure.OutOfOrder> { b.open(first) }
        val forged = second.copyOf()
        forged[12] = (forged[12].toInt() xor 1).toByte()
        assertFailsWith<E2E.Failure.DoesNotDecrypt> { b.open(forged) }
        assertEquals("two", b.open(second).decodeToString())
        assertFailsWith<E2E.Failure.DoesNotDecrypt> { b.open(ByteArray(3)) }
    }

    @Test
    fun handshakeHelloIsWellFormed() {
        val obj = parseJson(E2E.Handshake().hello)!!
        assertEquals(1, obj["v"].int)
        assertEquals(65, Base64Url.decode(obj["e"].str)?.size)
        assertEquals(16, Base64Url.decode(obj["nonce"].str)?.size)
    }

    @Test
    fun aPointOffTheCurveIsNoKey() {
        val good = P256.x963(P256.generate().public as ECPublicKey)
        val bad = good.copyOf().also { it[64] = (it[64].toInt() xor 1).toByte() }
        assertEquals(true, P256.publicKey(good) != null)
        assertNull(P256.publicKey(bad))
        assertNull(P256.publicKey(good.copyOf(64)))
    }

    /** The computer's side played here, as core/src/phone/e2e.rs answers a hello. */
    @Test
    fun agreesWithAComputerOnTheKeys() {
        val identity = KeyPairGenerator.getInstance("EC").run {
            initialize(ECGenParameterSpec("secp256r1"))
            generateKeyPair()
        }
        val pub = P256.x963(identity.public as ECPublicKey)
        val label = E2E.label(pub)
        val channel = 3
        val phone = E2E.Handshake()
        val hello = parseJson(phone.hello)!!
        val ePhone = Base64Url.decode(hello["e"].str)!!
        val noncePhone = Base64Url.decode(hello["nonce"].str)!!

        val eph = P256.generate()
        val ePc = P256.x963(eph.public as ECPublicKey)
        val noncePc = ByteArray(16).also(E2E.random::nextBytes)
        val signed = E2E.transcript(label, channel, ePhone, ePc, noncePhone, noncePc)
        val der = Signature.getInstance("SHA256withECDSA").run {
            initSign(identity.private)
            update(signed)
            sign()
        }
        val reply = buildJsonObject {
            put("v", 1)
            put("pub", Base64Url.encode(pub))
            put("e", Base64Url.encode(ePc))
            put("nonce", Base64Url.encode(noncePc))
            put("sig", Base64Url.encode(derToRaw(der)))
            put("arakun", "1.2.3")
        }.toString().encodeToByteArray()

        val wrong = assertFailsWith<E2E.Failure.BadHello> { phone.finish(reply, label, channel + 1) }
        assertEquals("sig", wrong.why)
        val session = phone.finish(reply, label, channel)
        assertEquals("1.2.3", session.arakun)
        // A second session from the same handshake would reuse its nonces.
        val again = assertFailsWith<E2E.Failure.BadHello> { phone.finish(reply, label, channel) }
        assertEquals("handshake already finished", again.why)

        val shared = KeyAgreement.getInstance("ECDH").run {
            init(eph.private)
            doPhase(P256.publicKey(ePhone), true)
            generateSecret()
        }
        val (phoneToPc, pcToPhone) = E2E.deriveKeys(shared, noncePhone, noncePc)
        val pc = E2E.Session(pcToPhone, phoneToPc)
        assertEquals("hi", pc.open(session.seal("hi".encodeToByteArray())).decodeToString())
        assertEquals("yo", session.open(pc.seal("yo".encodeToByteArray())).decodeToString())
    }
}

/** A DER ECDSA signature as the raw r‖s the computer sends. */
internal fun derToRaw(der: ByteArray): ByteArray {
    var i = 2
    fun integer(): ByteArray {
        check(der[i] == 0x02.toByte())
        val len = der[i + 1].toInt()
        val v = der.copyOfRange(i + 2, i + 2 + len)
        i += 2 + len
        val trimmed = v.dropWhile { it == 0.toByte() }.toByteArray()
        return ByteArray(32 - trimmed.size) + trimmed
    }
    return integer() + integer()
}

class PartTest {
    @Test
    fun joinsPiecesBackIntoTheMessage() {
        val v = load("part.json")
        for (c in v["cases"].array) {
            val j = Joiner()
            var whole: ByteArray? = null
            for (p in c["parts"].array) whole = j.add(parseJson(p.str)!!["part"])
            assertEquals(v["message"].str, whole?.decodeToString())
        }
    }

    private fun piece(s: String, more: Boolean = false) = buildJsonObject {
        put("body", Base64Url.encode(s.encodeToByteArray()))
        if (more) put("more", true)
    }

    @Test
    fun dropsAMessageTooBigToJoin() {
        val j = Joiner(most = 4)
        assertNull(j.add(piece("abc", more = true)))
        assertNull(j.add(piece("de", more = true)))
        // The rest of the dropped message is skipped, then the next one joins.
        assertNull(j.add(piece("f")))
        assertEquals("ok", j.add(piece("ok"))?.decodeToString())
    }
}

class BytesTest {
    @Test
    fun base32AndBase64Url() {
        assertEquals("mzxw6ytboi", Base32.encode("foobar".encodeToByteArray()))
        assertEquals("_-8", Base64Url.encode(byteArrayOf(-1, -17)))
        assertContentEquals(byteArrayOf(-1, -17), Base64Url.decode("_-8"))
        assertNull(Base64Url.decode("ab=="))
        assertNull(Base64Url.decode("a b"))
    }

    @Test
    fun readsOnlyWhatIsThere() {
        assertNull(ByteArray(7).readLong())
        assertEquals(0x0102L, byteArrayOf(0, 0, 0, 0, 0, 0, 1, 2).readLong())
    }
}

class PairingTest {
    @Test
    fun readsTheRelayLink() {
        val l = PairingLink.parse("https://lmx5lt63vsxoyes4.arakun.dev/#p=abc123&s=s%2Fecret")!!
        assertEquals("lmx5lt63vsxoyes4", l.label)
        assertEquals("lmx5lt63vsxoyes4.arakun.dev", l.host)
        assertEquals("abc123", l.code)
        assertEquals("s/ecret", l.secret)
        assertEquals(true, l.secure)
        assertEquals("lab.localhost:8787", PairingLink.parse("http://lab.localhost:8787/#p=a&s=b")?.host)
        assertNull(PairingLink.parse("https://x.arakun.dev/"))
        assertNull(PairingLink.parse("https://x.arakun.dev/#p=a"))
        assertNull(PairingLink.parse("not a link"))
    }

    @Test
    fun endKinds() {
        assertEquals(EndKind.UNPAIRED, EndKind.of("", 4401))
        assertEquals(EndKind.PAIRING, EndKind.of("bad_code", 1000))
        assertEquals(EndKind.UNTRUSTED, EndKind.of("bad_hello", 1000))
        assertEquals(EndKind.LIMIT, EndKind.of("limit:daily", 1000))
        assertEquals(EndKind.GONE, EndKind.of("", 1006))
    }
}
