package com.carlink.callui

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.content.ContextCompat
import com.carlink.MainActivity
import com.carlink.R
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn
import com.carlink.ocbm.Ocbm

/**
 * The CarPlay call as an Android call: a `Notification.CallStyle` card for a ringing or ongoing
 * iPhone call, with Answer / Decline / Hang up routed back to the phone as `INPUT_TELEPHONY` HID
 * taps. This is the pure-3P stand-in for what a native head unit gets from the telecom stack —
 * AAOS renders CallStyle in the notification shade and the HUN, and the actions are ordinary
 * PendingIntents, so nothing here needs a car permission.
 *
 * Ownership and threading: constructed and driven by `CarlinkManager` on the main thread
 * ([onCallState]); the button callback fires on the main thread too (a dynamic receiver on the main
 * looper). The [onButton] sink is expected to be non-blocking (`OcbmClient.sendTelephony` is a queue
 * offer).
 *
 * CallStyle rules that shaped this:
 * - Android 14+ throws unless a CallStyle notification is either a foreground-service notification
 *   or carries a `fullScreenIntent`. We are not an FGS here (the media FGS notification is Media3's),
 *   so a full-screen intent to [MainActivity] is set — which is also the right UX: an incoming call
 *   brings the projection forward. `USE_FULL_SCREEN_INTENT` is declared; where the OS still refuses
 *   (a Play-revoked grant), the throw is caught and a plain notification with the same actions is
 *   posted instead. Nothing is lost but the call-shaped rendering.
 * - The `Person` must have a non-empty name ([CallState.displayLine] guarantees one).
 * - The card must be `ongoing` so a swipe cannot dismiss a live call.
 */
