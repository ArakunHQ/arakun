// The account client, as core/src/cloud.rs is the desktop's: GitHub's device
// flow, then the account host (cloud/) that turns a GitHub token into a
// session, lists the account's computers and its signed-in devices. Every
// call blocks, so callers run them off the main thread.
package com.arakun.kit

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.FormBody
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import java.util.concurrent.TimeUnit

object Account {
    /** Where the account, the relay and the phone page live. */
    const val HOST = "https://app.arakun.dev"

    /** The OAuth App [HOST] checks tokens against: `CLIENT_ID` in core/src/cloud.rs. */
    const val CLIENT_ID = "Ov23lipgmMeATCa5aTr3"

    /** What this app's sessions are, in the account's list of signed-in devices. */
    const val KIND = "android"

    /** Where the person enters the code the device flow shows. */
    const val VERIFY_URL = "https://github.com/login/device"

    /** The account host keeps the primary address, to reach people about their account. */
    internal const val SCOPE = "user:email"

    /**
     * Plain `http://` only to this machine: a session or a GitHub token
     * must never cross a network unencrypted.
     */
    fun localHttp(url: String): Boolean {
        if (!url.startsWith("http://")) return false
        val authority = url.removePrefix("http://").split('/', '?', '#').first()
        if ('@' in authority) return false
        val host = if (authority.startsWith('[')) {
            val end = authority.indexOf(']')
            if (end < 0) return false
            val port = authority.substring(end + 1)
            if (port.isNotEmpty() && !port.startsWith(':')) return false
            authority.substring(0, end + 1)
        } else {
            authority.substringBefore(':')
        }
        return host.lowercase() in setOf("localhost", "127.0.0.1", "[::1]")
    }

    /** Whether a session may be sent to `base`. */
    fun safeBase(base: String): Boolean = base.startsWith("https://") || localHttp(base)

    /**
     * A label or id to put in a path, or null. These come from the account
     * host, so anything that could reach another route (`/`, `..`, `%`) is refused.
     */
    fun pathPart(part: String): String? =
        part.takeIf { it.length in 1..128 && it.all { c -> c in 'a'..'z' || c in 'A'..'Z' || c in '0'..'9' || c == '-' || c == '_' } }

    /**
     * Whether the key the account host names for `machine` is the one its
     * label derives from. The handshake is then checked against this key.
     */
    fun keyMatches(machine: Machine): Boolean {
        val key = Base64Url.decode(machine.pub) ?: return false
        return key.size == 65 && isLabel(machine.label) && E2E.label(key) == machine.label
    }
}

/**
 * Why a call to the account host or to GitHub did not give what was asked.
 * The messages never hold a token.
 */
sealed class AccountError(message: String) : Exception(message) {
    /** 401: the session is not valid (any more). */
    class SignedOut : AccountError("The sign-in is no longer valid.")

    /** The operator blocked the account, with the reason they gave (maybe empty). */
    class Blocked(val reason: String) :
        AccountError(if (reason.isEmpty()) "This account is blocked." else "This account is blocked: $reason.")

    /** The request got no answer. */
    class Unreachable(detail: String) : AccountError("Could not reach the server ($detail).")

    /** Any other answer than the one expected, with what the server said. */
    class Refused(said: String) :
        AccountError(if (said.isEmpty()) "The server refused the request." else "The server answered: ${said.trimEnd('.')}.")
}

/** What the account answered about a session the relay turned down. */
enum class SessionCheck {
    /** The account takes it: only the relay did not. */
    STANDS,

    /** The account says 401 too: the session is over. */
    OVER,

    /** No answer either way (no network, the host in trouble): ask again later. */
    UNKNOWN,
}

/** What `POST /auth/github` answers. */
class SignedIn(
    val session: String,
    val sessionId: String,
    val githubId: Long,
    val login: String,
    val flags: List<String>,
    val plan: String,
) {
    // The session is as good as a password to every computer of the account.
    override fun toString() = "SignedIn(login=$login, sessionId=$sessionId)"
}

/** Who is signed in, as `GET /me` answers. */
data class Me(
    val githubId: Long,
    val login: String,
    val flags: List<String>,
    val plan: String,
    val notices: List<Notice>,
)

/** A message from the operator to this account, until it is read. */
data class Notice(val id: String, val text: String, val url: String?) {
    /** The link, only when it is https; anything else stays text. */
    val link: String? get() = url?.takeIf { Regex("^https://[^/]").containsMatchIn(it) }
}

/** A computer of the account on the relay, as `GET /machines` lists it. */
data class Machine(
    val label: String,
    /** Its public key, base64url: the handshake must be signed with it. */
    val pub: String,
    val name: String,
    /** `app` or `server`. */
    val kind: String,
    val online: Boolean,
    /** Seconds since 1970; 0 when never seen. */
    val lastSeenAt: Long,
)

