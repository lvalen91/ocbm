package com.carlink.callsim

import android.util.Log

/** Single logcat tag for everything: `adb logcat -s CallSim`. */
object L {
    const val TAG = "CallSim"
    fun i(msg: String) { Log.i(TAG, msg) }
    fun w(msg: String, t: Throwable? = null) { Log.w(TAG, msg, t) }
    fun e(msg: String, t: Throwable? = null) { Log.e(TAG, msg, t) }
}
