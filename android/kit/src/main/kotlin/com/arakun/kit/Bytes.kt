package com.arakun.kit

import java.util.Base64

/** Base64url without padding, as the computer and the phone page write it. */
object Base64Url {
    fun encode(bytes: ByteArray): String = Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    fun decode(s: String): ByteArray? {
        if (!s.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '-' || it == '_' }) return null
        return try {
            Base64.getUrlDecoder().decode(s)
        } catch (_: IllegalArgumentException) {
            null
        }
    }
}

/** RFC 4648 base32 without padding, lower case: 10 bytes make 16 characters. */
internal object Base32 {
    private const val ALPHABET = "abcdefghijklmnopqrstuvwxyz234567"

    fun encode(bytes: ByteArray): String {
        val out = StringBuilder()
        var acc = 0
        var bits = 0
        for (b in bytes) {
            acc = (acc shl 8) or (b.toInt() and 0xFF)
            bits += 8
            while (bits >= 5) {
                out.append(ALPHABET[(acc shr (bits - 5)) and 31])
                bits -= 5
            }
            // Only the low bits still to be read matter.
            acc = acc and ((1 shl bits) - 1)
        }
        if (bits > 0) out.append(ALPHABET[(acc shl (5 - bits)) and 31])
        return out.toString()
    }
}

/** Big-endian bytes of an integer. */
internal fun Long.bigEndian(): ByteArray = ByteArray(8) { i -> (this ushr (56 - 8 * i)).toByte() }

internal fun Int.bigEndian(): ByteArray = ByteArray(4) { i -> (this ushr (24 - 8 * i)).toByte() }

/** The big-endian u64 at the start, or null when there are fewer than 8 bytes. */
internal fun ByteArray.readLong(): Long? {
    if (size < 8) return null
    var v = 0L
    for (i in 0 until 8) v = (v shl 8) or (this[i].toLong() and 0xFF)
    return v
}
