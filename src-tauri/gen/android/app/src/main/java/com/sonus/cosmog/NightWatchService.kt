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
    private external fun startNwSync()
    private external fun stopNwSync()

    override fun onCreate() {
        super.onCreate()
        ensureChannel(this)

        // Bounded acquire: Night Watcher survives swipe-away and runs 24/7, so
        // an untimed PARTIAL_WAKE_LOCK would pin the CPU forever and drain the
        // battery. The Rust sync loop pings heartbeatWakelock() well within the
        // cap, so a sync spanning many cycles (or one long transfer) keeps the
        // CPU; the OS still auto-releases at the cap if the loop dies without a
        // heartbeat, so a crash that skips onDestroy can never leak it.
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
            android.util.Log.w("NightWatchService", "startForeground refused: $t")
            releaseWakelock()
            stopSelf()
            return
        }

        // Kick the headless Rust sync loop in this process. Guarded so a native
        // failure never crashes the service.
        try {
            startNwSync()
        } catch (t: Throwable) {
            android.util.Log.w("NightWatchService", "startNwSync failed: $t")
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
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

        // Held in the companion (not per-instance) so the Rust heartbeat can
        // re-acquire it without a live service reference. This process is the
        // dedicated :nightwatch one, so a single static is safe.
        @Volatile
        private var wakeLock: PowerManager.WakeLock? = null

        // (Re)acquire the bounded CPU wakelock. Called from onCreate and, on the
        // heartbeat path, from the Rust sync loop. acquire() on a non-ref-counted
        // lock resets the timeout, so repeated calls just push the cap forward.
        @JvmStatic
        fun acquireWakelock(ctx: Context) {
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

        // Heartbeat from the Rust loop (same :nightwatch process): push the cap
        // forward on the already-created lock. No-op if the lock is gone (service
        // torn down); the loop is being canceled in that case anyway.
        @JvmStatic
        fun heartbeatWakelock() {
            try {
                wakeLock?.acquire(WAKELOCK_TIMEOUT_MS)
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "heartbeatWakelock failed: $t")
            }
        }

        @JvmStatic
        fun releaseWakelock() {
            try {
                wakeLock?.takeIf { it.isHeld }?.release()
            } catch (_: Throwable) {}
            wakeLock = null
        }

        const val PREFS_NAME = "cosmog_nw"
        const val KEY_ENABLED = "nw_enabled"
        // Set by BootReceiver on A12+ when it cannot start the dataSync FGS at
        // boot (see BootReceiver). Consumed by resumeIfPending() on next launch.
        const val KEY_BOOT_PENDING = "nw_boot_pending"

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

        @JvmStatic
        fun start(ctx: Context) {
            // Android 12+ throws ForegroundServiceStartNotAllowedException when
            // the app is backgrounded. Never let that propagate across JNI.
            try {
                val intent = Intent(ctx, NightWatchService::class.java)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    ctx.startForegroundService(intent)
                } else {
                    ctx.startService(intent)
                }
            } catch (t: Throwable) {
                android.util.Log.w("NightWatchService", "start refused: $t")
            }
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
        fun scheduleRestart(ctx: Context) {
            try {
                val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
                val at = SystemClock.elapsedRealtime() + RESTART_DELAY_MS
                val pi = restartPendingIntent(ctx)
                val exact = Build.VERSION.SDK_INT < Build.VERSION_CODES.S || am.canScheduleExactAlarms()
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

        // Persist whether Night Watcher should relaunch after boot. Read by
        // BootReceiver on BOOT_COMPLETED / LOCKED_BOOT_COMPLETED.
        @JvmStatic
        fun setBootFlag(ctx: Context, enabled: Boolean) {
            ctx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .edit()
                .putBoolean(KEY_ENABLED, enabled)
                // Clearing the enable flag also cancels any deferred boot resume.
                .apply { if (!enabled) remove(KEY_BOOT_PENDING) }
                .apply()
            // Disabling also drops any pending FGS-timeout restart alarm.
            if (!enabled) cancelRestart(ctx)
        }

        // Mark that a boot-time resume was deferred (A12+ cannot start a
        // dataSync FGS from BOOT_COMPLETED). Consumed by resumeIfPending().
        @JvmStatic
        fun setBootPending(ctx: Context, pending: Boolean) {
            ctx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .edit()
                .putBoolean(KEY_BOOT_PENDING, pending)
                .apply()
        }

        // Called from a foreground context (MainActivity.onResume) where an FGS
        // start is allowed. If a boot resume was deferred and Night Watcher is
        // still enabled, start the service now and clear the pending flag.
        @JvmStatic
        fun resumeIfPending(ctx: Context) {
            val prefs = ctx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            if (!prefs.getBoolean(KEY_ENABLED, false)) return
            if (!prefs.getBoolean(KEY_BOOT_PENDING, false)) return
            start(ctx)
            setBootPending(ctx, false)
        }
    }
}
