package com.arakun.kit

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import java.io.ByteArrayOutputStream
import java.net.URI

/** Whether `s` is shaped like a computer's label (cloud/src/relay/label.ts). */
fun isLabel(s: String): Boolean = LABEL_SHAPE.matches(s)

private val LABEL_SHAPE = Regex("[a-z2-7]{16}")

/**
 * The link Arakun's Phone tab shows (as a QR code) to pair a phone through the
 * relay: `https://<label>.<relay>/#p=<code>&s=<secret>`. The label is the
 * host's first part, unless the relay is run by hand (`wrangler dev`) and the
 * link has none: the relay then says it. The fragment holds the code the relay admits the phone
 * with and the secret it shows the computer inside the encrypted channel.
 */
data class PairingLink(
    val host: String,
    val label: String,
    val code: String,
    val secret: String,
    /** `ws` for a relay run by hand (`wrangler dev`), `wss` otherwise. */
    val secure: Boolean,
) {
    companion object {
        fun parse(text: String): PairingLink? {
            val uri = try {
                URI(text.trim())
            } catch (_: Exception) {
                return null
            }
            val scheme = uri.scheme?.lowercase() ?: return null
            if (scheme != "https" && scheme != "http") return null
            val host = uri.host?.lowercase() ?: return null
            val fragment = uri.rawFragment ?: return null
            val fields = mutableMapOf<String, String>()
            for (pair in fragment.split('&').filter { it.isNotEmpty() }) {
                val kv = pair.split('=', limit = 2)
                if (kv.size != 2) return null
                fields[kv[0]] = percentDecode(kv[1]) ?: kv[1]
            }
            val code = fields["p"]?.takeIf { it.isNotEmpty() } ?: return null
            val secret = fields["s"]?.takeIf { it.isNotEmpty() } ?: return null
            val label = host.split('.').first().takeIf { it.isNotEmpty() } ?: return null
            return PairingLink(
                host = if (uri.port >= 0) "$host:${uri.port}" else host,
                label = label,
                code = code,
                secret = secret,
                secure = scheme == "https",
            )
        }

        /** %XX escapes only: a `+` stays a plus, as in the page's own reading. */
        private fun percentDecode(s: String): String? {
            if ('%' !in s) return s
            val out = ByteArrayOutputStream()
            var i = 0
            while (i < s.length) {
                val c = s[i]
                if (c == '%') {
                    if (i + 2 >= s.length) return null
                    val b = s.substring(i + 1, i + 3).toIntOrNull(16) ?: return null
                    out.write(b)
                    i += 3
                } else {
                    out.write(c.toString().encodeToByteArray())
                    i++
                }
            }
            return out.toByteArray().decodeToString()
        }
    }
}

/** What the computer gave this phone when it paired, and where to find it. */
@Serializable
data class Credentials(
    val host: String,
    val label: String,
    val secure: Boolean,
    val id: String,
    val ticket: String,
    val token: String,
    /** The computer's name, as it last said it. */
    val name: String,
)

/**
 * Joins the `{part: {body, more}}` pieces of a message too big for one relay
 * frame: given each piece in turn, it gives back the whole message with the
 * last one and null before that.
 * A message over `most` bytes is dropped, as core/src/phone/relay.rs's
 * Joiner does, so the computer cannot grow it without end.
 */
class Joiner(private val most: Int = 64 * 1024 * 1024) {
    private var parts = ByteArrayOutputStream()
    private var tooBig = false

    fun add(part: JsonElement): ByteArray? {
        val piece = Base64Url.decode(part["body"].str) ?: ByteArray(0)
        val more = part["more"].bool
        if (tooBig || parts.size().toLong() + piece.size > most) {
            // Skip the rest of this message; the next one starts clean.
            tooBig = more
            parts = ByteArrayOutputStream()
            return null
        }
        parts.write(piece)
        if (more) return null
        val whole = parts.toByteArray()
        parts = ByteArrayOutputStream()
        return whole
    }
}

/** Why a channel ended, from what the relay or the computer said. */
enum class EndKind {
    /** The credentials are no longer good: pair again. */
    UNPAIRED,

    /** The pairing link did not work. */
    PAIRING,

    /** The computer behind the address is not the one expected. */
    UNTRUSTED,

    /** The relay has had enough for now. */
    LIMIT,

    /** Anything else: try again. */
    GONE;

    companion object {
        fun of(reason: String, code: Int): EndKind = when {
            code == 4401 || reason in setOf("unauthorized", "revoked", "bad_ticket", "unpaired", "wrong_secret") -> UNPAIRED
            reason in setOf("no_pairing", "bad_code", "no_credentials") -> PAIRING
            reason == "untrusted" || reason == "bad_hello" -> UNTRUSTED
            code == 4429 || reason.startsWith("limit:") -> LIMIT
            else -> GONE
        }
    }
}
