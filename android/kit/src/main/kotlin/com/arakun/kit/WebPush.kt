// Push notifications for the app, as the phone page gets them: the computer
// seals each notice for this phone (RFC 8291, aes128gcm, one record) and posts
// it to the endpoint the phone gave it. For the app that endpoint is the
// account host, which forwards the sealed body unread through FCM. The keys to
// open it never leave the phone. core/src/phone/push.rs is the sealing side.
package com.arakun.kit

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import java.math.BigInteger
import java.security.KeyFactory
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.ECPublicKey
import java.security.spec.ECPrivateKeySpec
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

object WebPush {
    /**
     * This phone's push keys: a P-256 pair and the auth secret. The computer
     * seals for the public key; the secret keeps anyone who only knows the
     * public key from making notices this phone would open.
     */
    class Keys(internal val private: ECPrivateKey, val public: ByteArray, val auth: ByteArray) {
        /** For the sealed store: the private scalar, the public point and the secret. */
        fun encoded(): String = buildJsonObject {
            put("d", Base64Url.encode(P256.fixed(private.s)))
            put("pub", Base64Url.encode(public))
            put("auth", Base64Url.encode(auth))
        }.toString()

        override fun toString() = "WebPush.Keys"

        companion object {
            fun generate(): Keys {
                val pair = P256.generate()
                val auth = ByteArray(16).also(E2E.random::nextBytes)
                return Keys(pair.private as ECPrivateKey, P256.x963(pair.public as ECPublicKey), auth)
            }

            /** What [encoded] wrote, or null when it does not read as keys. */
            fun decode(text: String): Keys? {
                val j = parseJson(text) ?: return null
                val d = j["d"].string?.let(Base64Url::decode)?.takeIf { it.size == 32 } ?: return null
                val pub = j["pub"].string?.let(Base64Url::decode) ?: return null
                val auth = j["auth"].string?.let(Base64Url::decode)?.takeIf { it.size == 16 } ?: return null
                if (P256.publicKey(pub) == null) return null
                return Keys(privateKey(d) ?: return null, pub, auth)
            }

            /** The private key a raw 32-byte scalar names. */
            internal fun privateKey(d: ByteArray): ECPrivateKey? = try {
                KeyFactory.getInstance("EC").generatePrivate(ECPrivateKeySpec(BigInteger(1, d), P256.params)) as ECPrivateKey
            } catch (_: Exception) {
                null
            }
        }
    }

    class Failure(why: String) : Exception(why)

    /**
     * The message inside a body sealed for `keys`, as RFC 8291 lays it out:
     * salt (16), record size (4), key id length (1), the sender's public key,
     * then one record. Throws [Failure] for anything else.
     */
    fun open(keys: Keys, body: ByteArray): ByteArray {
        if (body.size < 21) throw Failure("too short")
        val salt = body.copyOfRange(0, 16)
        val rs = body.copyOfRange(16, 20).fold(0L) { acc, b -> (acc shl 8) or (b.toLong() and 0xFF) }
        val idLen = body[20].toInt() and 0xFF
        if (idLen != 65) throw Failure("key id")
        val start = 21 + idLen
        if (body.size < start + 17) throw Failure("too short")
        val senderKey = body.copyOfRange(21, start)
        val record = body.copyOfRange(start, body.size)
        // One record carries the whole message: a longer body would be several.
        if (rs < 18 || record.size > rs) throw Failure("more than one record")
        // A point off the curve could leak the private key through ECDH.
        val sender = P256.publicKey(senderKey) ?: throw Failure("sender key")
        val shared = try {
            KeyAgreement.getInstance("ECDH").run {
                init(keys.private)
                doPhase(sender, true)
                generateSecret()
            }
        } catch (_: Exception) {
            throw Failure("sender key")
        }
        val (cek, nonce) = derive(shared, keys.auth, keys.public, senderKey, salt)
        val plain = try {
            Cipher.getInstance("AES/GCM/NoPadding").run {
                init(Cipher.DECRYPT_MODE, SecretKeySpec(cek, "AES"), GCMParameterSpec(128, nonce))
                doFinal(record)
            }
        } catch (_: Exception) {
            throw Failure("does not decrypt")
        }
        // Padding is zeros after the delimiter; the last record's delimiter is 2.
        var end = plain.size
        while (end > 0 && plain[end - 1] == 0.toByte()) end--
        if (end == 0 || plain[end - 1] != 2.toByte()) throw Failure("not the last record")
        return plain.copyOfRange(0, end - 1)
    }

