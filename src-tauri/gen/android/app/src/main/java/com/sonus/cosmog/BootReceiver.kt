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
 * The boot broadcast is itself an exemption from the A12+ background FGS-start
 * restriction, so up to A14 we just start. A15 forbids a boot receiver from
 * starting a dataSync FGS, so there we hop through a short exact alarm, which
 * is still an exemption. Exact alarms denied leaves nothing legal to try: the
 * pending flag hands it to the next app open.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        try {
            if (!NightWatchService.isEnabled(context)) return
            NightWatchService.setBootPending(context, true)

            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM) {
                NightWatchService.start(context)
            } else if (NightWatchService.canScheduleExactAlarms(context)) {
                NightWatchService.scheduleRestart(context, NightWatchService.BOOT_RESTART_DELAY_MS)
            }
        } catch (t: Throwable) {
            android.util.Log.w("BootReceiver", "boot relaunch failed: $t")
        }
    }
}
