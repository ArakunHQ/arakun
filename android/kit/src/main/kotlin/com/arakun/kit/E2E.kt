// The phone's half of the end-to-end encryption with the computer through the
// relay, as phone/e2e.js does it (core/src/phone/e2e.rs is the computer's):
// P-256 ECDH for the shared secret, the computer's P-256 ECDSA signature over
// the whole handshake, HKDF-SHA256 for the two session keys, AES-256-GCM for
// the frames. Tests check it against phone/test/vectors.
package com.arakun.kit

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.io.ByteArrayOutputStream
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECFieldFp
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

object E2E {
    const val VERSION = 1
    private const val TRANSCRIPT_TAG = "arakun-phone-v1"
    private const val KEYS_INFO = "arakun-phone-v1 keys"

    sealed class Failure(message: String) : Exception(message) {
        data class BadHello(val why: String) : Failure("bad hello: $why")
        data object OutOfOrder : Failure("frame out of order") {
            private fun readResolve(): Any = OutOfOrder
        }
        data object DoesNotDecrypt : Failure("frame does not decrypt") {
            private fun readResolve(): Any = DoesNotDecrypt
        }
    }

    internal val random = SecureRandom()

    /** The label a computer's public key (65 bytes, uncompressed) derives. */
    fun label(publicKey: ByteArray): String =
        Base32.encode(MessageDigest.getInstance("SHA-256").digest(publicKey).copyOf(10))

    /**
     * What the computer signs: tag, label, channel (u32 big-endian), both
     * ephemeral keys, both nonces; the phone's before the computer's.
     */
    internal fun transcript(
        label: String, channel: Int, ePhone: ByteArray, ePc: ByteArray, noncePhone: ByteArray, noncePc: ByteArray,
    ): ByteArray = TRANSCRIPT_TAG.encodeToByteArray() + label.encodeToByteArray() + channel.bigEndian() +
        ePhone + ePc + noncePhone + noncePc

    /** The parts of the computer's hello, once it is trusted. */
    class PcHello(val pub: ByteArray, val e: ByteArray, val nonce: ByteArray, val arakun: String)

    /** Checks the computer's hello answers ours and comes from the key `label` names. */
    fun verifyPc(hello: JsonElement, label: String, channel: Int, ePhone: ByteArray, noncePhone: ByteArray): PcHello {
        val v = hello["v"]
        if (v !is JsonPrimitive || v.isString || v.content != VERSION.toString()) throw Failure.BadHello("version")
        fun field(name: String, len: Int): ByteArray {
            val d = hello[name].string?.let(Base64Url::decode)
            if (d == null || d.size != len) throw Failure.BadHello(name)
            return d
        }
        val pub = field("pub", 65)
        if (label(pub) != label) throw Failure.BadHello("label")
        val e = field("e", 65)
        val nonce = field("nonce", 16)
        val sig = field("sig", 64)
        val signed = transcript(label, channel, ePhone, e, noncePhone, nonce)
        val key = P256.publicKey(pub) ?: throw Failure.BadHello("sig")
        val ok = try {
            Signature.getInstance("SHA256withECDSA").run {
                initVerify(key)
                update(signed)
                verify(P256.rawToDer(sig))
            }
        } catch (_: Exception) {
            false
        }
        if (!ok) throw Failure.BadHello("sig")
        return PcHello(pub, e, nonce, hello["arakun"].str)
    }

    /**
     * The two session keys out of the shared secret: phone to computer, then
     * computer to phone. The salt is both nonces, the phone's first.
     */
    fun deriveKeys(shared: ByteArray, noncePhone: ByteArray, noncePc: ByteArray): Pair<ByteArray, ByteArray> {
        val bits = hkdf(shared, noncePhone + noncePc, KEYS_INFO.encodeToByteArray(), 64)
        return bits.copyOfRange(0, 32) to bits.copyOfRange(32, 64)
    }

    internal fun hkdf(ikm: ByteArray, salt: ByteArray, info: ByteArray, length: Int): ByteArray {
        fun hmac(key: ByteArray, data: ByteArray): ByteArray =
            Mac.getInstance("HmacSHA256").run {
                init(SecretKeySpec(key, "HmacSHA256"))
                doFinal(data)
            }
        val prk = hmac(salt, ikm)
        val out = ByteArrayOutputStream()
        var t = ByteArray(0)
        var i = 1
        while (out.size() < length) {
            t = hmac(prk, t + info + byteArrayOf(i.toByte()))
            out.write(t)
            i++
        }
        return out.toByteArray().copyOf(length)
    }

    /**
     * One encrypted channel: the key this side seals with, the key it opens
     * with, and a counter for each. Frames must be sealed and opened in order.
     */
    class Session(sealKey: ByteArray, openKey: ByteArray) {
        private val sealKey = SecretKeySpec(sealKey, "AES")
        private val openKey = SecretKeySpec(openKey, "AES")
        private var sealed = 0L
        private var opened = 0L
        private val aad = byteArrayOf(Frame.Kind.DATA.byte)
        var arakun = ""

        /** `plain` as a data frame payload: its number, then the ciphertext and tag. */
        fun seal(plain: ByteArray): ByteArray {
            val counter = sealed++
            val c = Cipher.getInstance("AES/GCM/NoPadding")
            c.init(Cipher.ENCRYPT_MODE, sealKey, GCMParameterSpec(128, nonce(counter)))
            c.updateAAD(aad)
            return counter.bigEndian() + c.doFinal(plain)
        }