/** A signed-in device of the account, as `GET /sessions` lists it. */
data class SignedInDevice(
    val id: String,
    val name: String,
    /** `app`, `server`, `phone`, `ios` or `android`. */
    val kind: String,
    val createdAt: Long,
    val lastUsedAt: Long,
    /** The session that asked: this phone. */
    val current: Boolean,
)

/** GitHub's answer to the first step of the device flow. */
class DeviceCode(
    val userCode: String,
    val verificationUri: String,
    val deviceCode: String,
    /** How long to wait between polls. */
    val intervalMs: Long,
    val expiresInMs: Long,
) {
    // The device code fetches the token until it expires.
    override fun toString() = "DeviceCode(userCode=$userCode)"
}

/** What to do after one poll of GitHub's token endpoint. */
sealed interface Poll {
    /** Ask again after this long, and keep that pace. */
    data class Wait(val intervalMs: Long) : Poll

    class Token(val token: String) : Poll {
        override fun toString() = "Token"
    }

    /** The code ran out before it was entered. */
    data object Expired : Poll

    /** The person turned the request down on GitHub. */
    data object Denied : Poll
    data class Failed(val message: String) : Poll

    companion object {
        /** The next step from GitHub's answer, polling every `intervalMs` so far. */
        fun next(answer: JsonElement, intervalMs: Long): Poll {
            answer["access_token"].string?.takeIf { it.isNotEmpty() }?.let { return Token(it) }
            val code = answer["error"].string ?: return Failed("GitHub sent neither a token nor an error.")
            return when (code) {
                "authorization_pending" -> Wait(intervalMs)
                // GitHub adds 5 s on each slow_down and says the new total; take
                // the longer, so we never poll faster than it asks.
                "slow_down" -> Wait(maxOf(intervalMs + 5_000, (answer["interval"].double?.toLong() ?: 0) * 1000))
                "expired_token" -> Expired
                "access_denied" -> Denied
                else -> Failed(answer["error_description"].string?.takeIf { it.isNotEmpty() } ?: "GitHub answered $code.")
            }
        }
    }
}

/**
 * Talks to the account host at `base` and to GitHub. The session goes in an
 * `Authorization` header, never in a URL.
 */
