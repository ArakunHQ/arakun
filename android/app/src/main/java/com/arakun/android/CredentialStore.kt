package com.arakun.android

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import com.arakun.kit.Credentials
import kotlinx.serialization.json.Json
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The pairing credentials, sealed with a key the Android Keystore holds and
 * never lets out: they are what lets anyone holding them into the computer's
 * sessions. The app's data is left out of backups and device transfers too.
 */
class CredentialStore(context: Context) {
    private val prefs = context.getSharedPreferences("relay", Context.MODE_PRIVATE)

    private fun key(): SecretKey {
        val store = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE).run {
            init(
                KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .build(),
            )
            generateKey()
        }
    }

    fun load(): Credentials? = try {
        val blob = prefs.getString(FIELD, null)?.let { Base64.decode(it, Base64.NO_WRAP) }
        blob?.let {
            val cipher = Cipher.getInstance(TRANSFORM)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, it, 0, 12))
            Json.decodeFromString(Credentials.serializer(), cipher.doFinal(it, 12, it.size - 12).decodeToString())
        }
    } catch (_: Exception) {
        null
    }

    /**
     * Stores `c`, or forgets the credentials when it is null. The write
     * replaces the old ones at once, so a failed one keeps them.
     */
    // commit, not apply: the caller tells the user when the pairing was not kept.
    @SuppressLint("ApplySharedPref", "UseKtx")
    fun save(c: Credentials?): Boolean = try {
        if (c == null) {
            prefs.edit().remove(FIELD).commit()
        } else {
            val cipher = Cipher.getInstance(TRANSFORM)
            cipher.init(Cipher.ENCRYPT_MODE, key())
            val sealed = cipher.iv + cipher.doFinal(Json.encodeToString(Credentials.serializer(), c).encodeToByteArray())
            prefs.edit().putString(FIELD, Base64.encodeToString(sealed, Base64.NO_WRAP)).commit()
        }
    } catch (_: Exception) {
        false
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "com.arakun.android.relay"
        const val TRANSFORM = "AES/GCM/NoPadding"
        const val FIELD = "credentials"
    }
}
