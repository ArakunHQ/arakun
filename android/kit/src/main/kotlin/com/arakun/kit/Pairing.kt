package com.arakun.kit

import kotlinx.serialization.json.JsonElement
import java.io.ByteArrayOutputStream
import java.net.URI

/** Whether `s` is shaped like a computer's label (cloud/src/relay/label.ts). */
fun isLabel(s: String): Boolean = LABEL_SHAPE.matches(s)

private val LABEL_SHAPE = Regex("[a-z2-7]{16}")

/**
 * A link or code from elsewhere, now that the relay lets devices in by
 * account: the app pairs with nothing by link, so each kind only gets a
 * sentence that says what to do instead.
 */
enum class LinkKind {
    /**
     * The relay's old pairing link, `https://<label>.<relay>/#p=<code>&s=<secret>`
     * (or a relay run by hand, with no label in its host).
     */
    RELAY,

    /** A link to the computer's own page on the network or tailnet: `http://<address>/?p=<code>`. */
    DIRECT,

    /** Anything else. */
    OTHER;

    /** What the app says when a link of this kind is opened. */
    val sentence: String
        get() = when (this) {
            RELAY -> "Relay links are no longer used. Sign in to reach your computers."
            DIRECT -> "This app reaches your computers through your GitHub account. " +
                "To pair on Same Wi-Fi or Tailscale, open the link in this phone's browser."
            OTHER -> "That is not a link from Arakun."
        }

    companion object {
        fun of(text: String): LinkKind {
            val uri = try {
                URI(text.trim())
            } catch (_: Exception) {
                return OTHER
            }
            val scheme = uri.scheme?.lowercase()
            if ((scheme != "https" && scheme != "http") || uri.host.isNullOrEmpty()) return OTHER
            if (fields(uri.rawFragment).let { "p" in it && "s" in it }) return RELAY
            if ("p" in fields(uri.rawQuery)) return DIRECT
            return OTHER
        }

        private fun fields(part: String?): Set<String> =
            part.orEmpty().split('&').mapNotNull { kv -> kv.split('=', limit = 2).takeIf { it.size == 2 && it[1].isNotEmpty() }?.get(0) }.toSet()
    }
}

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

/** Why a channel ended, from what the relay or the computer said (phone/relay.js's kindOf). */
enum class EndKind {
    /** The account's session is gone: sign in again. */
    SIGNED_OUT,

    /** The relay will not let this phone in to that computer; the detail says why. */
    REFUSED,

    /** The computer behind the address is not the one the account names. */
    UNTRUSTED,

    /** The relay has had enough for now. */
    LIMIT,

    /** Anything else: try again. */
    GONE;

    companion object {
        fun of(reason: String, code: Int): EndKind = when {
            reason == "signed_out" || (code == 4401 && reason.isEmpty()) -> SIGNED_OUT
            // `unpaired`: the computer's user removed this device there, so it is not dialed again.
            reason in setOf("blocked", "not_yours", "no_machine", "update", "disabled", "unpaired") -> REFUSED
            reason == "untrusted" || reason == "bad_hello" -> UNTRUSTED
            code == 4429 || reason.startsWith("limit:") -> LIMIT
            else -> GONE
        }

        /**
         * A refusal as the app says it, the phone page's words. `message` is
         * the operator's text, for `disabled`; `accountHost` is where to sign in.
         */
        fun sentence(reason: String, message: String?, accountHost: String): String = when (reason) {
            "blocked" -> "This GitHub account is blocked from the relay."
            "not_yours" -> "That computer is signed in to another GitHub account."
            "no_machine" -> "That computer is no longer on your account. Sign in again in Arakun on it."
            "unpaired" -> "That computer removed this phone. Pick it again to connect."
            "update" -> "Update Arakun on the computer and sign in at $accountHost"
            "disabled" -> message?.takeIf { it.isNotEmpty() } ?: "The relay is closed right now."
            else -> "The relay closed the connection."
        }
    }
}
