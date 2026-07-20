package com.sonus.cosmog

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

object SecretStore {
    private const val PREF_NAME = "cosmog_secrets"

    @Volatile
    private var cached: SharedPreferences? = null

    private fun createPrefs(app: Context): SharedPreferences {
        val masterKey = MasterKey.Builder(app)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        return EncryptedSharedPreferences.create(
            app,
            PREF_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    private fun prefs(ctx: Context): SharedPreferences {
        val existing = cached
        if (existing != null) return existing
        synchronized(this) {
            val again = cached
            if (again != null) return again
            val app = ctx.applicationContext
            val prefs = try {
                createPrefs(app)
            } catch (t: Throwable) {
                // Keystore master key and the encrypted prefs blob are out of
                // sync (backup restore, keystore rotation, corrupt file). The
                // stored secrets are unrecoverable at this point; wipe both
                // and start fresh rather than crash-looping at startup.
                android.util.Log.w("SecretStore", "encrypted prefs unreadable, resetting: $t")
                app.deleteSharedPreferences(PREF_NAME)
                try {
                    java.security.KeyStore.getInstance("AndroidKeyStore")
                        .apply { load(null) }
                        .deleteEntry(MasterKey.DEFAULT_MASTER_KEY_ALIAS)
                } catch (_: Throwable) {}
                createPrefs(app)
            }
            cached = prefs
            return prefs
        }
    }

    @JvmStatic
    fun set(ctx: Context, key: String, value: String) {
        prefs(ctx).edit().putString(key, value).apply()
    }

    @JvmStatic
    fun get(ctx: Context, key: String): String? {
        return prefs(ctx).getString(key, null)
    }

    @JvmStatic
    fun remove(ctx: Context, key: String) {
        prefs(ctx).edit().remove(key).apply()
    }
}