class AccountClient(
    val base: String = Account.HOST,
    private val client: OkHttpClient = defaultClient,
    /** GitHub's address; tests swap it. */
    private val github: String = "https://github.com",
) {
    /** The OAuth App to ask GitHub with: the known one for [Account.HOST], else `GET /auth/config`. */
    fun clientId(): String {
        if (base == Account.HOST) return Account.CLIENT_ID
        val (status, body) = send("GET", "/auth/config", null, null, SMALL)
        return json(status, body)["github_client_id"].string?.takeIf { it.isNotEmpty() }
            ?: throw AccountError.Refused("sign-in is not set up on this server")
    }

    /** Asks GitHub for a code the person enters at [DeviceCode.verificationUri]. */
    fun startDeviceFlow(clientId: String): DeviceCode {
        val form = FormBody.Builder().add("client_id", clientId).add("scope", Account.SCOPE).build()
        val (status, body) = run(githubPost("/login/device/code", form), SMALL)
        if (status != 200) throw AccountError.Refused(githubError(status, body))
        val j = parseJson(body) ?: throw unreadable()
        val deviceCode = j["device_code"].string ?: throw unreadable()
        val userCode = j["user_code"].string ?: throw unreadable()
        return DeviceCode(
            userCode = userCode,
            verificationUri = j["verification_uri"].string ?: Account.VERIFY_URL,
            deviceCode = deviceCode,
            // A zero interval would poll as fast as the phone can.
            intervalMs = maxOf(1L, j["interval"].double?.toLong() ?: 5L) * 1000,
            expiresInMs = (j["expires_in"].double?.toLong() ?: 900L) * 1000,
        )
    }

    /** One poll of GitHub's token endpoint, polling every `intervalMs` so far. */
    fun pollOnce(clientId: String, deviceCode: String, intervalMs: Long): Poll {
        val form = FormBody.Builder()
            .add("client_id", clientId)
            .add("device_code", deviceCode)
            .add("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
            .build()
        val (status, body) = run(githubPost("/login/oauth/access_token", form), SMALL)
        // GitHub's own trouble leaves the code valid: ask again at the same pace.
        if (status >= 500) return Poll.Wait(intervalMs)
        val j = parseJson(body)
        return when {
            j != null && j.isObject && (status == 200 || j["error"].string != null) -> Poll.next(j, intervalMs)
            // A 200 that is not JSON (a proxy's page, say) is not an answer.
            status == 200 -> throw unreadable()
            else -> throw AccountError.Refused(githubError(status, body))
        }
    }

    /**
     * Trades the GitHub token for a session. The account host asks GitHub
     * who the token belongs to and does not keep it. `name` and `kind` say
     * what the session is on, for the list of signed-in devices.
     */
    fun signIn(githubToken: String, name: String, kind: String = Account.KIND): SignedIn {
        val ask = buildJsonObject {
            put("access_token", githubToken)
            put("name", name)
            put("kind", kind)
        }
        val (status, body) = send("POST", "/auth/github", null, ask.toString().toRequestBody(JSON), SMALL)
        // No session yet, so a 401 is about the GitHub token, not a stale sign-in.
        if (status == 401) throw AccountError.Refused("GitHub did not accept the sign-in. Try again.")
        val j = json(status, body)
        return SignedIn(
            session = j["session"].string?.takeIf { it.isNotEmpty() } ?: throw unreadable(),
            sessionId = j["session_id"].str,
            githubId = j["github_id"].double?.toLong() ?: throw unreadable(),
            login = j["login"].string ?: throw unreadable(),
            flags = j["flags"].array.mapNotNull { it.string },
            plan = j["plan"].str,
        )
    }

    fun me(session: String): Me = readMe(json(session, "/me", SMALL))

    /**
     * Whether `session` still stands, asked when the relay turned it down at
     * its socket. The relay's word alone never ends a session: a header lost
     * on the way to it would read the same. Only the account's own 401 does.
     */
    fun stillStands(session: String): SessionCheck = try {
        me(session)
        SessionCheck.STANDS
    } catch (_: AccountError.SignedOut) {
        SessionCheck.OVER
    } catch (_: AccountError) {
        SessionCheck.UNKNOWN
    }

    fun machines(session: String): List<Machine> = readMachines(json(session, "/machines", LIST))

    fun sessions(session: String): List<SignedInDevice> = readDevices(json(session, "/sessions", LIST))

    /** Ends one of the account's sessions, this one or another. */
    fun signOutSession(session: String, id: String) = act(session, "DELETE", "/sessions/${part(id)}")

    /** Marks a notice read, so `/me` stops listing it. */
    fun readNotice(session: String, id: String) = act(session, "POST", "/notices/${part(id)}/read")

    /** Ends this session on the account host. */
    fun logout(session: String) = act(session, "POST", "/auth/logout")

    private fun part(s: String): String = Account.pathPart(s) ?: throw AccountError.Refused("\"$s\" is not a valid name")

    private fun json(session: String, path: String, limit: Long): JsonElement {
        val (status, body) = send("GET", path, session, null, limit)
        return json(status, body)
    }

    /** A call that answers nothing but its status. */
    private fun act(session: String, method: String, path: String) {
        val (status, body) = send(method, path, session, if (method == "GET") null else ByteArray(0).toRequestBody(), SMALL)
        if (status !in 200..299) throw refusal(status, body)
    }

    private fun send(method: String, path: String, session: String?, body: RequestBody?, limit: Long): Pair<Int, String> {
        // Sessions and GitHub tokens go in these requests.
        if (!Account.safeBase(base)) throw AccountError.Refused("$base must start with https:// (http:// only for localhost)")
        val req = Request.Builder().url(base.trimEnd('/') + path).method(method, body).header("Accept", "application/json")
        if (session != null) req.header("Authorization", "Bearer $session")
        return run(req.build(), limit)
    }

    private fun githubPost(path: String, form: FormBody): Request =
        Request.Builder().url(github.trimEnd('/') + path).post(form).header("Accept", "application/json").build()

    /** The status and at most `limit` bytes of the body. */
    private fun run(req: Request, limit: Long): Pair<Int, String> = try {
        client.newCall(req).execute().use { res ->
            val source = res.body.source()
            // A huge answer cannot run the app out of memory.
            val small = !source.request(limit + 1)
            res.code to if (small) source.readUtf8() else ""
        }
    } catch (e: java.io.IOException) {
        throw AccountError.Unreachable(e.message ?: e.javaClass.simpleName)
    }

    private fun json(status: Int, body: String): JsonElement {
        if (status !in 200..299) throw refusal(status, body)
        return parseJson(body) ?: throw unreadable()
    }

    companion object {
        private const val SMALL = 64L * 1024
        /** A list of machines or signed-in devices is a few hundred bytes a row. */
        private const val LIST = 1024L * 1024
        private val JSON = "application/json".toMediaType()

        private val defaultClient: OkHttpClient by lazy {
            OkHttpClient.Builder()
                .callTimeout(30, TimeUnit.SECONDS)
                // No route asked here redirects. A redirect followed would send
                // the body again, GitHub's token in it, to wherever it pointed.
                .followRedirects(false)
                .followSslRedirects(false)
                .build()
        }

        private fun unreadable() = AccountError.Refused("an answer that could not be read")

        /** The account host's errors are `{"error": "..."}`; a blocked account's also carries `reason`. */
        internal fun refusal(status: Int, body: String): AccountError {
            val j = parseJson(body)
            val said = j?.get("error")?.string ?: ""
            return when {
                status == 401 -> AccountError.SignedOut()
                status == 403 && said == "blocked" -> AccountError.Blocked(j?.get("reason")?.string ?: "")
                status == 429 -> AccountError.Refused("too many requests; try again in a minute")
                said.isEmpty() -> AccountError.Refused("HTTP $status")
                else -> AccountError.Refused(said)
            }
        }

        /** GitHub's errors are `{"error": "...", "error_description": "..."}`. */
        private fun githubError(status: Int, body: String): String {
            val j = parseJson(body)
            return j?.get("error_description")?.string ?: j?.get("error")?.string ?: "HTTP $status"
        }

        internal fun readMe(j: JsonElement): Me = Me(
            githubId = j["github_id"].double?.toLong() ?: throw unreadable(),
            login = j["login"].string ?: throw unreadable(),
            flags = j["flags"].array.mapNotNull { it.string },
            plan = j["plan"].str,
            notices = j["notices"].array.mapNotNull { n ->
                val id = n["id"].string ?: n["id"].double?.toLong()?.toString() ?: return@mapNotNull null
                Notice(id, n["text"].str, n["url"].string)
            },
        )

        /** A row without a label or a key cannot be reached, so it is left out. */
        internal fun readMachines(j: JsonElement): List<Machine> {
            if (j !is kotlinx.serialization.json.JsonArray) throw unreadable()
            return j.mapNotNull { m ->
                Machine(
                    label = m["label"].string ?: return@mapNotNull null,
                    pub = m["pub"].string ?: return@mapNotNull null,
                    name = m["name"].str,
                    kind = m["kind"].str,
                    online = m["online"].bool,
                    lastSeenAt = m["last_seen_at"].double?.toLong() ?: 0,
                )
            }
        }

        internal fun readDevices(j: JsonElement): List<SignedInDevice> {
            if (j !is kotlinx.serialization.json.JsonArray) throw unreadable()
            return j.mapNotNull { d ->
                SignedInDevice(
                    id = d["id"].string ?: return@mapNotNull null,
                    name = d["name"].str,
                    kind = d["kind"].str,
                    createdAt = d["created_at"].double?.toLong() ?: 0,
                    lastUsedAt = d["last_used_at"].double?.toLong() ?: 0,
                    current = d["current"].bool,
                )
            }
        }
    }
}

/**
 * The account's session on this phone. It lets anyone holding it into every
 * computer of the account, so it lives only in the sealed store.
 */
@Serializable
data class AccountSession(val host: String, val token: String, val id: String, val login: String) {
    override fun toString() = "AccountSession(host=$host, id=$id, login=$login)"
}

/**
 * Where the app keeps secrets: sealed with a key only this device holds
 * (the app's CredentialStore). Writing null removes the entry. A write
 * replaces the old value at once, so a failed one keeps it.
 */
interface SealedStore {
    fun read(key: String): String?
    fun write(key: String, value: String?): Boolean

    /** Whether there is a value under `key`, even one that no longer opens. */
    fun has(key: String): Boolean
}

/** The session and the computer picked last, both kept in the sealed store only. */
class AccountKeeper(private val store: SealedStore) {
    fun load(): AccountSession? = store.read(SESSION)?.let {
        try {
            kotlinx.serialization.json.Json.decodeFromString(AccountSession.serializer(), it)
        } catch (_: Exception) {
            null
        }
    }

    /** Keeps `s`, or forgets the session and the computer picked when it is null. */
    fun save(s: AccountSession?): Boolean {
        if (s == null) {
            val machineGone = store.write(MACHINE, null)
            return store.write(SESSION, null) && machineGone
        }
        return store.write(SESSION, kotlinx.serialization.json.Json.encodeToString(AccountSession.serializer(), s))
    }

    /** The label of the computer opened last, so the app opens it again. */
    var machine: String?
        get() = store.read(MACHINE)?.takeIf(::isLabel)
        set(value) {
            store.write(MACHINE, value)
        }

    /**
     * Before sign-in this app paired through the relay and kept a ticket and
     * a token for one computer. The relay no longer takes them. Only a store
     * that has them is written, so a normal launch writes nothing.
     */
    fun forgetOldPairing() {
        if (store.has(OLD_CREDENTIALS)) store.write(OLD_CREDENTIALS, null)
    }

    companion object {
        const val SESSION = "account"
        const val MACHINE = "machine"

        /** Where the relay pairing's credentials were kept. */
        const val OLD_CREDENTIALS = "credentials"
    }
}
