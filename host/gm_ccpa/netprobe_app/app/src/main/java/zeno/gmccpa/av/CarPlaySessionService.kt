package zeno.gmccpa.av

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
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
 *
 * **Start/stop go through [FgsGate] (audit12 F094).** A stop that reaches the record before its
 * `startForeground` kills the process, and a session end arrives on a background thread. Each session
 * holder passes its own token ([newToken]); a stop with a start still pending is deferred to that
 * start's [onStartCommand].
 *
 * **Plan v5.9 D6.** A second kind of holder: [LINK_TOKEN], the adapter-link hold the auto-claim
 * (and every successful bring-up) places, so a headless process with the adapter linked is not
 * cached and frozen. And ONE state-driven notification (OD-9E) on the `gmccpa_status` channel:
 * SessionCore's state funnel calls [update]; the text comes from the pure [StatusNotice]; posts are
 * throttled with a trailing post and guarded by FgsGate's `posted` flag ([NoticePoster]).
 */
class CarPlaySessionService : Service() {

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // D6 rev c C3: the old channel (IMPORTANCE could not be changed on it) is deleted BEFORE
        // startForeground — deleting a channel that still carries this service's FGS notification
        // throws SecurityException (NotificationManagerService.java:3699). Wrapped: never blocks the start.
        val mgr = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        runCatching { mgr.deleteNotificationChannel(OLD_CHANNEL_ID) }
        // FIRST after that, unconditionally: this is what the startForegroundService that brought us
        // here obliges. Only after it may the gate consider stopping (see FgsGate).
        val shown = poster.pending
        startForeground(NOTIF_ID, buildNotification(this, shown))
        log.i("foreground session service started (process pinned to foreground priority)")
        // A stop that arrived while this start was pending was deferred to here. stopForeground +
        // stopSelf(startId) run under the gate lock; stopSelf ignores a stale startId, so a newer start
        // survives. A staying start marks the notification posted and re-posts a state that landed
        // between the build above and here (under the lock).
        when (gate.onStarted(onStays = { poster.onStartedStays(shown) }) { stopForeground(STOP_FOREGROUND_REMOVE); stopSelf(startId) }) {
            FgsGate.Started.STAYS -> Unit
            FgsGate.Started.STOPPED ->
                log.i("deferred stop: no session holds the service any more — stopping it now that startForeground ran (startId=$startId)")
            FgsGate.Started.UNREQUESTED_STAYS ->
                log.w("onStartCommand we did not request (startId=$startId) — pending starts clamped at 0; a session holds the service, so it stays")
            FgsGate.Started.UNREQUESTED_STOPPED ->
                log.w("onStartCommand we did not request (startId=$startId) — pending starts clamped at 0; no session holds the service, stopping it")
        }
        // START_STICKY would relaunch us with a null intent after an LMK kill, but a session cannot be
        // resumed without the phone re-dialing anyway, so do NOT auto-restart into an empty session.
        return START_NOT_STICKY
    }

    // Touches no gate state (v5.7.1): onDestroy runs later on main, after a new start/stop pair may
    // already have run on a background thread, so anything cleared here would be cleared too late.
    override fun onDestroy() {
        log.i("foreground session service stopped")
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        /** OD-9E: "GM CCPA status", IMPORTANCE_LOW (no sound, no heads-up), no badge. */
        private const val CHANNEL_ID = "gmccpa_status"
        /** The pre-D6 channel, deleted on start (its importance cannot be changed in place). */
        private const val OLD_CHANNEL_ID = "carplay_session"
        private const val NOTIF_ID = 1001

        private val log = ProbeLog.sub("cpsvc")
        /** Process-wide: the start/stop decision for every session holder. See [FgsGate]. */
        private val gate = FgsGate()
        private val main by lazy { Handler(Looper.getMainLooper()) }
        @Volatile private var appCtx: Context? = null

        /** The update path of the one notification (throttle + orphan guard), see [NoticePoster]. */
        private val poster = NoticePoster(
            gate = gate,
            clock = { SystemClock.elapsedRealtime() },
            schedule = { delayMs, r -> main.postDelayed({ r() }, delayMs) },
            post = { n ->
                val c = appCtx
                if (c != null) runCatching {
                    (c.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(NOTIF_ID, buildNotification(c, n))
                }.onFailure { log.w("status notification update failed: ${it.javaClass.simpleName}: ${it.message}") }
            },
            initial = StatusNotice.of(null, null, false),
        )

        /** plan v5.9 D6 rev b R5: the adapter-link hold. Negative, so no session token can equal it. */
        const val LINK_TOKEN = FgsTokens.LINK_TOKEN

        /** A token unique in this process, one per session holder (a CarPlayActivity instance); > 0. */
        fun newToken(): Long = FgsTokens.newToken()

        /** D6: whether [token] holds the service now. */
        fun holds(token: Long): Boolean = gate.holds(token)

        /**
         * SessionCore's state funnel (OD-9E): the latest notice. Posted (throttled, trailing post) only
         * while the service's notification is up; otherwise kept for the next start to build from.
         */
        fun update(ctx: Context, n: Notice) {
            appCtx = ctx.applicationContext
            poster.update(n)
        }

        /** No PII (StatusNotice); the tap opens the launcher, which restores a live CarPlay screen. */
        private fun buildNotification(ctx: Context, n: Notice): Notification {
            val mgr = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            // Channels are mandatory (API 26+; minSdk is 32) and createNotificationChannel is idempotent.
            mgr.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "GM CCPA status", NotificationManager.IMPORTANCE_LOW).apply { setShowBadge(false) },
            )
            // Code 0, untagged: the launcher as from the icon. The media card's PendingIntent for the
            // same component uses its own code and action ([TapTarget], D-F1).
            val open = PendingIntent.getActivity(ctx, TapTarget.NOTIFICATION_REQUEST,
                Intent(ctx, zeno.gmccpa.MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_IMMUTABLE)
            return Notification.Builder(ctx, CHANNEL_ID)
                .setContentTitle(n.title)
                .setContentText(n.text)
                .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setCategory(Notification.CATEGORY_SERVICE)
                .setShowWhen(false)
                .setContentIntent(open)
                .build()
        }

        /**
         * [token] wants the service up. `startForegroundService` runs under the gate lock and is
         * counted as pending only once it returned; a throw unregisters [token] and propagates, as before.
         */
        fun start(ctx: Context, token: Long) {
            appCtx = ctx.applicationContext
            try {
                gate.start(token) { ctx.startForegroundService(Intent(ctx, CarPlaySessionService::class.java)) }
            } catch (t: Throwable) {
                log.e("startForegroundService failed for session $token: ${t.javaClass.simpleName}: ${t.message}")
                throw t
            }
        }

        /**
         * [token] no longer needs the service. `stopService` is called (under the gate lock) only when
         * no other token holds it AND every start has reached its startForeground; otherwise the stop
         * is deferred to the pending `onStartCommand`. Never stops another holder's service.
         */
        fun stop(ctx: Context, token: Long) {
            when (gate.stop(token) { ctx.stopService(Intent(ctx, CarPlaySessionService::class.java)) }) {
                FgsGate.Stop.STOPPED -> log.i("stopService (session $token; no holder, no pending start)")
                FgsGate.Stop.DEFERRED ->
                    log.i("stop deferred — ${gate.pending()} start(s) not yet in startForeground; onStartCommand will stop it (session $token)")
                FgsGate.Stop.HELD ->
                    log.i("stop skipped — ${gate.holderCount()} other session(s) still hold the service (session $token)")
            }
        }
    }
}
