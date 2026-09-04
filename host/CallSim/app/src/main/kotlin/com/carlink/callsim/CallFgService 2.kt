package com.carlink.callsim

import android.app.ForegroundServiceStartNotAllowedException
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Person
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.telecom.Connection

/**
 * Minimal foreground service carrying the CallStyle notification (incoming: Answer/Decline,
 * ongoing: Hang up). Types phoneCall (+ microphone when the platform lets us).
 */
class CallFgService : Service() {
    companion object {
        const val CHANNEL = "callsim_calls"
        const val NOTIF_ID = 1
        @Volatile private var running = false

        fun update(ctx: Context, conn: CallSimConnection) {
            val notif = build(ctx, conn)
            val nm = ctx.getSystemService(NotificationManager::class.java)
            if (running) {
                nm.notify(NOTIF_ID, notif)
                return
            }
            try {
                ctx.startForegroundService(Intent(ctx, CallFgService::class.java))
            } catch (e: ForegroundServiceStartNotAllowedException) {
                L.w("FGS start not allowed (app in background, no exemption): $e — posting the CallStyle notification directly; " +
                    "run `adb shell am start -n com.carlink.callsim/.MainActivity` first if the mic stays silent")
                nm.notify(NOTIF_ID, notif)
            } catch (e: Exception) {
                L.e("FGS start failed", e)
                nm.notify(NOTIF_ID, notif)
            }
        }

        fun stop(ctx: Context) {
            ctx.getSystemService(NotificationManager::class.java).cancel(NOTIF_ID)
            if (running) ctx.stopService(Intent(ctx, CallFgService::class.java))
        }

        private fun pi(ctx: Context, action: String, code: Int): PendingIntent =
            PendingIntent.getBroadcast(
                ctx, code,
                Intent(ctx, AdbReceiver::class.java).setAction(action).putExtra(AdbReceiver.EXTRA_SOURCE, "notification"),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )

        fun build(ctx: Context, conn: CallSimConnection?): Notification {
            val name = conn?.name ?: "CallSim"
            val number = conn?.number ?: ""
            val ringing = conn?.state == Connection.STATE_RINGING
            val person = Person.Builder().setName(name).setImportant(true).build()
            val hangup = pi(ctx, AdbReceiver.ACTION_HANGUP, 1)
            val answer = pi(ctx, AdbReceiver.ACTION_ANSWER, 2)
            val decline = pi(ctx, AdbReceiver.ACTION_REJECT, 3)
            val fullScreen = PendingIntent.getActivity(
                ctx, 4,
                Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_IMMUTABLE,
            )
            val style = if (ringing) Notification.CallStyle.forIncomingCall(person, decline, answer)
            else Notification.CallStyle.forOngoingCall(person, hangup)
            val stateText = conn?.let { Connection.stateToString(it.state) } ?: "idle"
            return Notification.Builder(ctx, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_phone_call)
                .setStyle(style)
                .setContentTitle(if (ringing) "Incoming fake call" else "Fake call $stateText")
                .setContentText("$name  $number")
                .setCategory(Notification.CATEGORY_CALL)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setFullScreenIntent(fullScreen, true)
                .setContentIntent(fullScreen)
                .build()
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val conn = CallRegistry.current
        val notif = build(this, conn)
        try {
            startForeground(NOTIF_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL)
            running = true
            L.i("FGS started type=phoneCall")
        } catch (e: Exception) {
            L.e("startForeground(phoneCall) failed: $e")
            stopSelf()
            return START_NOT_STICKY
        }
        // Upgrade to phoneCall|microphone. Android 14+ throws SecurityException when the app is
        // not allowed while-in-use mic access at this moment; then we rely on Telecom's binding
        // (BIND_INCLUDE_CAPABILITIES) for the recorder instead.
        try {
            startForeground(
                NOTIF_ID, notif,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
            )
            L.i("FGS type upgraded to phoneCall|microphone")
        } catch (e: Exception) {
            L.w("FGS microphone type refused (${e.javaClass.simpleName}: ${e.message}); staying phoneCall only")
        }
        if (conn == null) {
            L.w("FGS started with no live call; stopping")
            stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        L.i("FGS stopped")
        super.onDestroy()
    }
}
