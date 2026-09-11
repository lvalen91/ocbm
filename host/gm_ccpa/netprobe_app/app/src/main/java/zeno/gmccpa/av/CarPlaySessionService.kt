package zeno.gmccpa.av

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import zeno.gmccpa.ProbeLog

/**
 * Keeps the process at foreground priority for the life of a CarPlay session.
 *
 * **Why (C3).** The receiver session — OCBM link, USB transport, heartbeat, the Rust control server,
 * the A/V seams and the audio player — lives in this app's process. With no foreground component the
 * moment the user opens another app / a GM dialog / reverse gear backgrounds the task, the process
 * drops to cached and Low-Memory-Killer reclaims it minutes later, taking the live session with it.
 * A started foreground service pins the whole process at foreground priority, so a *backgrounded*
 * (stopped, not destroyed) session survives.
 *
 * **Scope / honest limit.** This raises process priority; it does NOT yet relocate session OWNERSHIP
 * off the Activity. If the Activity is *destroyed* (not merely stopped), its `onDestroy` still tears
 * the seams down. Moving the session objects into this service so they outlive Activity destruction is
 * the larger, hardware-gated slice tracked in docs/11 R2 (T2.2). Start with the priority win, which is
 * safe and covers the common backgrounding case, and add ownership relocation under a flag once it can
 * be verified on the truck.
 */
class CarPlaySessionService : Service() {

    private val log = ProbeLog.sub("cpsvc")

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIF_ID, buildNotification())
        log.i("foreground session service started (process pinned to foreground priority)")
        // START_STICKY would relaunch us with a null intent after an LMK kill, but a session cannot be
        // resumed without the phone re-dialing anyway, so do NOT auto-restart into an empty session.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        log.i("foreground session service stopped")
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun buildNotification(): Notification {
        val mgr = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        // minSdk 26: channels are mandatory and createNotificationChannel is idempotent.
        mgr.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "CarPlay session", NotificationManager.IMPORTANCE_LOW).apply { setShowBadge(false) },
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("CarPlay active")
            .setContentText("Wireless CarPlay session running")
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setOngoing(true)
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "carplay_session"
        private const val NOTIF_ID = 1001

        fun start(ctx: Context) {
            ctx.startForegroundService(Intent(ctx, CarPlaySessionService::class.java))
        }

        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, CarPlaySessionService::class.java))
        }
    }
}
