package com.carlink.callsim

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.telecom.Connection
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/** Tiny on-phone UI: status line + the same commands adb sends. Also requests permissions. */
class MainActivity : Activity() {
    private lateinit var status: TextView
    private val handler = Handler(Looper.getMainLooper())
    private val tick = object : Runnable {
        override fun run() { refresh(); handler.postDelayed(this, 1000) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(32, 32, 32, 32) }
        status = TextView(this).apply { textSize = 14f }
        col.addView(status)
        fun btn(label: String, action: String, vararg extras: Pair<String, String>) {
            col.addView(Button(this).apply {
                text = label
                setOnClickListener {
                    val i = Intent(this@MainActivity, AdbReceiver::class.java).setAction(action).putExtra(AdbReceiver.EXTRA_SOURCE, "ui")
                    extras.forEach { (k, v) -> i.putExtra(k, v) }
                    sendBroadcast(i)
                }
            })
        }
        btn("Incoming call", AdbReceiver.ACTION_INCOMING, "name" to "Test Caller")
        btn("Outgoing call", AdbReceiver.ACTION_OUTGOING)
        btn("Answer", AdbReceiver.ACTION_ANSWER)
        btn("Hang up (far end)", AdbReceiver.ACTION_HANGUP)
        btn("Route: speaker", AdbReceiver.ACTION_ROUTE, "route" to "speaker")
        btn("Route: bluetooth", AdbReceiver.ACTION_ROUTE, "route" to "bluetooth")
        btn("Route: earpiece", AdbReceiver.ACTION_ROUTE, "route" to "earpiece")
        btn("Log status", AdbReceiver.ACTION_STATUS)
        setContentView(ScrollView(this).apply { addView(col) })

        val missing = AdbReceiver.missingPermissions(this)
        if (missing.isNotEmpty()) requestPermissions(missing.toTypedArray(), 1)
    }

    override fun onResume() { super.onResume(); handler.post(tick) }
    override fun onPause() { super.onPause(); handler.removeCallbacks(tick) }

    private fun refresh() {
        val c = CallRegistry.current
        status.text = buildString {
            append("CallSim  pkg=").append(packageName).append('\n')
            append("call: ").append(c?.let { "${Connection.stateToString(it.state)}  ${it.name}  ${it.number}" } ?: "none").append('\n')
            append("missing perms: ").append(AdbReceiver.missingPermissions(this@MainActivity).ifEmpty { listOf("none") }.joinToString()).append('\n')
            append("logcat: adb logcat -s CallSim")
        }
    }
}
