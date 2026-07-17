package com.sonus.cosmog

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

object SecretStore {
    private const val PREF_NAME = "cosmog_secrets"

    @Volatile
    private var cached: SharedPreferences? = null

    private fun prefs(ctx: Context): SharedPreferences {
        val existing = cached
        if (existing != null) return existing
        synchronized(this) {
            val again = cached
            if (again != null) return again
            val app = ctx.applicationContext
            val masterKey = MasterKey.Builder(app)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            val prefs = EncryptedSharedPreferences.create(
                app,
                PREF_NAME,
                masterKey,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
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
