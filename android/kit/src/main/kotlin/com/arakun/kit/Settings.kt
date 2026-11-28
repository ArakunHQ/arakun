// A session's settings and the agents' plan limits, as core/src/phone/wire.rs
// writes them (`settings`, `plan_usage`).
package com.arakun.kit

import kotlinx.serialization.json.JsonElement

/**
 * One setting a device can change (the model, the reasoning), its value and
 * its choices, from a `settings` or an `options` message.
 */
data class SettingSet(val key: String, val label: String, val current: String, val options: List<Option>) {
    /** `disabled` says why the choice cannot be picked now, when it cannot. */
    data class Option(val id: String, val name: String, val detail: String?, val disabled: String?)

    /** The choice that is on: `picked` here, else the computer's current one. */
    fun on(picked: String?): String = picked ?: current

    companion object {
        const val MODEL = "modelId"
        const val EFFORT = "reasoningEffort"

        /** The list a `settings` or `options` message carries; one without a key is left out. */
        fun list(j: JsonElement): List<SettingSet> = j.array.mapNotNull { s ->
            val key = s["key"].string?.takeIf(String::isNotEmpty) ?: return@mapNotNull null
            SettingSet(
                key = key,
                label = s["label"].string?.takeIf(String::isNotEmpty) ?: key,
                current = s["current"].str,
                options = s["options"].array.mapNotNull { o ->
                    val id = o["id"].string ?: return@mapNotNull null
                    // The computer sends why as text; an older one may send true.
                    val disabled = o["disabled"].string ?: if (o["disabled"].bool) "" else null
                    Option(id, o["name"].string?.takeIf(String::isNotEmpty) ?: id, o["detail"].string, disabled)
                },
            )
        }

        /**
         * Picks `id` for `key` among `picked`. A new model drops the reasoning
         * picked for the old one: the new model may not offer it.
         */
        fun pick(picked: Map<String, String>, key: String, id: String): Map<String, String> {
            val next = picked + (key to id)
            return if (key == MODEL) next - EFFORT else next
        }

        /** What a `start` carries as `settings`: `[key, value]` pairs, the model first. */
        fun startPairs(picked: Map<String, String>): List<Pair<String, String>> =
            listOf(MODEL, EFFORT).mapNotNull { k -> picked[k]?.let { k to it } }
    }
}

/** The agents a session can start with, by id and name, as the phone page lists them. */
val AGENTS: List<Pair<String, String>> = listOf(
    "claude" to "Claude Code",
    "droid" to "Droid",
    "opencode" to "OpenCode",
    "codex" to "Codex",
    "devin" to "Devin",
    "cursor" to "Cursor",
    "antigravity" to "Antigravity",
)

/** The agent a place starts with: its own, else the first. */
fun Place.startAgent(): String = agent.ifEmpty { AGENTS.first().first }

/** An `options` message: what a new session of `agent` in `env` would start with. */
data class StartOptions(val env: String, val agent: String, val settings: List<SettingSet>) {
    companion object {
        fun of(j: JsonElement) = StartOptions(j["env"].str, j["agent"].str, SettingSet.list(j["settings"]))
    }
}

/** A `usage` message: both agents' plan limits, a card each. */
data class PlanLimits(val agents: List<Agent>) {
    /** One window: how full it is (0 to 100, or more once over) and when it resets. */
    data class Row(val label: String, val percent: Int, val detail: String?)

    data class Agent(
        /** claude or droid. */
        val agent: String,
        val name: String,
        val signedIn: Boolean,
        val plan: String?,
        val rows: List<Row>,
        /** The windows not running now, by label. */
        val idle: List<String>,
        /** Droid's extra usage balance, as text. */
        val balance: String?,
        val note: String?,
        /** normal, warn or danger: how the note reads. */
        val tone: String,
        /** When the numbers were read, in milliseconds since 1970; null when not said. */
        val at: Long?,
    )

    /** When the newest numbers were read, or null. */
    val at: Long? get() = agents.mapNotNull { it.at }.maxOrNull()

    companion object {
        fun of(j: JsonElement) = PlanLimits(
            j["agents"].array.map { a ->
                Agent(
                    agent = a["agent"].str,
                    name = a["name"].string?.takeIf(String::isNotEmpty) ?: a["agent"].str,
                    signedIn = a["signed_in"].bool,
                    plan = a["plan"].string?.takeIf(String::isNotEmpty),
                    rows = a["rows"].array.map { r -> Row(r["label"].str, maxOf(0, r["percent"].int ?: 0), r["detail"].string) },
                    idle = a["idle"].array.mapNotNull { it.string },
                    balance = a["balance"].string,
                    note = a["note"].string?.takeIf(String::isNotEmpty),
                    tone = a["tone"].string ?: "normal",
                    at = a["at"].double?.takeIf { it > 0 }?.let { (it * 1000).toLong() },
                )
            },
        )
    }
}
