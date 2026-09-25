package com.carlink.voice

import android.view.KeyEvent

/**
 * Which hardware keys, arriving at the foreground activity, invoke Siri — the one table
 * `MainActivity.onKeyDown` consults, kept pure so the mapping is unit-tested.
 *
 * Measured on the AAOS 14 emulator (chevy12, 2026-09-25):
 *  - `KEYCODE_SEARCH` (84) DOES reach the activity (`input keyevent 84` → `[KEY] voice key 84 -> Siri sent`
 *    → `UPLINK ON` → `[voice] siri: PCM 16000Hz 1ch`). It is the only voice key the platform lets through.
 *  - `KEYCODE_VOICE_ASSIST` (231) never reaches an activity: `CarInputService` intercepts it (whether
 *    injected through the VHAL, `cmd car_service inject-key 231`, or `input keyevent 231`) and hands it
 *    to the CURRENT digital assistant via `AssistUtils`. It reaches this app only when the user has made
 *    it the assistant — [CarlinkVoiceInteractionService], `onShow` → the same Siri hold pair.
 *  - `KEYCODE_ASSIST` (219) is consumed by the window manager the same way.
 *  - `KEYCODE_HEADSETHOOK` long-press is a MediaSession key (`MediaKeyDecoder`), not an activity key,
 *    and on the emulator never reached this app's session at all.
 *
 * Only the first DOWN of a press fires: auto-repeat while held would send a Siri hold pair per repeat
 * and toggle Siri off again.
 */
object VoiceKeys {
    val SIRI_KEYS: Set<Int> = setOf(KeyEvent.KEYCODE_VOICE_ASSIST, KeyEvent.KEYCODE_ASSIST, KeyEvent.KEYCODE_SEARCH)

    fun isSiriKey(keyCode: Int): Boolean = keyCode in SIRI_KEYS

    /** True for the event that should send the Siri hold pair: a Siri key on its first DOWN. */
    fun triggers(
        keyCode: Int,
        repeatCount: Int,
    ): Boolean = isSiriKey(keyCode) && repeatCount == 0
}
