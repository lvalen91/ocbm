package com.carlink.logging

import android.util.Log

/**
 * Logging for the whole instrument.
 *
 * **One logcat tag — `NETPROBE`** — so a single `adb logcat -s NETPROBE` captures a complete session
 * with nothing missing. Filtering happens on two axes inside that stream:
 *
 *  - **Subsystem**, as a fixed-width bracketed field at the start of the line (`[ocbm]`, `[usb ]`,
 *    `[mfi ]`, …). Grep it: `adb logcat -s NETPROBE | grep '\[ocbm\]'`.
 *  - **Severity**, so problems can be pulled out on their own: `adb logcat -s NETPROBE:W`.
 *
 * Using several *tags* was the obvious alternative and is worse in practice — logcat's `-s` filterspec
 * has no tag wildcard, so every new subsystem would have to be added to every command line, and
 * forgetting one silently drops output.
 *
 * Everything written here is also mirrored to the on-screen report and the exportable text buffer via
 * [mirror]. The mirror is cleared when the Activity goes away, so a background OCBM session that
 * outlives the UI keeps logging to logcat instead of touching a dead view.
 */
object ProbeLog {
    const val TAG = "NETPROBE"

    /** UI/report mirror. Set by MainActivity; null when there is no UI to write to. */
    @Volatile
    var mirror: ((String) -> Unit)? = null

    /** Subsystem names are padded to this width so the messages line up in a dense log. */
    private const val SUB_WIDTH = 5

    class Logger internal constructor(
        private val sub: String?,
        private val enabled: Boolean,
    ) {
        fun i(msg: String) {
            if (enabled) write(Log.INFO, sub, msg)
        }

        fun w(msg: String) {
            if (enabled) write(Log.WARN, sub, msg)
        }

        fun e(msg: String) {
            if (enabled) write(Log.ERROR, sub, msg)
        }
    }

    /** A logger tagged with a subsystem, e.g. `ProbeLog.sub("ocbm")`. */
    fun sub(name: String): Logger = Logger(name, true)

    /** A logger that discards everything — used by the headless self-test so it isn't noisy. */
    fun silent(): Logger = Logger(null, false)

    /** Already-formatted probe output (section headers and the network probes' own lines). */
    fun raw(msg: String) = write(Log.INFO, null, msg)

    /** A run marker, so one session is findable in a long capture. */
    fun banner(msg: String) = write(Log.INFO, null, "===== $msg =====")

    private fun write(
        level: Int,
        sub: String?,
        msg: String,
    ) {
        val line = if (sub == null) msg else "[${sub.padEnd(SUB_WIDTH)}] $msg"
        // Log.println rather than Log.i/w/e so the level is a value, not a call-site choice.
        Log.println(level, TAG, line)
        mirror?.invoke(line)
    }
}