class CallNotificationHost(
    private val context: Context,
    private val onButton: (index: Byte) -> Unit,
) {
    private val nm = context.getSystemService(NotificationManager::class.java)
    private val roster = CallRoster()
    private var shown: CallRoster.Presentation = CallRoster.Presentation.None
    private var registered = false

    private val receiver =
        object : BroadcastReceiver() {
            override fun onReceive(
                ctx: Context,
                intent: Intent,
            ) {
                val idx =
                    when (intent.action) {
                        ACTION_ANSWER -> Ocbm.TEL_ANSWER
                        ACTION_DECLINE, ACTION_HANGUP -> Ocbm.TEL_END
                        else -> return
                    }
                logInfo("[CALL_UI] ${intent.action?.substringAfterLast('.')} -> INPUT_TELEPHONY $idx", tag = TAG)
                onButton(idx)
            }
        }

    init {
        val ch =
            NotificationChannel(CHANNEL_ID, "Phone calls", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "Incoming and ongoing CarPlay calls"
                setShowBadge(false)
                setSound(null, null) // ringing is the phone's job (ALERT lane); see setSilent below
                lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            }
        nm.createNotificationChannel(ch)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE && !nm.canUseFullScreenIntent()) {
            logWarn("[CALL_UI] USE_FULL_SCREEN_INTENT not granted — CallStyle will fall back to a plain card", tag = TAG)
        }
        val filter =
            IntentFilter().apply {
                addAction(ACTION_ANSWER)
                addAction(ACTION_DECLINE)
                addAction(ACTION_HANGUP)
            }
        // Not exported: only our own PendingIntents (sent with our identity) may trigger these.
        ContextCompat.registerReceiver(
            context,
            receiver,
            filter,
            null,
            Handler(Looper.getMainLooper()),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        registered = true
    }

    /** Feed one iOS `callState` record. Main thread. */
    fun onCallState(state: CallState) {
        render(roster.update(state, SystemClock.elapsedRealtime()))
    }

    /** The current reduced view, for anyone arbitrating audio against it. */
    val presentation: CallRoster.Presentation get() = shown

    /** Session over: drop every call and the card. Main thread. */
    fun clear() {
        render(roster.clear())
    }

    fun release() {
        clear()
        if (registered) {
            runCatching { context.unregisterReceiver(receiver) }
            registered = false
        }
    }

    private fun render(p: CallRoster.Presentation) {
        if (p == shown) return
        val previous = shown
        shown = p
        when (p) {
            CallRoster.Presentation.None -> {
                nm.cancel(NOTIFICATION_ID)
                if (previous != CallRoster.Presentation.None) logInfo("[CALL_UI] cleared", tag = TAG)
            }
            is CallRoster.Presentation.Incoming -> post(styled(p), "incoming ${p.call.displayLine}")
            is CallRoster.Presentation.Ongoing -> post(styled(p), "ongoing ${p.call.displayLine}")
        }
    }

    /** The CallStyle form of [p]; [render] guarantees it is never [CallRoster.Presentation.None]. */
    private fun styled(p: CallRoster.Presentation): Notification =
        when (p) {
            is CallRoster.Presentation.Incoming ->
                plainBuilder(p.call, "Incoming call")
                    .setStyle(NotificationCompat.CallStyle.forIncomingCall(person(p.call), pending(ACTION_DECLINE), pending(ACTION_ANSWER)))
                    .setFullScreenIntent(activityIntent(), true)
                    .build()
            is CallRoster.Presentation.Ongoing -> {
                val b =
                    plainBuilder(p.call, "Ongoing call")
                        .setStyle(NotificationCompat.CallStyle.forOngoingCall(person(p.call), pending(ACTION_HANGUP)))
                        .setFullScreenIntent(activityIntent(), true)
                if (p.sinceMs > 0) {
                    // Chronometer runs on wall-clock `when`; convert the elapsed stamp.
                    b.setUsesChronometer(true).setWhen(System.currentTimeMillis() - (SystemClock.elapsedRealtime() - p.sinceMs))
                }
                b.build()
            }
            CallRoster.Presentation.None -> error("nothing to style")
        }

    private fun post(
        n: Notification,
        what: String,
    ) {
        try {
            nm.notify(NOTIFICATION_ID, n)
            logInfo("[CALL_UI] posted $what", tag = TAG)
        } catch (e: IllegalArgumentException) {
            // CallStyle refused (no FGS, full-screen grant revoked). Fall back to a plain card.
            logWarn("[CALL_UI] CallStyle rejected (${e.message}) — plain fallback", tag = TAG)
            val p = shown

            fun action(
                label: String,
                broadcast: String,
            ) = NotificationCompat.Action.Builder(0, label, pending(broadcast)).build()
            val plain =
                when (p) {
                    is CallRoster.Presentation.Incoming ->
                        plainBuilder(p.call, "Incoming call").addAction(action("Answer", ACTION_ANSWER)).addAction(action("Decline", ACTION_DECLINE))
                    is CallRoster.Presentation.Ongoing -> plainBuilder(p.call, "Ongoing call").addAction(action("Hang up", ACTION_HANGUP))
                    CallRoster.Presentation.None -> return
                }
            runCatching { nm.notify(NOTIFICATION_ID, plain.build()) }
                .onFailure { logWarn("[CALL_UI] fallback notify failed: ${it.message}", tag = TAG) }
        }
    }

    private fun plainBuilder(
        call: CallState,
        text: String,
    ): NotificationCompat.Builder =
        NotificationCompat
            .Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_carlink_attribution)
            .setContentTitle(call.displayLine)
            .setContentText(call.label?.let { "$text · $it" } ?: text)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true) // iOS rings through the ALERT audio lane; a second ringtone here would double up.
            .addPerson(person(call))
            .setContentIntent(activityIntent())

    private fun person(call: CallState): Person =
        Person
            .Builder()
            .setName(call.displayLine)
            .setImportant(true)
            .setKey(call.key)
            .build()

    private fun pending(action: String): PendingIntent =
        PendingIntent.getBroadcast(
            context,
            action.hashCode(),
            Intent(action).setPackage(context.packageName),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    private fun activityIntent(): PendingIntent =
        PendingIntent.getActivity(
            context,
            REQ_ACTIVITY,
            Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    companion object {
        private const val TAG = "CALL_UI"
        const val CHANNEL_ID = "carlink_calls"

        /** Distinct from the media FGS slot (1001) so the two never replace each other. */
        const val NOTIFICATION_ID = 1002
        private const val REQ_ACTIVITY = 1002

        const val ACTION_ANSWER = "com.carlink.callui.ANSWER"
        const val ACTION_DECLINE = "com.carlink.callui.DECLINE"
        const val ACTION_HANGUP = "com.carlink.callui.HANGUP"
    }
}
