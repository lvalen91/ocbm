package zeno.gmccpa.assist

import zeno.gmccpa.pair.NativeCore

/**
 * The pure half of the steering-wheel Voice → Siri path: the log wording for what reached the phone
 * and for the session's showFlags. No Android types, so the JVM gate tests it (AssistOutcomeTest).
 */
object AssistOutcome {

    /** One line for a [NativeCore.siriTap] result. "sent" only when BOTH halves of the tap were written. */
    fun describeTap(code: Int): String = when (code) {
        NativeCore.SiriTap.NO_CORE -> "DROPPED — CarPlay not connected (no native core in this process)"
        NativeCore.SiriTap.NOT_STARTED -> "DROPPED — CarPlay not connected (no session past RECORD)"
        NativeCore.SiriTap.PANIC -> "FAILED — native panic"
        NativeCore.SiriTap.BOTH -> "sent requestSiri buttondown(2)+buttonup(3)"
        0 -> "FAILED — event channel refused both buttondown(2) and buttonup(3)"
        1 -> "PARTIAL — buttondown(2) written, buttonup(3) FAILED (Siri may read as held)"
        2 -> "PARTIAL — buttondown(2) FAILED, buttonup(3) written"
        else -> "UNKNOWN result $code"
    }

    /**
     * VoiceInteractionSession.SHOW_* bits by name. Expected from the wheel: 0x23 = PUSH_TO_TALK (0x20,
     * GMCarInputService's flag) | WITH_ASSIST | WITH_SCREENSHOT (0x1|0x2, OR-ed in by
     * VoiceInteractionManagerService.showSessionForActiveService on this firmware).
     */
    fun describeFlags(flags: Int): String {
        val names = listOf(
            0x01 to "WITH_ASSIST", 0x02 to "WITH_SCREENSHOT", 0x04 to "SOURCE_ASSIST_GESTURE",
            0x08 to "SOURCE_APPLICATION", 0x10 to "SOURCE_ACTIVITY", 0x20 to "SOURCE_PUSH_TO_TALK",
            0x40 to "SOURCE_NOTIFICATION", 0x80 to "SOURCE_AUTOMOTIVE_SYSTEM_UI",
        )
        val set = names.filter { flags and it.first != 0 }.map { it.second }
        val rest = flags and names.fold(0) { acc, p -> acc or p.first }.inv()
        val all = if (rest != 0) set + "0x${Integer.toHexString(rest)}" else set
        return "0x${Integer.toHexString(flags)}" + if (all.isEmpty()) "" else " (${all.joinToString("|")})"
    }
}
