package com.sonus.cosmog

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat

/**
 * Foreground service that keeps the Cosmog process alive while transfers are
 * in flight. Android aggressively suspends background apps (Doze, standby,
 * cached-process reap) which was killing multi-minute uploads mid-flight; the
 * service holds a foreground notification and a partial WakeLock so the OS
 * treats the app as user-facing until every transfer settles.
 *
 * Bound only via startForegroundService / stopService from Rust via JNI.
 */
class TransferService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    private var wakeLock: PowerManager.WakeLock? = null

    override fun onCreate() {
        super.onCreate()
        ensureChannel(this)

        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        // No timeout: the foreground-service lifetime already bounds the lock
        // (released in onDestroy). A timeout would silently drop the CPU for
        // transfers longer than it, which defeats the service's purpose.
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "cosmog:transfers").apply {
            setReferenceCounted(false)
            acquire()
        }

        val notif = buildNotification(this)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(FG_NOTIFICATION_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(FG_NOTIFICATION_ID, notif)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // NOT sticky: if the process dies the webview (and every transfer)
        // dies with it. A sticky restart would resurrect only this service,
        // leaving a permanent "Transfers in progress" notification + wakelock
        // with nothing actually running.
        return START_NOT_STICKY
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        // User swiped the app away: transfers are gone, so is our reason to live.
        stopSelf()
        super.onTaskRemoved(rootIntent)
    }

    override fun onDestroy() {
        try {
            wakeLock?.takeIf { it.isHeld }?.release()
        } catch (_: Throwable) {}
        wakeLock = null
        super.onDestroy()
    }

    companion object {
        const val CHANNEL_ID = "cosmog-transfers-fg"
        const val FG_NOTIFICATION_ID = 424242

        private fun ensureChannel(ctx: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            if (nm.getNotificationChannel(CHANNEL_ID) != null) return
            val ch = NotificationChannel(
                CHANNEL_ID,
                "Background transfers",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Keeps uploads and downloads running while the app is in the background."
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
                .setContentTitle("Transfers in progress")
                .setContentText("Uploads and downloads continue in the background")
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
            // the app is backgrounded (webview timers keep firing back there).
            // Never let that propagate across JNI into Rust.
            try {
                val intent = Intent(ctx, TransferService::class.java)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    ctx.startForegroundService(intent)
                } else {
                    ctx.startService(intent)
                }
            } catch (t: Throwable) {
                android.util.Log.w("TransferService", "start refused: $t")
            }
        }

        @JvmStatic
        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, TransferService::class.java))
        }
    }
}
