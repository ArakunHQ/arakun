package com.arakun.kit

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.util.Base64
import kotlin.random.Random

/**
 * Photos sent with a prompt, as `upload` pieces (core/src/phone/upload.rs).
 * A relay frame is small, so a photo goes in pieces under an id the phone
 * makes up, and the `send` or `start` that follows names the ids.
 */
object Photos {
    /**
     * The most base64 one piece carries, as the phone page's SHOT_PIECE. The
     * computer decodes each piece alone, so it is a multiple of 4.
     */
    const val PIECE = 160 * 1024

    /** The most bytes one photo may hold (MAX_IMAGE_BYTES in core/src/agent/attach.rs). */
    const val MAX_BYTES = 5 * 1024 * 1024

    /** The `upload` messages that carry `bytes` as upload `id`; `session` is empty before one exists. */
    fun pieces(session: String, id: String, name: String, bytes: ByteArray): List<JsonObject> {
        val b64 = Base64.getEncoder().encodeToString(bytes)
        val of = maxOf(1, (b64.length + PIECE - 1) / PIECE)
        return (0 until of).map { i ->
            buildJsonObject {
                put("type", "upload")
                put("session", session)
                put("upload", id)
                put("name", name)
                put("part", i)
                put("of", of)
                put("data", b64.substring(minOf(i * PIECE, b64.length), minOf((i + 1) * PIECE, b64.length)))
            }
        }
    }

    /** A fresh id for photo `index` of one prompt: never the same twice, as the computer keeps them by id. */
    fun newId(index: Int, now: Long = System.currentTimeMillis()): String {
        val tail = (1..5).map { "abcdefghijklmnopqrstuvwxyz0123456789"[Random.nextInt(36)] }.joinToString("")
        return "$now-$index-$tail"
    }

    /** What a prompt says when it is only photos, as the phone page writes it. */
    fun textFor(text: String, count: Int): String = when {
        text.isNotBlank() || count == 0 -> text
        count == 1 -> "(photo)"
        else -> "($count photos)"
    }
}
