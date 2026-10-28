package zeno.gmccpa.av

/**
 * Plan v5.9 MED M5 / OD-9C (owner 2026-10-07: "stock-style approach"): pause the PHONE on a permanent
 * media focus loss the way stock does, and hand MainAudio back when CarPlay audio is wanted again.
 *
 * ## Stock (SD = the Silverado GMCarPlay jadx, plan_v5.md "SD =")
 * - The media `AUDIOFOCUS_LOSS` (-1) reaches `ResourceManager.onAudioFocusChanged` from the
 *   CarPlayAudio listener (SD:resmanagement/CarPlayAudioManager.java:713-735) and is turned into a
 *   `changeModes` by `AudioStateMachine` (ResourceManager.java:1289-1301).
 * - In the CONTROLLER state (the phone owns MainAudio, as frames #7 onward of
 *   docs/ops/captures/2026-07-24_carplay_cmd_capture.bin report: permanentEntity 1) it sends a MainAudio TAKE by the
 *   accessory, but only when no CarPlay focus is held at all (`!sm.hasFocus()`, AudioStateMachine.java:
 *   826-833; values :81-112). That is the take [take] sends (receiver `events::send_take_main_audio`).
 * - In the ACCESSORY state a focus GAIN hands MainAudio back to the controller (:702-707) — [giveBack].
 *
 * ## gm_ccpa mapping
 * - [onPermanentLoss]: the player's listener saw `AUDIOFOCUS_LOSS`. Takes only if [shouldTake].
 * - [giveBack]: focus is held again (GRANTED / GAIN), or a MediaSession play / play-pause asked for
 *   CarPlay audio. The receiver sends the Untake only when a take of ours is still outstanding (none
 *   sent, or the phone took MainAudio back itself after a CarPlay-side play = nothing sent).
 * - A CarPlay screen coming back to the front is NOT a give-back: stock keeps the head unit's source
 *   until the driver plays something, and the screen return only arms the focus-reclaim latch.
 *
 * All sends go through [post] ([CarPlayEventTx] in the app), never the caller's thread.
 */
class MainAudioHandover(
    private val port: Port,
    private val post: (Runnable) -> Unit,
    private val log: (String) -> Unit,
) {
    /** The two native commands (NativeCore.takeMainAudio / releaseMainAudio in the app). */
    interface Port {
        fun take(): Boolean
        /** 0 = nothing owed, 1 = Untake sent, -1 = owed but the write failed. */
        fun release(): Int
    }

    /** Returns whether a take was queued. */
    fun onPermanentLoss(current: Boolean, running: Boolean, voiceFocusHeld: Boolean): Boolean {
        if (!shouldTake(current, running, voiceFocusHeld)) {
            log("OD-9C: no MainAudio take (current=$current running=$running voiceFocusHeld=$voiceFocusHeld)")
            return false
        }
        post(Runnable {
            val ok = runCatching { port.take() }.getOrDefault(false)
            log("OD-9C: MainAudio take (stock media-loss changeModes) sent=$ok — the phone should pause")
        })
        return true
    }

    fun giveBack(why: String) {
        post(Runnable {
            when (runCatching { port.release() }.getOrDefault(-1)) {
                1 -> log("OD-9C: MainAudio given back ($why) — Untake sent")
                -1 -> log("OD-9C: MainAudio give-back ($why) owed but NOT sent — no event channel")
                else -> Unit   // nothing owed: the normal case, silent
            }
        })
    }

    companion object {
        /**
         * Take only for the CURRENT, running player, with no CarPlay voice focus held.
         * - current: a replacement screen's player requests AUDIOFOCUS_GAIN while the old one may still
         *   be listening; AAOS hands the OLD one a permanent LOSS. That loss is ours, not the driver's,
         *   and taking MainAudio on it would pause the phone on every screen relaunch.
         * - running: a stopped player's late callback acts on nothing.
         * - voiceFocusHeld: stock's `!sm.hasFocus()` — never take MainAudio under a live CarPlay call,
         *   Siri or alert focus.
         */
        fun shouldTake(current: Boolean, running: Boolean, voiceFocusHeld: Boolean): Boolean =
            current && running && !voiceFocusHeld
    }
}
