package com.arakun.android

import com.arakun.kit.Base64Url
import com.arakun.kit.WebPush
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * Where FCM hands over what the account host forwarded: the computer's
 * sealed notice, base64url in the field `e`. Only this phone's keys open it;
 * anything that does not open is dropped without a word, and nothing of it
 * is logged.
 */
class PushService : FirebaseMessagingService() {
    override fun onMessageReceived(message: RemoteMessage) {
        val sealed = message.data["e"]?.let(Base64Url::decode) ?: return
        // Only keys already made: a notice can only be sealed for those.
        val keys = Push.keptKeys() ?: return
        val plain = try {
            WebPush.open(keys, sealed)
        } catch (_: WebPush.Failure) {
            return
        }
        WebPush.Notice.of(plain)?.let { Push.received(this, it) }
    }

    /** FCM changed this phone's token: the computer linked now hears it from the app. */
    override fun onNewToken(token: String) {
        Push.newToken.value = token
    }
}
