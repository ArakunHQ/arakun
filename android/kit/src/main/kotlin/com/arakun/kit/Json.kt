package com.arakun.kit

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull

// The app reads the computer's messages through these, so a field it does not
// know, or one a newer computer leaves out, never fails a whole message.

fun parseJson(text: String): JsonElement? = try {
    Json.parseToJsonElement(text)
} catch (_: Exception) {
    null
}

fun parseJson(bytes: ByteArray): JsonElement? = parseJson(bytes.decodeToString())

/** The field `key` of an object, or null (JSON's) for anything else. */
operator fun JsonElement.get(key: String): JsonElement = (this as? JsonObject)?.get(key) ?: JsonNull

val JsonElement.string: String? get() = (this as? JsonPrimitive)?.takeIf { it.isString }?.content
val JsonElement.str: String get() = string ?: ""
val JsonElement.double: Double? get() = (this as? JsonPrimitive)?.takeIf { !it.isString }?.doubleOrNull

/** Whole numbers only as far as Int reaches: a bigger one is null, not a wrap. */
val JsonElement.int: Int?
    get() {
        val d = double ?: return null
        val t = if (d < 0) kotlin.math.ceil(d) else kotlin.math.floor(d)
        return if (t >= Int.MIN_VALUE && t <= Int.MAX_VALUE) t.toInt() else null
    }

val JsonElement.bool: Boolean get() = (this as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull ?: false
val JsonElement.array: List<JsonElement> get() = (this as? JsonArray) ?: emptyList()
val JsonElement.isNull: Boolean get() = this is JsonNull

/**
 * Whether it is an object. A check rather than `is JsonObject`, whose smart
 * cast would make `j["key"]` the map's nullable lookup instead of the one above.
 */
val JsonElement.isObject: Boolean get() = this is JsonObject
