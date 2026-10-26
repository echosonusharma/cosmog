package com.sonus.cosmog

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Starts NightWatchService from an alarm: after the dataSync FGS timeout cap
 * (A14+), and after boot on A15+ where the boot receiver may not start a
 * dataSync FGS itself. An exact alarm firing grants a temporary background
 * FGS-start allowlist, so start() succeeds here where a plain background start
 * would be refused. A refusal re-arms rather than ending the chain.
 */
class NwRestartReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        try {
            if (!NightWatchService.isEnabled(context)) return
            NightWatchService.startOrRetry(context)
        } catch (t: Throwable) {
            android.util.Log.w("NwRestartReceiver", "restart failed: $t")
        }
    }
}