    /** The content key and nonce both sides reach from the shared secret (RFC 8291 section 3). */
    internal fun derive(
        shared: ByteArray, auth: ByteArray, uaPublic: ByteArray, asPublic: ByteArray, salt: ByteArray,
    ): Pair<ByteArray, ByteArray> {
        val ikm = E2E.hkdf(shared, auth, "WebPush: info\u0000".encodeToByteArray() + uaPublic + asPublic, 32)
        val cek = E2E.hkdf(ikm, salt, "Content-Encoding: aes128gcm\u0000".encodeToByteArray(), 16)
        val nonce = E2E.hkdf(ikm, salt, "Content-Encoding: nonce\u0000".encodeToByteArray(), 12)
        return cek to nonce
    }

    /**
     * Where the account host takes notices for this phone's FCM token, or
     * null when the host is not https (the computer only sends there).
     */
    fun fcmEndpoint(base: String, token: String): String? {
        if (!base.startsWith("https://") || token.isEmpty()) return null
        return base.trimEnd('/').toHttpUrlOrNull()?.newBuilder()
            ?.addPathSegments("push/v1/fcm")
            ?.addPathSegment(token)
            ?.build()?.toString()
    }

    /** `POST /push` with where to send and the notices wanted, as the page sends it. */
    fun subscribeBody(endpoint: String, keys: Keys, wanted: Wanted): String = buildJsonObject {
        put("subscription", buildJsonObject {
            put("endpoint", endpoint)
            put("keys", buildJsonObject {
                put("p256dh", Base64Url.encode(keys.public))
                put("auth", Base64Url.encode(keys.auth))
            })
        })
        put("notices", wanted.json())
    }.toString()

    /** `POST /push` that stops the notices from that computer. */
    fun unsubscribeBody(): String = JsonObject(mapOf("subscription" to JsonNull)).toString()

    /** `POST /push` with only the notices wanted. */
    fun wantedBody(wanted: Wanted): String = buildJsonObject { put("notices", wanted.json()) }.toString()

    /** Which notices this phone wants; everything until the person says otherwise, as on the computer. */
    data class Wanted(
        val done: Boolean = true,
        val input: Boolean = true,
        val error: Boolean = true,
        /** How long the person must have been away from the computer before a notice comes; 0 sends them always. */
        val awayAfterSecs: Int = 120,
        /** A turn that finishes sooner than this sends no notice; 0 lets every one through. */
        val minTurnSecs: Int = 30,
    ) {
        fun json(): JsonObject = buildJsonObject {
            put("done", done)
            put("input", input)
            put("error", error)
            put("away_after_secs", awayAfterSecs)
            put("min_turn_secs", minTurnSecs)
        }

        fun wants(kind: String): Boolean = when (kind) {
            "done" -> done
            "input" -> input
            "error" -> error
            else -> true
        }

        companion object {
            fun of(j: JsonElement): Wanted = Wanted(
                done = j["done"].boolOr(true),
                input = j["input"].boolOr(true),
                error = j["error"].boolOr(true),
                awayAfterSecs = j["away_after_secs"].int?.takeIf { it >= 0 } ?: 120,
                minTurnSecs = j["min_turn_secs"].int?.takeIf { it >= 0 } ?: 30,
            )

            private fun JsonElement.boolOr(default: Boolean): Boolean =
                (this as? kotlinx.serialization.json.JsonPrimitive)?.takeIf { !it.isString }?.content?.toBooleanStrictOrNull() ?: default
        }
    }

