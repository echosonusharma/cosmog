package com.sonus.cosmog

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build

/**
 * Relaunches NightWatchService after a device reboot if the user left Night
 * Watcher enabled. Registered for BOOT_COMPLETED and LOCKED_BOOT_COMPLETED so
 * it fires whether or not the device is unlocked.
 *
 * A12+ (API 31+) restriction: starting a dataSync foreground service from
 * BOOT_COMPLETED is not an allowed background-start exemption and throws
 * ForegroundServiceStartNotAllowedException, which would crash the resume. The
 * sanctioned fix is a WorkManager expedited worker, but WorkManager is not a
 * declared dependency of this module (and build.gradle.kts is out of scope for
 * this change), so we cannot enqueue one here. Fallback: on A12+ we defer the
 * FGS start by persisting a "boot pending" flag, which MainActivity.onResume
 * consumes the next time the app is foregrounded (an allowed start context).
 * Pre-A12 we start immediately as before.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        try {
            val enabled = context
                .getSharedPreferences(NightWatchService.PREFS_NAME, Context.MODE_PRIVATE)
                .getBoolean(NightWatchService.KEY_ENABLED, false)
            if (!enabled) return

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                // Cannot legally start a dataSync FGS from boot on A12+.
                NightWatchService.setBootPending(context, true)
            } else {
                NightWatchService.start(context)
            }
        } catch (t: Throwable) {
            android.util.Log.w("BootReceiver", "boot relaunch failed: $t")
        }
    }
}
