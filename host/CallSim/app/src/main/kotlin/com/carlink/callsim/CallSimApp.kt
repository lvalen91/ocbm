package com.carlink.callsim

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager

class CallSimApp : Application() {
    companion object { const val MSG_CHANNEL = "callsim_messages" }

    override fun onCreate() {
        super.onCreate()
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CallFgService.CHANNEL, "CallSim calls", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "Fake-call notifications (CallSim rings by itself, channel is silent)"
                setSound(null, null)
                enableVibration(false)
            },
        )
        // Messaging-style test notifications WITH a sound: the shape Android Auto is designed to route
        // to the car's SYSTEM audio sink (a shell-posted notification has no sound and cannot).
        nm.createNotificationChannel(
            NotificationChannel(MSG_CHANNEL, "CallSim messages", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "Fake incoming messages (NOTIFY action) — sounds on purpose"
                setSound(
                    android.provider.Settings.System.DEFAULT_NOTIFICATION_URI,
                    android.media.AudioAttributes.Builder()
                        .setUsage(android.media.AudioAttributes.USAGE_NOTIFICATION_COMMUNICATION_INSTANT)
                        .setContentType(android.media.AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build(),
                )
                enableVibration(true)
            },
        )
        // Idempotent; self-managed accounts need no user enablement.
        Accounts.register(this)
        L.i("CallSimApp started pid=${android.os.Process.myPid()}")
    }
}
