package com.arakun.kit

import java.security.interfaces.ECPublicKey
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class WebPushTest {
    private fun b64(s: String) = Base64Url.decode(s)!!

    /** The phone's keys from RFC 8291 Appendix A. */
    private val rfcKeys = WebPush.Keys(
        WebPush.Keys.privateKey(b64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94"))!!,
        b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
        b64("BTBZMqHH6r4Tts7J_aSIgg"),
    )

    private val rfcBody = b64(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
    )

    @Test
    fun opensTheRfc8291Example() {
        assertEquals("When I grow up, I want to be a watermelon", WebPush.open(rfcKeys, rfcBody).decodeToString())
    }

    /** As `encrypt` in core/src/phone/push.rs seals it, with `pad` zeros before the tag. */
    private fun seal(keys: WebPush.Keys, plain: ByteArray, pad: Int = 0, rs: Int = 4096): ByteArray {
        val sender = P256.generate()
        val senderPub = P256.x963(sender.public as ECPublicKey)
        val salt = ByteArray(16).also(E2E.random::nextBytes)
        val shared = KeyAgreement.getInstance("ECDH").run {
            init(sender.private)
            doPhase(P256.publicKey(keys.public), true)
            generateSecret()
        }
        val (cek, nonce) = WebPush.derive(shared, keys.auth, keys.public, senderPub, salt)
        val record = Cipher.getInstance("AES/GCM/NoPadding").run {
            init(Cipher.ENCRYPT_MODE, SecretKeySpec(cek, "AES"), GCMParameterSpec(128, nonce))
            doFinal(plain + byteArrayOf(2) + ByteArray(pad))
        }
        return salt + rs.bigEndian() + byteArrayOf(65) + senderPub + record
    }

    @Test
    fun aNoticeWithTheEnvironmentAndTurnLengthHasASubtitle() {
        val n = WebPush.Notice.of("""{"kind":"done","session":"s1","title":"t","body":"b","env":"app","secs":252}""".encodeToByteArray())!!
        assertEquals("app", n.env)
        assertEquals(252L, n.secs)
        assertEquals("app \u00b7 4m 12s", n.subtitle)
        assertEquals("app \u00b7 42s", WebPush.subtitle("app", 42))
        assertEquals("app \u00b7 1m 0s", WebPush.subtitle("app", 60))
        assertEquals("app", WebPush.subtitle("app", null))
        assertEquals("", WebPush.subtitle("", 90))
    }

    @Test
    fun aNoticeWithoutThemHasNoSubtitleAndAnOldOneStillReads() {
        val old = WebPush.Notice.of("""{"kind":"done","session":"s1","title":"t","body":"b"}""".encodeToByteArray())!!
        assertEquals(WebPush.Notice("done", "s1", "t", "b"), old)
        assertEquals("", old.env)
        assertNull(old.secs)
        assertEquals("", old.subtitle)
    }

    @Test
    fun aNoticeSealedForThePhoneOpensOnItAndReads() {
        val keys = WebPush.Keys.generate()
        val json = """{"kind":"input","session":"s1","title":"Needs you","body":"Allow Bash?"}"""
        val plain = WebPush.open(keys, seal(keys, json.encodeToByteArray(), pad = 7))
        assertEquals(WebPush.Notice("input", "s1", "Needs you", "Allow Bash?"), WebPush.Notice.of(plain))
        assertNull(WebPush.Notice.of("""{"kind":"done"}""".encodeToByteArray()), "no session")
        assertNull(WebPush.Notice.of("nope".encodeToByteArray()))
    }

    @Test
    fun anythingElseIsRefused() {
        val keys = WebPush.Keys.generate()
        val body = seal(keys, "hi".encodeToByteArray())
        // Another phone's keys, a flipped bit, a cut body.
        assertFailsWith<WebPush.Failure> { WebPush.open(WebPush.Keys.generate(), body) }
        assertFailsWith<WebPush.Failure> { WebPush.open(keys, body.copyOf().also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() }) }
        assertFailsWith<WebPush.Failure> { WebPush.open(keys, body.copyOf(30)) }
        // A sender key off the curve is never used for ECDH.
        val offCurve = body.copyOf().also { it[21 + 64] = (it[21 + 64].toInt() xor 1).toByte() }
        assertFailsWith<WebPush.Failure> { WebPush.open(keys, offCurve) }
        // More than one record.
        assertFailsWith<WebPush.Failure> { WebPush.open(keys, seal(keys, ByteArray(100), rs = 50)) }
    }

    @Test
    fun theKeysKeepAndComeBackTheSame() {
        val store = object : SealedStore {
            val values = mutableMapOf<String, String>()
            override fun read(key: String) = values[key]
            override fun has(key: String) = key in values
            override fun write(key: String, value: String?): Boolean {
                if (value == null) values.remove(key) else values[key] = value
                return true
            }
        }
        assertNull(PushKeeper(store).kept())
        val made = PushKeeper(store).keys()!!
        val again = PushKeeper(store).keys()!!
        assertTrue(made.public.contentEquals(again.public))
        assertTrue(made.auth.contentEquals(again.auth))
        // The private half works after the round trip through the store.
        assertEquals("x", WebPush.open(again, seal(made, "x".encodeToByteArray())).decodeToString())
        assertFalse("d" in made.toString())
        assertNull(WebPush.Keys.decode("""{"d":"AA","pub":"AA","auth":"AA"}"""))
    }

    @Test
    fun theSubscriptionIsWhatTheComputerTakes() {
        val keys = WebPush.Keys.generate()
        val endpoint = WebPush.fcmEndpoint("https://app.arakun.dev/", "abc:DEF_1-2/3")
        assertEquals("https://app.arakun.dev/push/v1/fcm/abc:DEF_1-2%2F3", endpoint)
        assertNull(WebPush.fcmEndpoint("http://localhost:8787", "abc"))
        assertNull(WebPush.fcmEndpoint("https://app.arakun.dev", ""))

        val body = parseJson(WebPush.subscribeBody(endpoint!!, keys, WebPush.Wanted(done = false)))!!
        val sub = body["subscription"]
        assertEquals(endpoint, sub["endpoint"].string)
        assertEquals(65, Base64Url.decode(sub["keys"]["p256dh"].str)?.size)
        assertEquals(16, Base64Url.decode(sub["keys"]["auth"].str)?.size)
        assertEquals(WebPush.Wanted(done = false), WebPush.Wanted.of(body["notices"]))

        assertTrue(parseJson(WebPush.unsubscribeBody())!!["subscription"].isNull)
        assertNotNull(parseJson(WebPush.unsubscribeBody())!!.let { (it as kotlinx.serialization.json.JsonObject)["subscription"] })
        assertEquals(WebPush.Wanted(error = false), WebPush.Wanted.of(parseJson(WebPush.wantedBody(WebPush.Wanted(error = false)))!!["notices"]))
        assertEquals(WebPush.Wanted(), WebPush.Wanted.of(parseJson("""{"done":"yes"}""")!!))
    }

    @Test
    fun theAwayTimeAndShortestTurnDefaultWhenLeftOut() {
        val old = WebPush.Wanted.of(parseJson("""{"done":false}""")!!)
        assertEquals(120, old.awayAfterSecs)
        assertEquals(30, old.minTurnSecs)
        assertEquals(WebPush.Wanted(done = false), old)
        assertEquals(WebPush.Wanted(), WebPush.Wanted.of(parseJson("null")!!))
        // Not a number, or below zero: the default.
        val odd = WebPush.Wanted.of(parseJson("""{"away_after_secs":"soon","min_turn_secs":-5}""")!!)
        assertEquals(120, odd.awayAfterSecs)
        assertEquals(30, odd.minTurnSecs)
        val picked = WebPush.Wanted.of(parseJson("""{"away_after_secs":0,"min_turn_secs":300}""")!!)
        assertEquals(0, picked.awayAfterSecs)
        assertEquals(300, picked.minTurnSecs)
    }

    @Test
    fun theAwayTimeAndShortestTurnRoundTrip() {
        val w = WebPush.Wanted(input = false, awayAfterSecs = 900, minTurnSecs = 0)
        val back = parseJson(WebPush.wantedBody(w))!!["notices"]
        assertEquals(900, back["away_after_secs"].int)
        assertEquals(0, back["min_turn_secs"].int)
        assertEquals(w, WebPush.Wanted.of(back))
    }

    @Test
    fun theChoicesMatchThePhonePage() {
        assertEquals(listOf(0, 60, 120, 300, 900), WebPush.AWAY_CHOICES.map { it.secs })
        assertEquals(listOf("Always", "1 min", "2 min", "5 min", "15 min"), WebPush.AWAY_CHOICES.map { it.label })
        assertEquals(listOf(0, 15, 30, 60, 300), WebPush.SHORTEST_CHOICES.map { it.secs })
        assertEquals(listOf("Off", "15 s", "30 s", "1 min", "5 min"), WebPush.SHORTEST_CHOICES.map { it.label })
        // A value set elsewhere stays on the list, so the picker never shows nothing.
        assertEquals(WebPush.AWAY_CHOICES, WebPush.choicesKeeping(WebPush.AWAY_CHOICES, 120))
        assertEquals(WebPush.Choice(45, "45 s"), WebPush.choicesKeeping(WebPush.SHORTEST_CHOICES, 45).last())
    }

    @Test
    fun readsTheMutedSessions() {
        assertEquals(listOf("s1", "s2"), WebPush.mutedOf(parseJson("""{"muted":["s1","s2",3]}""")!!))
        // An older computer knows no muting: nothing is muted.
        assertEquals(emptyList<String>(), WebPush.mutedOf(parseJson("""{"notifying":false}""")!!))
    }

    @Test
    fun muteSaysWhichSessionAndWhether() {
        assertEquals(parseJson("""{"session":"s","muted":true}"""), parseJson(WebPush.muteBody("s", true)))
        assertEquals(parseJson("""{"session":"s","muted":false}"""), parseJson(WebPush.muteBody("s", false)))
        assertEquals(listOf("a", "b"), WebPush.muteReply(parseJson("""{"muted":["a","b"]}""")!!))
        assertEquals(emptyList<String>(), WebPush.muteReply(parseJson("""{"muted":[]}""")!!))
        assertNull(WebPush.muteReply(parseJson("""{"error":"no"}""")!!))
    }
}
