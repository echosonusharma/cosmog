package com.sonus.cosmog

import android.app.Application

/**
 * Application subclass so the Rust ndk_context is initialized even when the
 * process starts headless (BootReceiver or NightWatchService) with no activity.
 * MainActivity also calls initNdkContext + initNwClasses, both idempotent.
 */
class CosmogApp : Application() {
    override fun onCreate() {
        super.onCreate()
        try {
            // Triggers System.loadLibrary via NativeBridge, then caches the
            // ndk_context on this (JVM) thread.
            NativeBridge.initNdkContext(applicationContext)
            // Cache NW app-class GlobalRefs on the JVM thread. find_class for
            // app classes fails from the native spawn_blocking threads the NW
            // Rust fns run on, so it must happen here. Idempotent on the Rust
            // side (OnceLock guards).
            initNwClasses()
        } catch (t: Throwable) {
            android.util.Log.w("CosmogApp", "native init failed: $t")
        }
    }

    // Implemented in Rust (saf.rs). Caches NightWatchService + NwTreePicker
    // classes as GlobalRefs. Must run on a JVM thread.
    external fun initNwClasses()
}