    /** One entry of a list of times to pick from. */
    data class Choice(val secs: Int, val label: String)

    /** "Only when I'm away for", as the phone page offers it. */
    val AWAY_CHOICES = listOf(
        Choice(0, "Always"), Choice(60, "1 min"), Choice(120, "2 min"), Choice(300, "5 min"), Choice(900, "15 min"),
    )

    /** "Turns shorter than this stay quiet", as the phone page offers it. */
    val SHORTEST_CHOICES = listOf(
        Choice(0, "Off"), Choice(15, "15 s"), Choice(30, "30 s"), Choice(60, "1 min"), Choice(300, "5 min"),
    )

    /**
     * `choices`, with `secs` added when it is not one of them (set from another
     * device), so a picker always shows what the computer keeps.
     */
    fun choicesKeeping(choices: List<Choice>, secs: Int): List<Choice> =
        if (choices.any { it.secs == secs }) choices else choices + Choice(secs, "$secs s")

    /** `POST /mute`: one session's notices off, or on again. */
    fun muteBody(session: String, muted: Boolean): String = buildJsonObject {
        put("session", session)
        put("muted", muted)
    }.toString()

    /** The muted sessions in the computer's answer to `POST /mute`; null when it sent none. */
    fun muteReply(j: JsonElement): List<String>? =
        if (j["muted"] is kotlinx.serialization.json.JsonArray) j["muted"].array.mapNotNull { it.string } else null

    /** The muted sessions in `GET /me`; none from a computer that knows no muting. */
    fun mutedOf(me: JsonElement): List<String> = muteReply(me) ?: emptyList()

    /** Whether the computer knows muting: one from before sends no `muted` in `GET /me`. */
    fun knowsMuting(me: JsonElement): Boolean = muteReply(me) != null

    /**
     * The environment, and how long the turn took: `app`, `app · 4m 12s` from a minute
     * up, `app · 42s` under it. Empty without an environment.
     */
    fun subtitle(env: String, secs: Long?): String {
        if (env.isEmpty()) return ""
        if (secs == null || secs < 0) return env
        return if (secs >= 60) "$env \u00b7 ${secs / 60}m ${secs % 60}s" else "$env \u00b7 ${secs}s"
    }

    /** What the computer says in a notice: `kind` is `done`, `input` or `error`. */
    data class Notice(
        val kind: String,
        val session: String,
        val title: String,
        val body: String,
        /** The environment the session runs in; empty from an older computer. */
        val env: String = "",
        /** How long the turn took, sent with `done` only. */
        val secs: Long? = null,
    ) {
        val subtitle: String get() = subtitle(env, secs)

        companion object {
            fun of(plain: ByteArray): Notice? {
                val j = parseJson(plain)?.takeIf { it.isObject } ?: return null
                val session = j["session"].string?.takeIf { it.isNotEmpty() } ?: return null
                val secs = j["secs"].int?.takeIf { it >= 0 }?.toLong()
                return Notice(j["kind"].str, session, j["title"].str, j["body"].str, j["env"].str, secs)
            }
        }
    }
}

/** This phone's push keys, made once and kept in the sealed store. */
class PushKeeper(private val store: SealedStore) {
    /** The keys kept, or new ones kept now; null when they could not be kept. */
    fun keys(): WebPush.Keys? {
        store.read(KEYS)?.let(WebPush.Keys::decode)?.let { return it }
        // New keys orphan every subscription made with the old ones, so only
        // when there are none, or they no longer open.
        val made = WebPush.Keys.generate()
        return if (store.write(KEYS, made.encoded())) made else null
    }

    /** The keys kept, without making any. */
    fun kept(): WebPush.Keys? = store.read(KEYS)?.let(WebPush.Keys::decode)

    companion object {
        const val KEYS = "push_keys"
    }
}
