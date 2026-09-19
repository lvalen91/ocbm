package com.carlink.media

import android.view.KeyEvent

/** What a decoded media-button press asks the phone to do. */
enum class MediaKeyAction {
    PLAY,
    PAUSE,

    /** The dedicated HID toggle — sent as such, never split into a play-or-pause guess from mirrored state. */
    PLAY_PAUSE,
    NEXT,
    PREVIOUS,

    /** A SHORT headset-hook press (release without a long-press). Call-aware at the consumer. */
    HEADSET_HOOK,

    /** Headset-hook LONG press, or an explicit voice-assist key: trigger Siri. */
    VOICE_ASSIST,
}

/**
 * Turns the raw `KeyEvent` stream a MediaSession receives into [MediaKeyAction]s.
 *
 * Pure and single-threaded by contract (Media3 delivers media-button intents on the session's
 * application thread). Kept out of the session class so the press/long-press/release state machine
 * can be tested without a session.
 *
 * Rules:
 * - Transport keys fire on the first ACTION_DOWN only; key repeats and releases are consumed silently.
 * - HEADSETHOOK is a hold: DOWN arms it, a long-press (FLAG_LONG_PRESS or a repeat) fires
 *   [MediaKeyAction.VOICE_ASSIST] exactly once, and a release that was NOT long fires
 *   [MediaKeyAction.HEADSET_HOOK]. Android's own `MediaSessionService` uses the same shape for
 *   the voice key, so a head unit that passes the raw key through behaves like a phone would.
 * - Anything else is [Decoded.Unhandled] so the session's default handling can still run.
 */
class MediaKeyDecoder {
    sealed interface Decoded {
        /** Consumed, nothing to do (repeat, release, or the arming half of a hold). */
        data object Ignore : Decoded

        /** Not a key this decoder owns. */
        data object Unhandled : Decoded

        data class Fire(
            val action: MediaKeyAction,
        ) : Decoded
    }

    private var hookDown = false
    private var hookLongFired = false

    fun decode(
        keyCode: Int,
        action: Int,
        repeatCount: Int,
        isLongPress: Boolean,
    ): Decoded {
        if (keyCode == KeyEvent.KEYCODE_HEADSETHOOK) return decodeHook(action, repeatCount, isLongPress)
        val mapped =
            when (keyCode) {
                KeyEvent.KEYCODE_MEDIA_PLAY -> MediaKeyAction.PLAY
                KeyEvent.KEYCODE_MEDIA_PAUSE, KeyEvent.KEYCODE_MEDIA_STOP -> MediaKeyAction.PAUSE
                KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE -> MediaKeyAction.PLAY_PAUSE
                KeyEvent.KEYCODE_MEDIA_NEXT -> MediaKeyAction.NEXT
                KeyEvent.KEYCODE_MEDIA_PREVIOUS -> MediaKeyAction.PREVIOUS
                KeyEvent.KEYCODE_VOICE_ASSIST, KeyEvent.KEYCODE_ASSIST, KeyEvent.KEYCODE_SEARCH -> MediaKeyAction.VOICE_ASSIST
                else -> return Decoded.Unhandled
            }
        return if (action == KeyEvent.ACTION_DOWN && repeatCount == 0) Decoded.Fire(mapped) else Decoded.Ignore
    }

    private fun decodeHook(
        action: Int,
        repeatCount: Int,
        isLongPress: Boolean,
    ): Decoded {
        when (action) {
            KeyEvent.ACTION_DOWN -> {
                if (repeatCount == 0 && !isLongPress) {
                    hookDown = true
                    hookLongFired = false
                    return Decoded.Ignore
                }
                // A repeat or an explicit long-press flag: the hold has crossed the threshold.
                if (hookLongFired) return Decoded.Ignore
                hookDown = true
                hookLongFired = true
                return Decoded.Fire(MediaKeyAction.VOICE_ASSIST)
            }
            KeyEvent.ACTION_UP -> {
                val short = hookDown && !hookLongFired
                hookDown = false
                hookLongFired = false
                return if (short) Decoded.Fire(MediaKeyAction.HEADSET_HOOK) else Decoded.Ignore
            }
            else -> return Decoded.Ignore
        }
    }
}
