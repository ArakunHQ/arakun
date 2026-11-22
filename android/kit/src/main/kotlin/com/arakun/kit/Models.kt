// What the computer sends, as core/src/phone/wire.rs writes it. Times are
// milliseconds since 1970.
package com.arakun.kit

import kotlinx.serialization.json.JsonElement

enum class SessionStatus {
    IDLE, WORKING, NEEDS_INPUT, ERROR;

    companion object {
        fun of(j: JsonElement): SessionStatus = when (j.str) {
            "working" -> WORKING
            "needs_input" -> NEEDS_INPUT
            "error" -> ERROR
            else -> IDLE
        }
    }
}

private fun JsonElement.millis(): Long = ((double ?: 0.0) * 1000).toLong()

/** A session's line in the inbox. */
data class InboxSession(
    val id: String,
    val title: String,
    val agent: String,
    val status: SessionStatus,
    val updated: Long,
    val added: Int,
    val removed: Int,
    val project: String,
    val env: String,
    val place: String,
    val pr: String?,
    /** What a waiting session asks: a permission or a question. */
    val asks: String?,
) {
    companion object {
        fun of(j: JsonElement) = InboxSession(
            id = j["id"].str,
            title = j["title"].str,
            agent = j["agent"].str,
            status = SessionStatus.of(j["status"]),
            updated = j["updated"].millis(),
            added = j["added"].int ?: 0,
            removed = j["removed"].int ?: 0,
            project = j["project"].str,
            env = j["env"].str,
            place = j["place"].str,
            pr = j["pr"].string,
            asks = j["asks"]["text"].string,
        )
    }
}

/** Where a session can start. */
data class Place(
    val path: String,
    val project: String,
    val env: String,
    val branch: String?,
    val agent: String,
) {
    companion object {
        fun of(j: JsonElement) = Place(
            path = j["path"].str,
            project = j["project"].str,
            env = j["env"].str,
            branch = j["branch"].string,
            agent = j["agent"].str,
        )
    }
}

data class Inbox(val host: String, val sessions: List<InboxSession>, val places: List<Place>) {
    companion object {
        fun of(j: JsonElement) = Inbox(
            host = j["host"].str,
            sessions = j["sessions"].array.map(InboxSession::of),
            places = j["places"].array.map(Place::of),
        )
    }
}

/** One entry of a session's transcript, cut down for a phone. */
sealed interface Entry {
    /** `images` are the names of the pictures the words mention. */
    data class User(
        val text: String, val at: Long, val elapsedMs: Int?, val pastes: List<String>, val images: List<String>,
    ) : Entry

    data class Agent(val text: String) : Entry
    data class Thought(val text: String) : Entry
    data class Tool(
        val id: String,
        val title: String,
        val kind: String,
        /** pending, in_progress, completed or failed. */
        val status: String,
        val command: String?,
        val output: String?,
        val exitCode: Int?,
        val added: Int,
        val removed: Int,
        val diffs: List<Diff>,
    ) : Entry

    data class Diff(val path: String, val added: Int, val removed: Int)

    data class Plan(val items: List<PlanItem>) : Entry

    /** `status` is pending, in_progress or completed. */
    data class PlanItem(val content: String, val status: String)

    data class Notice(val text: String, val error: Boolean) : Entry
    data class Compacted(val before: Int?, val after: Int?) : Entry
    data object Unknown : Entry

    companion object {
        fun of(j: JsonElement): Entry = when (j["type"].str) {
            "user" -> User(
                text = j["text"].str,
                at = j["at"].millis(),
                elapsedMs = j["elapsed_ms"].int,
                pastes = j["pastes"].array.map { it["text"].str },
                images = j["images"].array.map { it["name"].str },
            )
            "agent" -> Agent(j["text"].str)
            "thought" -> Thought(j["text"].str)
            "tool" -> Tool(
                id = j["id"].str, title = j["title"].str, kind = j["kind"].str, status = j["status"].str,
                command = j["command"].string, output = j["output"].string, exitCode = j["exit_code"].int,
                added = j["added"].int ?: 0, removed = j["removed"].int ?: 0,
                diffs = j["diffs"].array.map { Diff(it["path"].str, it["added"].int ?: 0, it["removed"].int ?: 0) },
            )
            "plan" -> Plan(j["entries"].array.map { PlanItem(it["content"].str, it["status"].str) })
            "notice" -> Notice(j["text"].str, j["error"].bool)
            "compacted" -> Compacted(j["before"].int, j["after"].int)
            else -> Unknown
        }
    }
}