        /** The plaintext of a data frame payload, which must be the next one. */
        fun open(payload: ByteArray): ByteArray {
            if (payload.size < 8 + 16) throw Failure.DoesNotDecrypt
            val counter = payload.readLong() ?: throw Failure.DoesNotDecrypt
            if (counter != opened) throw Failure.OutOfOrder
            val plain = try {
                val c = Cipher.getInstance("AES/GCM/NoPadding")
                c.init(Cipher.DECRYPT_MODE, openKey, GCMParameterSpec(128, nonce(opened)))
                c.updateAAD(aad)
                c.doFinal(payload, 8, payload.size - 8)
            } catch (_: Exception) {
                throw Failure.DoesNotDecrypt
            }
            opened++
            return plain
        }

        private companion object {
            /** Four zero bytes, then the counter as u64 big-endian. */
            fun nonce(counter: Long): ByteArray = ByteArray(4) + counter.bigEndian()
        }
    }

    /**
     * A handshake under way: `hello` goes to the computer, and `finish` takes
     * its answer and gives the session, or throws why it is not trusted.
     * It finishes once: two sessions from one handshake would share keys and
     * counters, and so reuse AES-GCM nonces.
     */
    class Handshake {
        private val key: KeyPair = P256.generate()
        private val nonce = ByteArray(16).also(random::nextBytes)
        private val ePhone = P256.x963(key.public as ECPublicKey)
        private var finished = false

        val hello: ByteArray = buildJsonObject {
            put("v", VERSION)
            put("e", Base64Url.encode(ePhone))
            put("nonce", Base64Url.encode(nonce))
        }.toString().encodeToByteArray()

        fun finish(reply: ByteArray, label: String, channel: Int): Session {
            if (finished) throw Failure.BadHello("handshake already finished")
            val obj = parseJson(reply) ?: throw Failure.BadHello("not json")
            if (obj !is kotlinx.serialization.json.JsonObject) throw Failure.BadHello("not json")
            val pc = verifyPc(obj, label, channel, ePhone, nonce)
            val theirs = P256.publicKey(pc.e) ?: throw Failure.BadHello("e")
            val shared = try {
                KeyAgreement.getInstance("ECDH").run {
                    init(key.private)
                    doPhase(theirs, true)
                    generateSecret()
                }
            } catch (_: Exception) {
                throw Failure.BadHello("e")
            }
            val (phoneToPc, pcToPhone) = deriveKeys(shared, nonce, pc.nonce)
            val session = Session(phoneToPc, pcToPhone)
            session.arakun = pc.arakun
            finished = true
            return session
        }
    }
}

/** P-256 keys as the protocol writes them: uncompressed points and raw r‖s signatures. */
internal object P256 {
    val params: ECParameterSpec by lazy { (generate().public as ECPublicKey).params }

    fun generate(): KeyPair = KeyPairGenerator.getInstance("EC").run {
        initialize(ECGenParameterSpec("secp256r1"), E2E.random)
        generateKeyPair()
    }

    private fun fixed(n: BigInteger): ByteArray {
        val b = n.toByteArray()
        return when {
            b.size == 32 -> b
            b.size > 32 -> b.copyOfRange(b.size - 32, b.size)
            else -> ByteArray(32 - b.size) + b
        }
    }

    fun x963(pub: ECPublicKey): ByteArray = byteArrayOf(4) + fixed(pub.w.affineX) + fixed(pub.w.affineY)

    /**
     * The key an uncompressed point names, or null when it is not a point on
     * the curve. Java's key factory does not check that, and ECDH with a point
     * off the curve can leak the private key.
     */
    fun publicKey(x963: ByteArray): ECPublicKey? {
        if (x963.size != 65 || x963[0] != 4.toByte()) return null
        val x = BigInteger(1, x963.copyOfRange(1, 33))
        val y = BigInteger(1, x963.copyOfRange(33, 65))
        val curve = params.curve
        val p = (curve.field as ECFieldFp).p
        if (x >= p || y >= p) return null
        val lhs = y.modPow(BigInteger.valueOf(2), p)
        val rhs = x.modPow(BigInteger.valueOf(3), p).add(curve.a.multiply(x)).add(curve.b).mod(p)
        if (lhs != rhs) return null
        return try {
            KeyFactory.getInstance("EC").generatePublic(ECPublicKeySpec(ECPoint(x, y), params)) as ECPublicKey
        } catch (_: Exception) {
            null
        }
    }

    /** A raw 64-byte r‖s signature in the DER form Java verifies. */
    fun rawToDer(raw: ByteArray): ByteArray {
        fun integer(bytes: ByteArray): ByteArray {
            val v = BigInteger(1, bytes).toByteArray()
            return byteArrayOf(0x02, v.size.toByte()) + v
        }
        val body = integer(raw.copyOfRange(0, 32)) + integer(raw.copyOfRange(32, 64))
        return byteArrayOf(0x30, body.size.toByte()) + body
    }
}

/** A frame between the phone and the relay: the type byte, then the payload. */
class Frame(val kind: Kind, val payload: ByteArray) {
    enum class Kind(val byte: Byte) {
        HELLO(0x01), DATA(0x02), CONTROL(0x03),
    }

    val encoded: ByteArray get() = byteArrayOf(kind.byte) + payload

    companion object {
        fun decode(bytes: ByteArray): Frame? {
            val first = bytes.firstOrNull() ?: return null
            val kind = Kind.entries.firstOrNull { it.byte == first } ?: return null
            return Frame(kind, bytes.copyOfRange(1, bytes.size))
        }
    }
}
