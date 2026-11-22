// Crash reports, sent to Arakun's PostHog project only after the user says
// yes, as the desktop does it (src/telemetry.rs). The app asks once, on first
// launch, and the answer is changed in the inbox's menu.
//
// Only crashes: an uncaught exception's type, message and stack trace. No
// usage events, screen views, deep links (the pairing link holds a secret),
// feature flags or session replay, and no person profile. The message loses
// URLs, paths, emails and anything shaped like a secret before it is queued.
// Debug builds send nothing.
package com.arakun.android

import android.app.Application
import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.edit
import com.posthog.PersonProfiles
import com.posthog.PostHog
import com.posthog.PostHogEvent
import com.posthog.android.PostHogAndroid
import com.posthog.android.PostHogAndroidConfig

object Telemetry {
    private const val PREFS = "telemetry"
    private const val CRASHES = "crashes"

    /** Whether this build can send at all: not a debug build, and a project token. */
    private val possible = !BuildConfig.DEBUG && BuildConfig.POSTHOG_KEY.isNotEmpty()

    private var app: Application? = null
    private var started = false

    /** The user's answer: null until they have been asked. */
    var crashes by mutableStateOf<Boolean?>(null)
        private set

    /** Whether to ask now: a build that can send, and no answer yet. */
    val asking: Boolean get() = possible && crashes == null

    /** At launch, before anything can crash: reads the answer and starts if it was yes. */
    fun start(app: Application) {
        this.app = app
        val prefs = app.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        crashes = if (prefs.contains(CRASHES)) prefs.getBoolean(CRASHES, false) else null
        if (crashes == true) setUp(app)
    }

    /** The user's answer, from the first-launch question or the menu. */
    fun answer(yes: Boolean) {
        val app = app ?: return
        crashes = yes
        app.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit { putBoolean(CRASHES, yes) }
        when {
            !yes -> if (started) PostHog.optOut()
            started -> PostHog.optIn()
            else -> setUp(app)
        }
    }

    private fun setUp(app: Application) {
        if (!possible || started) return
        val config = PostHogAndroidConfig(
            projectToken = BuildConfig.POSTHOG_KEY,
            host = BuildConfig.POSTHOG_HOST,
            captureApplicationLifecycleEvents = false,
            captureDeepLinks = false,
            captureScreenViews = false,
            capturePushNotificationSubscriptions = false,
            capturePushNotificationOpened = false,
        ).apply {
            personProfiles = PersonProfiles.NEVER
            setDefaultPersonProperties = false
            preloadFeatureFlags = false
            sendFeatureFlagEvent = false
            sessionReplay = false
            errorTrackingConfig.autoCapture = true
            addBeforeSend(::onlyCrashes)
        }
        PostHogAndroid.setup(app, config)
        started = true
    }

    /** Lets only exceptions out, their messages scrubbed. */
    private fun onlyCrashes(event: PostHogEvent): PostHogEvent? {
        if (event.event != "\$exception") return null
        val props = event.properties ?: return event
        val list = props["\$exception_list"] as? List<*> ?: return event
        props["\$exception_list"] = list.map { item ->
            val entry = item as? Map<*, *> ?: return@map item
            entry.entries.associate { (k, v) ->
                k.toString() to (if (k == "value" && v is String) scrub(v) else v)
            }
        }
        return event
    }
}

private const val MAX_MESSAGE = 300

/**
 * `text` without what may be private: URLs, paths, emails, the value of a
 * `key=value` and anything shaped like a secret. The desktop's
 * `telemetry::scrub` rules, plus: a value whose name reads as a credential
 * (`password=`, `api_key:`) goes whatever its shape.
 */
fun scrub(text: String): String {
    val out = StringBuilder()
    var afterScheme = false
    text.split(' ').forEachIndexed { i, word ->
        if (i > 0) out.append(' ')
        val body = word.trimStart { it in "([{<\"'`" }
        val lead = word.substring(0, word.length - body.length)
        val core = body.trimEnd { it in ")]}>\"'`,.;:!?" }
        val tail = body.substring(core.length)
        val replaced = if (afterScheme && core.isNotEmpty()) "<secret>" else redact(core)
        afterScheme = core.lowercase() in setOf("bearer", "basic")
        out.append(lead).append(replaced ?: core).append(tail)
    }
    val s = out.toString()
    return if (s.length > MAX_MESSAGE) s.substring(0, MAX_MESSAGE) + "…" else s
}

/** What replaces `word`, if it needs replacing. */
private fun redact(word: String): String? {
    if (word.isEmpty()) return null
    if ("://" in word) return "<url>"
    if (word.length >= 3 && word[0].isLetter() && word[1] == ':' && (word[2] == '\\' || word[2] == '/')) return "<path>"
    // `key=value`, `key:value`: the name says what failed, the value may be secret.
    val sep = word.indexOfFirst { it == '=' || it == ':' }
    if (sep > 0 && sep < word.length - 1) {
        val name = word.substring(0, sep)
        if ('/' !in name && '\\' !in name) {
            val value = word.substring(sep + 1)
            val inner = value.trimStart { it in "([{<\"'`" }
            val quotes = value.substring(0, value.length - inner.length)
            // A credential's name says its value is secret, whatever its shape.
            val redacted = if (credentialName(name)) "<secret>" else redact(inner) ?: return null
            return name + word[sep] + quotes + redacted
        }
    }
    if ('@' in word && '.' in word && !word.startsWith('@')) return "<email>"
    if ('/' in word || '\\' in word || word.startsWith('~')) return "<path>"
    return if (secretShaped(word)) "<secret>" else null
}

private val CREDENTIAL_NAMES = listOf("pass", "pwd", "secret", "token", "key", "auth", "credential", "session", "cookie")

private fun credentialName(name: String): Boolean {
    val n = name.trimStart { it in "([{<\"'`" }.lowercase()
    return CREDENTIAL_NAMES.any { it in n }
}

private val SECRET_PREFIXES = listOf(
    "sk-", "sk_", "pk_", "rk_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_",
    "xox", "phc_", "phx_", "phs_", "AKIA", "AIza", "glpat-", "eyJ",
)

private fun secretShaped(word: String): Boolean {
    if (word.length >= 12 && SECRET_PREFIXES.any(word::startsWith)) return true
    val tokenish = word.all { it.isLetterOrDigit() && it.code < 128 || it in "-_+=." }
    val digits = word.count { it in '0'..'9' }
    val letters = word.count { it in 'a'..'z' || it in 'A'..'Z' }
    val hex = word.length >= 16 && word.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }
    return hex || (tokenish && word.length >= 20 && digits >= 2 && letters >= 2)
}
