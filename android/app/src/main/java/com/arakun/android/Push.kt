// Push notifications, as the phone page gets them (phone/app.js, phone/sw.js):
// the app hands each computer it links to an endpoint on the account host and
// its own keys; the computer seals each notice for those keys, the account
// host forwards it unread through FCM, and PushService opens and shows it.
// Firebase is set up here by hand, so a build without Arakun's FCM options
// still runs and simply has no notifications.
package com.arakun.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.content.edit
import com.arakun.kit.PushKeeper
import com.arakun.kit.WebPush
import com.google.android.gms.tasks.Task
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

object Push {
    private const val PREFS = "push"
    private const val ON = "on"
    private const val DONE = "done"
    private const val INPUT = "input"
    private const val ERROR = "error"

    /** A tap on a notification: MainActivity opens the session named in [EXTRA_SESSION]. */
    const val OPEN_SESSION = "com.arakun.android.OPEN_SESSION"
    const val EXTRA_SESSION = "session"

    /** One notification per session: the newer replaces the older under the session's tag. */
    private const val ID = 1

    private var app: Application? = null

    /** Whether this build can get notifications: it has FCM's options and Firebase took them. */
    var available = false
        private set

    /** The person turned notifications on in the app. */
    var on by mutableStateOf(false)
        private set

    /** Which notices to be told about; every computer linked to gets the same. */
    var wanted by mutableStateOf(WebPush.Wanted())
        private set

    /** FCM's latest token, when it gave a new one while the app runs. */
    val newToken = MutableStateFlow<String?>(null)

    /** The session the app shows while it is on screen: a notice for it is not shown. */
    @Volatile
    var onScreen: String? = null

    fun start(app: Application) {
        this.app = app
        val prefs = app.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        on = prefs.getBoolean(ON, false)
        wanted = WebPush.Wanted(prefs.getBoolean(DONE, true), prefs.getBoolean(INPUT, true), prefs.getBoolean(ERROR, true))
        val options = listOf(BuildConfig.FCM_PROJECT_ID, BuildConfig.FCM_APP_ID, BuildConfig.FCM_API_KEY, BuildConfig.FCM_SENDER_ID)
        if (options.all { it.isNotEmpty() }) {
            available = try {
                FirebaseApp.initializeApp(
                    app,
                    FirebaseOptions.Builder()
                        .setProjectId(BuildConfig.FCM_PROJECT_ID)
                        .setApplicationId(BuildConfig.FCM_APP_ID)
                        .setApiKey(BuildConfig.FCM_API_KEY)
                        .setGcmSenderId(BuildConfig.FCM_SENDER_ID)
                        .build(),
                )
                true
            } catch (_: Exception) {
                false
            }
        }
        if (available) channels(app)
    }

    /** This phone's push keys, sealed by the Keystore; made the first time. */
    fun keys(): WebPush.Keys? = app?.let { PushKeeper(CredentialStore(it)).keys() }

    fun keptKeys(): WebPush.Keys? = app?.let { PushKeeper(CredentialStore(it)).kept() }

    /** This phone's FCM token, or null when there is none to be had now. */
    suspend fun token(): String? {
        if (!available) return null
        return FirebaseMessaging.getInstance().token.await()
    }

    /** Whether Android lets the app show notifications: on 13 and later, once the person allowed it. */
    fun allowed(context: Context): Boolean = NotificationManagerCompat.from(context).areNotificationsEnabled()

    fun turnOn() {
        if (!available) return
        on = true
        prefs()?.edit { putBoolean(ON, true) }
        // From now on FCM keeps the token fresh by itself.
        FirebaseMessaging.getInstance().isAutoInitEnabled = true
    }

    /**
     * Off, here and at FCM: with the token deleted, the account host answers
     * the computers this phone no longer links to that it is gone, and they
     * forget the subscription.
     */
    fun turnOff() {
        on = false
        prefs()?.edit { putBoolean(ON, false) }
        newToken.value = null
        if (!available) return
        FirebaseMessaging.getInstance().isAutoInitEnabled = false
        FirebaseMessaging.getInstance().deleteToken()
    }

    /**
     * Signed out: the token goes, so the computers stop reaching this phone
     * for that sign-in. Notifications stay on; the next link gets a new token.
     */
    fun dropToken() {
        if (!available || !on) return
        newToken.value = null
        FirebaseMessaging.getInstance().deleteToken()
    }

    fun want(w: WebPush.Wanted) {
        wanted = w
        prefs()?.edit {
            putBoolean(DONE, w.done)
            putBoolean(INPUT, w.input)
            putBoolean(ERROR, w.error)
        }
    }

    private fun prefs() = app?.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** A notice opened in PushService, on its own thread. */
    fun received(context: Context, n: WebPush.Notice) {
        // Turned off here before the computer heard: say nothing.
        if (!on || !wanted.wants(n.kind)) return
        if (onScreen == n.session) return
        show(context, n)
    }

    /** The person opened the session: its notification has nothing more to say. */
    fun seen(context: Context, session: String) {
        NotificationManagerCompat.from(context).cancel(session, ID)
    }

    @SuppressLint("MissingPermission")
    private fun show(context: Context, n: WebPush.Notice) {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        val open = Intent(context, MainActivity::class.java)
            .setAction(OPEN_SESSION)
            .putExtra(EXTRA_SESSION, n.session)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val tap = PendingIntent.getActivity(
            context,
            n.session.hashCode(),
            open,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val note = NotificationCompat.Builder(context, channel(n.kind))
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(n.title.ifEmpty { "Arakun" })
            .setContentText(n.body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(n.body))
            .setCategory(if (n.kind == INPUT) NotificationCompat.CATEGORY_REMINDER else NotificationCompat.CATEGORY_STATUS)
            .setAutoCancel(true)
            .setContentIntent(tap)
            .build()
        try {
            NotificationManagerCompat.from(context).notify(n.session, ID, note)
        } catch (_: SecurityException) {
            // The permission was taken back meanwhile.
        }
    }

    /** One channel per kind, so each can be silenced or made louder in Android's settings. */
    private fun channel(kind: String) = when (kind) {
        INPUT -> INPUT
        ERROR -> ERROR
        else -> DONE
    }

    private fun channels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        manager.createNotificationChannels(
            listOf(
                NotificationChannel(INPUT, "Needs permission or answer", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "The agent waits for a permission or an answer."
                },
                NotificationChannel(DONE, "Turn finished", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "The agent finished its turn."
                },
                NotificationChannel(ERROR, "Something went wrong", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "A turn failed."
                },
            ),
        )
    }

    private suspend fun <T> Task<T>.await(): T? = suspendCancellableCoroutine { done ->
        addOnCompleteListener { t -> done.resume(if (t.isSuccessful) t.result else null) }
    }
}
