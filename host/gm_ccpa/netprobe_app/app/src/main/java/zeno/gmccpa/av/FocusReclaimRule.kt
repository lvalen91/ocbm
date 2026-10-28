package zeno.gmccpa.av

/**
 * When the media player may take audio focus back after losing it (plan v5.9 MED M5). Pure; the
 * AacPlayer lines that consult it belong to VA (v5.8 "Shared files: AacPlayer — VA only").
 *
 * ## The defect
 * After a PERMANENT `AUDIOFOCUS_LOSS` the player pauses and abandons focus, and then `feed()` called
 * `reclaimFocus()` every `FOCUS_RETRY_MS` for as long as media kept arriving. Media kept arriving
 * because nothing paused the phone, so a driver who picks FM got focus taken back within seconds.
 * Since OD-9C the loss also takes MainAudio from the phone the way stock does ([MainAudioHandover]),
 * which should pause it; this rule still stands for a phone that keeps playing anyway (take refused,
 * a voice slot held, or no session channel).
 *
 * ## The rule
 * After a permanent loss, focus is re-requested only on an explicit resume intent:
 * - [userIntent]: a MediaSession play / play-pause, a NEW CarPlay screen (`AacPlayer.start()`), or
 *   the SURVIVING screen brought back to the front (CarPlayActivity.onResume ->
 *   `AacPlayer.onScreenReturned()`). The last is the normal return path on this unit: cp12 (rc1)
 *   shows the screen restored twice with no new seam bind, i.e. no new `start()`;
 * - or the PHONE resuming: iOS playbackStatus was not PLAYING at some point after the loss — already
 *   paused when it landed, or PAUSED (or STOPPED) after it ([sawPausedAfterLoss]) — and is PLAYING
 *   now ([nowPlaying]).
 * Media merely continuing to flow never re-requests. Not after a permanent loss ([lostForGood] false
 * — e.g. a request REFUSED at start, which since C-F1 (2026-10-08) sets the same `pausedForFocus`
 * hold), the existing retry-while-flowing behaviour stands. (Corrected 2026-10-08: a request
 * "orphaned at start" is not an example of this case. The orphan path in `AacPlayer.requestFocus`
 * abandons a GRANTED request for a reason — `stop()` ran, or a permanent LOSS landed first — and
 * the "LOSS landed first" case ran the listener's AUDIOFOCUS_LOSS branch, i.e. [Latch.onPermanentLoss],
 * so [lostForGood] is TRUE there, not false.)
 */
object FocusReclaimRule {

    /**
     * The `AacPlayer.feed` gate (C-F1, 2026-10-08): ask for focus again from the consume thread only
     * while the track is held for focus, no request is cached as held, and the rule above allows it.
     * A refusal at start leaves `pausedForFocus` true with nothing held and [Latch.lostForGood]
     * false, so this is true and the retry runs once per `FOCUS_RETRY_MS` while media flows.
     */
    fun shouldRequestFromFeed(pausedForFocus: Boolean, focusHeld: Boolean, latch: Latch): Boolean =
        pausedForFocus && !focusHeld && latch.shouldReclaim()

    fun shouldReclaim(
        lostForGood: Boolean,
        sawPausedAfterLoss: Boolean,
        nowPlaying: Boolean,
        userIntent: Boolean,
    ): Boolean = when {
        !lostForGood -> true
        userIntent -> true
        else -> sawPausedAfterLoss && nowPlaying
    }

    /**
     * The four inputs as a latch, for the player's threads: the focus listener (main looper), `feed`
     * (the consume thread), the NowPlayingState status hook (cp-meta) and the resume-intent hook
     * (binder). Every method is one short critical section; nothing here calls out.
     */
    class Latch {
        private var lostForGood = false
        private var sawPausedAfterLoss = false
        private var nowPlaying = false
        private var userIntent = false

        /**
         * A permanent AUDIOFOCUS_LOSS landed: arm the rule, forget earlier intents and edges. A phone
         * that is ALREADY not playing counts as paused-after-loss: NowPlayingState forwards status
         * only on a change, so no PAUSE edge would ever follow and its later PLAY could not reclaim.
         */
        @Synchronized
        fun onPermanentLoss() {
            lostForGood = true; sawPausedAfterLoss = !nowPlaying; userIntent = false
        }

        /** Focus is held again (GRANTED, or GAIN): back to the not-lost state. */
        @Synchronized
        fun onFocusHeld() {
            lostForGood = false; sawPausedAfterLoss = false; userIntent = false
        }

        /** iOS playbackStatus changed (NowPlayingState ids; null = unknown). */
        @Synchronized
        fun onPhoneStatus(status: Int?) {
            nowPlaying = status == NowPlayingState.PLAY
            if (lostForGood && (status == NowPlayingState.PAUSE || status == NowPlayingState.STOP))
                sawPausedAfterLoss = true
        }

        /** The user asked for CarPlay audio (play, play-pause, a screen launch or return). */
        @Synchronized
        fun onUserIntent() { userIntent = true }

        @Synchronized
        fun shouldReclaim(): Boolean =
            FocusReclaimRule.shouldReclaim(lostForGood, sawPausedAfterLoss, nowPlaying, userIntent)
    }
}