/** What the agent waits for the user to allow. */
data class PermissionAsk(val toolCallId: String, val title: String, val options: List<Option>) {
    /** `kind` is allow_once, allow_always, reject_once or reject_always. */
    data class Option(val id: String, val name: String, val kind: String)

    companion object {
        fun of(j: JsonElement): PermissionAsk? = if (j.isNull) null else PermissionAsk(
            toolCallId = j["tool_call_id"].str,
            title = j["title"].str,
            options = j["options"].array.map { Option(it["id"].str, it["name"].str, it["kind"].str) },
        )
    }
}

/** A questionnaire the agent waits on. */
data class QuestionAsk(val toolCallId: String, val questions: List<Question>) {
    data class Question(val topic: String, val question: String, val options: List<String>, val multi: Boolean)

    companion object {
        fun of(j: JsonElement): QuestionAsk? = if (j.isNull) null else QuestionAsk(
            toolCallId = j["tool_call_id"].str,
            questions = j["questions"].array.map {
                Question(it["topic"].str, it["question"].str, it["options"].array.map { o -> o.str }, it["multi"].bool)
            },
        )
    }
}

data class Changes(val files: Int = 0, val added: Int = 0, val removed: Int = 0)

/**
 * A session open on the phone: its loaded entries `from until from+entries.size`
 * of `count`, and its state. Immutable: each message makes a new one.
 */
data class LiveSession(
    val id: String,
    val title: String,
    val agent: String,
    val status: SessionStatus = SessionStatus.IDLE,
    val count: Int = 0,
    val from: Int = 0,
    val entries: List<Entry> = emptyList(),
    val permission: PermissionAsk? = null,
    val question: QuestionAsk? = null,
    val usage: String? = null,
    val queue: List<String> = emptyList(),
    /** When the running turn started, by this phone's clock. */
    val turnStarted: Long? = null,
    val changes: Changes = Changes(),
) {
    /**
     * A `session` message: its tail of entries and its state. Opened again
     * after a reconnect, the tail overlaps what is loaded, older ones included.
     */
    fun applySession(j: JsonElement, now: Long = System.currentTimeMillis()): LiveSession {
        val tailFrom = j["from"].int ?: 0
        val tail = j["entries"].array.map(Entry::of)
        val (newFrom, newEntries) = if (entries.isNotEmpty() && tailFrom >= from && tailFrom <= from + entries.size) {
            from to entries.take(tailFrom - from) + tail
        } else {
            tailFrom to tail
        }
        return copy(
            from = newFrom,
            entries = newEntries,
            count = j["count"].int ?: (newFrom + newEntries.size),
            title = j["title"].string?.takeIf { it.isNotEmpty() } ?: title,
            agent = j["agent"].string ?: agent,
        ).applyState(j, now)
    }

    /** An `entries` message: from `from` on changed, or older ones (`older`). */
    fun applyEntries(j: JsonElement): LiveSession {
        val at = j["from"].int ?: 0
        val list = j["entries"].array.map(Entry::of)
        val newCount = j["count"].int ?: count
        return when {
            j["older"].bool -> copy(entries = list.take(maxOf(0, from - at)) + entries, from = at, count = newCount)
            at <= from -> copy(from = at, entries = list, count = newCount)
            else -> copy(entries = entries.take(at - from) + list, count = newCount)
        }
    }

    /** A `state` message (and the same fields of a `session` one). */
    fun applyState(j: JsonElement, now: Long = System.currentTimeMillis()): LiveSession {
        val c = j["changes"]
        return copy(
            status = SessionStatus.of(j["status"]),
            permission = PermissionAsk.of(j["permission"]),
            question = QuestionAsk.of(j["question"]),
            usage = j["usage"]["summary"].string,
            queue = j["queue"].array.map { it.str },
            title = j["title"].string?.takeIf { it.isNotEmpty() } ?: title,
            turnStarted = j["elapsed_ms"].double?.let { now - it.toLong() },
            changes = Changes(c["files"].int ?: 0, c["added"].int ?: 0, c["removed"].int ?: 0),
        )
    }
}
