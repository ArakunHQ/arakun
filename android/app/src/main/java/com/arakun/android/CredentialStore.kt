package com.arakun.android

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import com.arakun.kit.SealedStore
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The account's session and the computer picked last, sealed with a key the
 * Android Keystore holds and never lets out: the session lets anyone holding
 * it into every computer of the account. The app's data is left out of
 * backups and device transfers too. Before sign-in the relay pairing's
 * credentials were kept here, under `credentials`.
 */
class CredentialStore(context: Context) : SealedStore {
    private val prefs = context.getSharedPreferences("relay", Context.MODE_PRIVATE)

    private fun secretKey(): SecretKey {
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

    override fun has(key: String): Boolean = prefs.contains(key)

    override fun read(key: String): String? = try {
        prefs.getString(key, null)?.let { Base64.decode(it, Base64.NO_WRAP) }?.let {
            val cipher = Cipher.getInstance(TRANSFORM)
            cipher.init(Cipher.DECRYPT_MODE, secretKey(), GCMParameterSpec(128, it, 0, 12))
            cipher.doFinal(it, 12, it.size - 12).decodeToString()
        }
    } catch (_: Exception) {
        null
    }

    // commit, not apply: the caller tells the user when the sign-in was not kept.
    @SuppressLint("ApplySharedPref", "UseKtx")
    override fun write(key: String, value: String?): Boolean = try {
        if (value == null) {
            prefs.edit().remove(key).commit()
        } else {
            val cipher = Cipher.getInstance(TRANSFORM)
            cipher.init(Cipher.ENCRYPT_MODE, secretKey())
            val sealed = cipher.iv + cipher.doFinal(value.encodeToByteArray())
            prefs.edit().putString(key, Base64.encodeToString(sealed, Base64.NO_WRAP)).commit()
        }
    } catch (_: Exception) {
        false
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "com.arakun.android.relay"
        const val TRANSFORM = "AES/GCM/NoPadding"
    }
}
