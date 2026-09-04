package com.carlink.callsim

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioManager
import android.net.Uri
import android.os.Bundle
import android.telecom.CallAudioState
import android.telecom.Connection
import android.telecom.TelecomManager

/**
 * adb entry point; also the target of the notification actions.
 *
 *   adb shell am broadcast -n com.carlink.callsim/.AdbReceiver -a com.carlink.callsim.INCOMING --es name "Test Caller" --es number "+15550100"
 */
class AdbReceiver : BroadcastReceiver() {
    companion object {
        private const val P = "com.carlink.callsim."
        const val ACTION_INCOMING = P + "INCOMING"
        const val ACTION_OUTGOING = P + "OUTGOING"
        const val ACTION_ANSWER = P + "ANSWER"
        const val ACTION_REJECT = P + "REJECT"
        const val ACTION_HANGUP = P + "HANGUP"
        const val ACTION_HOLD = P + "HOLD"
        const val ACTION_UNHOLD = P + "UNHOLD"
        const val ACTION_ROUTE = P + "ROUTE"
        const val ACTION_STATUS = P + "STATUS"
        const val ACTION_NOTIFY = P + "NOTIFY"
        const val EXTRA_SOURCE = "source"
        const val DEFAULT_NUMBER = "+15550100"

        private val RUNTIME_PERMS = listOf(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.POST_NOTIFICATIONS,
            Manifest.permission.BLUETOOTH_CONNECT,
        )

        fun missingPermissions(ctx: Context): List<String> =
            RUNTIME_PERMS.filter { ctx.checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
    }

    override fun onReceive(ctx: Context, intent: Intent) {
        val action = intent.action ?: return
        val source = intent.getStringExtra(EXTRA_SOURCE) ?: "adb"
        L.i("cmd ${action.removePrefix(P)} from $source extras=${intent.extras?.keySet()?.joinToString()}")
        val tm = ctx.getSystemService(TelecomManager::class.java)
        val handle = Accounts.register(ctx)
        val cur = CallRegistry.current
        when (action) {
            ACTION_NOTIFY -> {
                // A messaging notification the way Android Auto's pipeline wants it (NotificationCompat
                // MessagingStyle, reply + mark-as-read semantic actions that show no UI, a sounding
                // channel). Platform-only actions were rejected by gearhead: "semantic reply action,
                // but getShowsUserInterface() is true" -> "No semantic reply action found" (2026-09-04).
                //   adb shell am broadcast -n com.carlink.callsim/.AdbReceiver -a com.carlink.callsim.NOTIFY --es from "Ann" --es text "Running late"
                if (intent.getBooleanExtra("clear", true)) {
                    androidx.core.app.NotificationManagerCompat.from(ctx).cancelAll()   // one record at a time
                }
                val from = intent.getStringExtra("from") ?: "Test Sender"
                val text = intent.getStringExtra("text") ?: "Hello from CallSim"
                val me = androidx.core.app.Person.Builder().setName("Me").build()
                val sender = androidx.core.app.Person.Builder().setName(from).build()
                val style = androidx.core.app.NotificationCompat.MessagingStyle(me)
                    .addMessage(text, System.currentTimeMillis(), sender)
                val noop = android.app.PendingIntent.getBroadcast(
                    ctx, 0, Intent(ctx, AdbReceiver::class.java).setAction(ACTION_STATUS),
                    android.app.PendingIntent.FLAG_MUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT,
                )
                val replyInput = androidx.core.app.RemoteInput.Builder("reply").setLabel("Reply").build()
                val reply = androidx.core.app.NotificationCompat.Action.Builder(android.R.drawable.ic_menu_send, "Reply", noop)
                    .addRemoteInput(replyInput)
                    .setSemanticAction(androidx.core.app.NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
                    .setShowsUserInterface(false)
                    .build()
                val markRead = androidx.core.app.NotificationCompat.Action.Builder(android.R.drawable.ic_menu_view, "Mark as read", noop)
                    .setSemanticAction(androidx.core.app.NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
                    .setShowsUserInterface(false)
                    .build()
                val n = androidx.core.app.NotificationCompat.Builder(ctx, CallSimApp.MSG_CHANNEL)
                    .setSmallIcon(android.R.drawable.stat_notify_chat)
                    .setStyle(style)
                    .setCategory(androidx.core.app.NotificationCompat.CATEGORY_MESSAGE)
                    .setPriority(androidx.core.app.NotificationCompat.PRIORITY_HIGH)
                    .addAction(reply)
                    .addAction(markRead)
                    .setAutoCancel(true)
                    .build()
                androidx.core.app.NotificationManagerCompat.from(ctx)
                    .notify((System.currentTimeMillis() and 0x7fffffff).toInt(), n)
                L.i("NOTIFY posted from=\"$from\" text=\"$text\" (channel ${CallSimApp.MSG_CHANNEL}, sound on, compat actions)")
            }
            ACTION_INCOMING -> {
                warnPermissions(ctx)
                val number = intent.getStringExtra("number") ?: DEFAULT_NUMBER
                val name = intent.getStringExtra("name") ?: "Test Caller"
                if (cur != null) { L.e("INCOMING refused: a call already exists (${Connection.stateToString(cur.state)})"); return }
                val permitted = tm.isIncomingCallPermitted(handle)
                L.i("isIncomingCallPermitted=$permitted")
                val extras = Bundle().apply {
                    putParcelable(TelecomManager.EXTRA_INCOMING_CALL_ADDRESS, Uri.fromParts("tel", number, null))
                    putString(CallSimConnection.EXTRA_NAME, name)
                    intent.getStringExtra("wav")?.let { putString(CallSimConnection.EXTRA_WAV, it) }
                }
                try {
                    tm.addNewIncomingCall(handle, extras)
                    L.i("addNewIncomingCall sent name=\"$name\" number=$number")
                } catch (e: Exception) {
                    L.e("addNewIncomingCall failed", e)
                }
            }
            ACTION_OUTGOING -> {
                warnPermissions(ctx)
                val number = intent.getStringExtra("number") ?: DEFAULT_NUMBER
                if (cur != null) { L.e("OUTGOING refused: a call already exists (${Connection.stateToString(cur.state)})"); return }
                val permitted = tm.isOutgoingCallPermitted(handle)
                L.i("isOutgoingCallPermitted=$permitted")
                val inner = Bundle().apply {
                    intent.getStringExtra("name")?.let { putString(CallSimConnection.EXTRA_NAME, it) }
                    intent.getStringExtra("wav")?.let { putString(CallSimConnection.EXTRA_WAV, it) }
                }
                val extras = Bundle().apply {
                    putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, handle)
                    putBundle(TelecomManager.EXTRA_OUTGOING_CALL_EXTRAS, inner)
                }
                try {
                    tm.placeCall(Uri.fromParts("tel", number, null), extras)
                    L.i("placeCall sent number=$number (auto-answers after ${CallSimConnection.AUTO_ANSWER_MS} ms)")
                } catch (e: Exception) {
                    L.e("placeCall failed", e)
                }
            }
            ACTION_ANSWER -> cur?.answer(source) ?: L.w("ANSWER: no call")
            ACTION_REJECT -> cur?.rejectLocal(source) ?: L.w("REJECT: no call")
            ACTION_HANGUP -> {
                if (cur == null) { L.w("HANGUP: no call"); return }
                if (intent.getStringExtra("cause") == "local" || source == "notification") cur.hangupLocal(source)
                else cur.hangupRemote(source)
            }
            ACTION_HOLD -> cur?.let { L.i("HOLD"); it.setOnHold() } ?: L.w("HOLD: no call")
            ACTION_UNHOLD -> cur?.let { L.i("UNHOLD"); it.setActive() } ?: L.w("UNHOLD: no call")
            ACTION_ROUTE -> {
                if (cur == null) { L.w("ROUTE: no call"); return }
                val route = when (intent.getStringExtra("route")?.lowercase()) {
                    "earpiece" -> CallAudioState.ROUTE_EARPIECE
                    "speaker" -> CallAudioState.ROUTE_SPEAKER
                    "bluetooth", "bt" -> CallAudioState.ROUTE_BLUETOOTH
                    "wired" -> CallAudioState.ROUTE_WIRED_HEADSET
                    else -> { L.w("ROUTE: --es route earpiece|speaker|bluetooth|wired"); return }
                }
                L.i("setAudioRoute(${CallAudioState.audioRouteToString(route)}) requested")
                @Suppress("DEPRECATION")
                cur.setAudioRoute(route)
            }
            ACTION_STATUS -> status(ctx, tm)
        }
    }

    private fun warnPermissions(ctx: Context) {
        val missing = missingPermissions(ctx)
        if (missing.isNotEmpty()) {
            L.w("missing runtime permissions: ${missing.joinToString()} — grant with: " +
                missing.joinToString("; ") { "adb shell pm grant com.carlink.callsim $it" })
        }
    }

    private fun status(ctx: Context, tm: TelecomManager) {
        val am = ctx.getSystemService(AudioManager::class.java)
        val acct = tm.getPhoneAccount(Accounts.handle(ctx))
        val cur = CallRegistry.current
        L.i("STATUS account=${if (acct == null) "NOT REGISTERED" else "registered selfManaged=${acct.hasCapabilities(android.telecom.PhoneAccount.CAPABILITY_SELF_MANAGED)}"} " +
            "call=${cur?.let { "${Connection.stateToString(it.state)} ${it.name} ${it.number}" } ?: "none"} " +
            "tm.isInCall=${tm.isInCall} audioMode=${am.mode} " +
            "commDevice=${AudioUtil.describe(am.communicationDevice)} " +
            "availableComm=${am.availableCommunicationDevices.joinToString { AudioUtil.typeName(it.type) }} " +
            "missingPerms=${missingPermissions(ctx)}")
    }
}
