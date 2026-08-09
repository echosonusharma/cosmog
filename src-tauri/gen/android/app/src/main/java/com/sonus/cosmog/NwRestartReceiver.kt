package com.sonus.cosmog

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Relaunches NightWatchService after the dataSync FGS timeout cap (A14+). The
 * exact alarm that fires this grants a temporary background FGS-start allowlist,
 * so start() succeeds here where a plain background start would be refused. Only
 * restarts if Night Watcher is still enabled; the next foreground resume covers
 * the case where the start is refused anyway.
 */
class NwRestartReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        try {
            val enabled = context
                .getSharedPreferences(NightWatchService.PREFS_NAME, Context.MODE_PRIVATE)
                .getBoolean(NightWatchService.KEY_ENABLED, false)
            if (!enabled) return
            NightWatchService.start(context)
        } catch (t: Throwable) {
            android.util.Log.w("NwRestartReceiver", "restart failed: $t")
        }
    }
}
