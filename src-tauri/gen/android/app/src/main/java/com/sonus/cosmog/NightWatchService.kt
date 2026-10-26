package com.sonus.cosmog

import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import androidx.core.app.NotificationCompat

/**
 * Foreground service for Night Watcher background sync. Unlike TransferService
 * (which dies with the webview when the app is swiped away), Night Watcher must
 * keep running headless so periodic syncs fire even with no activity present.
 *
 * Started/stopped from Rust via JNI (start/stop statics). The boot flag lets
 * BootReceiver relaunch it after a device restart.
 */
class NightWatchService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    // Implemented in Rust (night_watcher_headless.rs). Runs in THIS (:nightwatch)
    // process, independent of the Tauri/wry Activity, so background sync survives
    // the Activity being destroyed. Idempotent on the Rust side.
    // False = loop did not come up; Rust fails without throwing.
    private external fun startNwSync(): Boolean
    private external fun stopNwSync()

    override fun onCreate() {
        super.onCreate()
        ensureChannel(this)

        // Bounded so it auto-releases if the Rust loop dies; re-armed only while busy.
        acquireWakelock(this)

        val notif = buildNotification(this)
        // Android 12+ throws ForegroundServiceStartNotAllowedException if the
        // start races a background transition. Never let it crash the process.
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(FG_NOTIFICATION_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            } else {
                startForeground(FG_NOTIFICATION_ID, notif)
            }
        } catch (t: Throwable) {
            armRetry("startForeground refused: $t")
            releaseWakelock()
            stopSelf()
            return
        }
        foregrounded = true

        if (!startLoop()) bailToRetry()
    }

    // Kick the headless Rust sync loop in this process. Guarded so a native
    // failure never crashes the service.
    private fun startLoop(): Boolean {
        syncing = try {
            startNwSync()
        } catch (t: Throwable) {
            android.util.Log.w("NightWatchService", "startNwSync failed: $t")
            false
        }
        return syncing
    }

    // Every failure path re-arms: dying quietly leaves sync off with nothing
    // scheduled and nothing for the next app open to consume.
    private fun armRetry(reason: String) {
        android.util.Log.w("NightWatchService", "$reason; retrying later")
        setBootPending(this, true)
        scheduleRestart(this, nextRetryDelayMs(this))
        // Gates onStartCommand, so a start aimed at this dying instance is ignored.
        foregrounded = false
    }

    // No loop: drop the foreground state rather than show an "active" notification
    // over a dead service.
    private fun bailToRetry() {
        armRetry("sync loop did not start")
        releaseWakelock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Also runs when onCreate bailed on a refused startForeground, hence the gate.
        if (foregrounded) {
            // A start aimed at a live service skips onCreate, the only other place
            // the loop is kicked, so an idle service gets another chance here.
            if (!syncing) startLoop()
            // Only a running loop discharges a deferred resume.
            if (syncing) {
                clearRetryCount(this)
                setBootPending(this, false)
            } else {
                bailToRetry()
            }
        }
        // Sticky: this runs in its own :nightwatch process with no Activity, so
        // an LMK kill would otherwise leave background sync dead until the user
        // reopens the app. START_STICKY has the OS recreate the service (onCreate
        // re-runs startNwSync). Sync is idempotent + resumes from nw_file_state.
        return START_STICKY
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        // KEY DIVERGENCE from TransferService: do NOT stopSelf here. Night
        // Watcher must survive the user swiping the app away so scheduled syncs
        // keep firing. Just defer to the default.
        super.onTaskRemoved(rootIntent)
    }

    // A14 (API 34) caps a dataSync FGS at ~6h cumulative per 24h; A15+ enforces
    // it harder. When the budget is spent the OS calls onTimeout, and we MUST
    // stop the foreground state within a few seconds or the app is killed with
    // ForegroundServiceDidNotStopInTimeException. Night Watcher is 24/7, so it
    // WILL hit this daily. Comply, then re-arm: an exact alarm relaunches us once
    // the rolling window frees budget, and a boot-pending flag makes the next app
    // foreground resume us too (belt-and-suspenders if the alarm is refused).
    override fun onTimeout(startId: Int) = handleTimeout()
    override fun onTimeout(startId: Int, fgsType: Int) = handleTimeout()

    private fun handleTimeout() {
        android.util.Log.w("NightWatchService", "dataSync FGS timed out; rescheduling")
        setBootPending(this, true)
        scheduleRestart(this)
        syncing = false
        // Same gate as armRetry: this instance is dying, so a start landing between
        // here and onDestroy must not clear the pending flag on it.
        foregrounded = false
        try {
            stopNwSync()
        } catch (t: Throwable) {
            android.util.Log.w("NightWatchService", "stopNwSync on timeout failed: $t")
        }
        releaseWakelock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        foregrounded = false
        syncing = false
        try {
            stopNwSync()
        } catch (t: Throwable) {
            android.util.Log.w("NightWatchService", "stopNwSync failed: $t")
        }
        releaseWakelock()
        super.onDestroy()
    }

    companion object {
        const val CHANNEL_ID = "cosmog-nightwatch-fg"
        const val FG_NOTIFICATION_ID = 424243

        // Cap the wakelock so it can never be held indefinitely: the OS releases
        // it this long after the LAST acquire/heartbeat, so a dead loop cannot
        // pin the CPU. The Rust loop heartbeats well inside this window.
        private const val WAKELOCK_TIMEOUT_MS = 10L * 60L * 1000L

        // Companion-held like the wakelock below: one instance, dedicated process.
        @Volatile
        private var foregrounded = false

        @Volatile
        private var syncing = false

        // Held in the companion (not per-instance) so the Rust heartbeat can
        // re-acquire it without a live service reference. This process is the
        // dedicated :nightwatch one, so a single static is safe.
        @Volatile
        private var wakeLock: PowerManager.WakeLock? = null

        // Kept so the heartbeat can re-acquire after an idle release; cleared on teardown.
        @Volatile
        private var appCtx: Context? = null

        // (Re)acquire the bounded CPU wakelock. Called from onCreate and, on the
        // heartbeat path, from the Rust sync loop. acquire() on a non-ref-counted
        // lock resets the timeout, so repeated calls just push the cap forward.
        @JvmStatic
        fun acquireWakelock(ctx: Context) {
            appCtx = ctx.applicationContext
            try {
                val pm = ctx.applicationContext.getSystemService(Context.POWER_SERVICE) as PowerManager
                val wl = wakeLock ?: pm
                    .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "cosmog:nightwatch")
                    .also {
                        it.setReferenceCounted(false)
                        wakeLock = it
                    }
                wl.acquire(WAKELOCK_TIMEOUT_MS)
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "acquireWakelock failed: $t")
            }
        }

        // Called by the Rust loop only while a scan/upload is active. No-op once
        // the service is torn down.
        @JvmStatic
        fun heartbeatWakelock() {
            val c = appCtx ?: return
            acquireWakelock(c)
        }

        // Rust loop is idle: let the CPU sleep until the next scan/upload.
        @JvmStatic
        fun idleWakelock() {
            try {
                wakeLock?.takeIf { it.isHeld }?.release()
            } catch (_: Throwable) {}
        }

        @JvmStatic
        fun releaseWakelock() {
            appCtx = null
            try {
                wakeLock?.takeIf { it.isHeld }?.release()
            } catch (_: Throwable) {}
            wakeLock = null
        }

        const val PREFS_NAME = "cosmog_nw"
        const val KEY_ENABLED = "nw_enabled"
        // Set whenever a resume is deferred or a start fails; cleared by
        // onStartCommand once the Rust loop is confirmed up.
        const val KEY_BOOT_PENDING = "nw_boot_pending"
        const val KEY_RETRY_COUNT = "nw_retry_count"

        private fun ensureChannel(ctx: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            if (nm.getNotificationChannel(CHANNEL_ID) != null) return
            val ch = NotificationChannel(
                CHANNEL_ID,
                "Background sync",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Keeps Night Watcher syncing in the background."
                setShowBadge(false)
            }
            nm.createNotificationChannel(ch)
        }

        private fun buildNotification(ctx: Context): Notification {
            val open = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)
            val pi = if (open != null) {
                val flags =
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M)
                        android.app.PendingIntent.FLAG_IMMUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT
                    else android.app.PendingIntent.FLAG_UPDATE_CURRENT
                android.app.PendingIntent.getActivity(ctx, 0, open, flags)
            } else null

            return NotificationCompat.Builder(ctx, CHANNEL_ID)
                .setContentTitle("Night Watcher active")
                .setContentText("Syncing in the background")
                .setSmallIcon(R.drawable.ic_notification)
                .setOngoing(true)
                .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
                .setContentIntent(pi)
                .setPriority(NotificationCompat.PRIORITY_LOW)
                .build()
        }

        // False when the OS refused the start.
        @JvmStatic
        fun start(ctx: Context): Boolean {
            // Android 12+ throws ForegroundServiceStartNotAllowedException when
            // the app is backgrounded. Never let that propagate across JNI.
            return try {
                val intent = Intent(ctx, NightWatchService::class.java)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    ctx.startForegroundService(intent)
                } else {
                    ctx.startService(intent)
                }
                true
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "start refused: $t")
                false
            }
        }

        // Refused start re-arms instead of ending the chain.
        @JvmStatic
        fun startOrRetry(ctx: Context) {
            if (start(ctx)) return
            setBootPending(ctx, true)
            scheduleRestart(ctx, nextRetryDelayMs(ctx))
        }

        @JvmStatic
        fun stop(ctx: Context) {
            cancelRestart(ctx)
            ctx.stopService(Intent(ctx, NightWatchService::class.java))
        }

        // Delay before an FGS-timeout restart. The dataSync budget refills over a
        // rolling 24h window, so an immediate relaunch would just time out again;
        // ~1h back gives usable budget while keeping the duty cycle high.
        private const val RESTART_DELAY_MS = 60L * 60L * 1000L
        private const val RESTART_REQ = 424244

        // Boot hop delay: long enough to be out of the boot broadcast, short
        // enough not to lose a morning of sync.
        const val BOOT_RESTART_DELAY_MS = 15L * 1000L

        // Refused start / failed loop: causes are usually transient, and a failed
        // attempt burns almost no dataSync budget.
        const val FAILED_START_RETRY_MS = 5L * 60L * 1000L
        private const val MAX_RETRY_SHIFT = 4

        // 5m, 10m, 20m, 40m, 80m, then flat, so a permanent cause stops waking the
        // device every 5. On disk, not in a static: startOrRetry runs in the main
        // process, which the OS reclaims between alarms, and the reset runs in
        // :nightwatch, so an in-memory counter would never survive or be seen.
        private fun nextRetryDelayMs(ctx: Context): Long {
            val n = readRaw(ctx, KEY_RETRY_COUNT)?.toIntOrNull() ?: 0
            writeRaw(ctx, KEY_RETRY_COUNT, (n + 1).toString())
            return FAILED_START_RETRY_MS shl minOf(n, MAX_RETRY_SHIFT)
        }

        private fun clearRetryCount(ctx: Context) = writeRaw(ctx, KEY_RETRY_COUNT, "0")

        // Not pre-granted to fresh installs targeting A13+. Without it every restart
        // alarm is inexact, and an inexact alarm carries no FGS-start allowlist.
        @JvmStatic
        fun canScheduleExactAlarms(ctx: Context): Boolean = try {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
                true
            } else {
                val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
                am.canScheduleExactAlarms()
            }
        } catch (t: Throwable) {
            android.util.Log.w("NightWatchService", "canScheduleExactAlarms failed: $t")
            false
        }

        // Opens the system "Alarms & reminders" screen; the grant exists only there.
        // False when nothing handled it (some OEM ROMs), so the UI can say so.
        @JvmStatic
        fun requestExactAlarms(ctx: Context): Boolean {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return true
            return try {
                val i = Intent(android.provider.Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM)
                    .setData(android.net.Uri.fromParts("package", ctx.packageName, null))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                ctx.startActivity(i)
                true
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "requestExactAlarms failed: $t")
                false
            }
        }

        private fun restartPendingIntent(ctx: Context): PendingIntent {
            val i = Intent(ctx, NwRestartReceiver::class.java)
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) PendingIntent.FLAG_IMMUTABLE else 0
            return PendingIntent.getBroadcast(ctx.applicationContext, RESTART_REQ, i, flags)
        }

        // Schedule an exact wake alarm that relaunches the service after the cap
        // window. An exact alarm firing grants the app a temporary allowlist to
        // start an FGS from the background; an inexact one would not, so fall back
        // to that only when exact alarms are unavailable and rely on boot-pending.
        @JvmStatic
        @JvmOverloads
        fun scheduleRestart(ctx: Context, delayMs: Long = RESTART_DELAY_MS) {
            try {
                val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
                val at = SystemClock.elapsedRealtime() + delayMs
                val pi = restartPendingIntent(ctx)
                val exact = canScheduleExactAlarms(ctx)
                if (exact) {
                    am.setExactAndAllowWhileIdle(AlarmManager.ELAPSED_REALTIME_WAKEUP, at, pi)
                } else {
                    am.setAndAllowWhileIdle(AlarmManager.ELAPSED_REALTIME_WAKEUP, at, pi)
                }
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "scheduleRestart failed: $t")
            }
        }

        @JvmStatic
        fun cancelRestart(ctx: Context) {
            try {
                val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
                am.cancel(restartPendingIntent(ctx))
            } catch (_: Throwable) {}
        }

        // Flags are plain files, not SharedPreferences: prefs cache per process,
        // so a write from :nightwatch (FGS timeout) was invisible to the main one.
        private fun flagFile(ctx: Context, key: String) = java.io.File(ctx.filesDir, "$key.flag")

        private fun readRaw(ctx: Context, key: String): String? = try {
            val f = flagFile(ctx, key)
            if (f.exists()) f.readText().trim() else null
        } catch (_: Throwable) {
            null
        }

        private fun writeRaw(ctx: Context, key: String, value: String) {
            try {
                // Temp + rename: the other process must never read a truncated flag.
                val dst = flagFile(ctx, key)
                val tmp = java.io.File(ctx.filesDir, "$key.flag.${android.os.Process.myPid()}.tmp")
                tmp.writeText(value)
                if (!tmp.renameTo(dst)) {
                    tmp.delete()
                    throw java.io.IOException("rename failed")
                }
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "write flag $key failed: $t")
            }
        }

        private fun readFlag(ctx: Context, key: String): Boolean? = readRaw(ctx, key)?.let { it == "1" }

        private fun writeFlag(ctx: Context, key: String, value: Boolean) =
            writeRaw(ctx, key, if (value) "1" else "0")

        // Falls back to the legacy prefs value for installs that predate the flag file.
        @JvmStatic
        fun isEnabled(ctx: Context): Boolean = readFlag(ctx, KEY_ENABLED)
            ?: ctx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).getBoolean(KEY_ENABLED, false)

        // Persist whether Night Watcher should relaunch after boot. Read by
        // BootReceiver on BOOT_COMPLETED.
        @JvmStatic
        fun setBootFlag(ctx: Context, enabled: Boolean) {
            writeFlag(ctx, KEY_ENABLED, enabled)
            if (!enabled) {
                writeFlag(ctx, KEY_BOOT_PENDING, false)
                clearRetryCount(ctx)
                cancelRestart(ctx)
            }
        }

        // Mark that a resume was deferred (boot, FGS timeout, failed start).
        // Consumed by onStartCommand once the loop runs.
        @JvmStatic
        fun setBootPending(ctx: Context, pending: Boolean) {
            writeFlag(ctx, KEY_BOOT_PENDING, pending)
        }

        // Called from a foreground context (MainActivity.onResume) where an FGS
        // start is allowed. If a resume was deferred and Night Watcher is still
        // enabled, start the service now.
        @JvmStatic
        fun resumeIfPending(ctx: Context) {
            if (!isEnabled(ctx)) return
            if (readFlag(ctx, KEY_BOOT_PENDING) != true) return
            // Cleared by the service itself once its loop runs, not here: this start
            // can still be refused, and then the flag is the only thing left.
            startOrRetry(ctx)
        }
    }
}
