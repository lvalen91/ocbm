package com.carlink.voice

import android.view.KeyEvent
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The activity-side voice-key table. `KeyEvent.KEYCODE_*` are compile-time constants, so this runs
 * on the plain JVM without Robolectric.
 */
class VoiceKeysTest {
    @Test
    fun `the three voice keys fire Siri on their first DOWN only`() {
        for (k in listOf(KeyEvent.KEYCODE_VOICE_ASSIST, KeyEvent.KEYCODE_ASSIST, KeyEvent.KEYCODE_SEARCH)) {
            assertTrue("key $k", VoiceKeys.isSiriKey(k))
            assertTrue("key $k repeat 0", VoiceKeys.triggers(k, repeatCount = 0))
            assertFalse("key $k auto-repeat must not re-send the hold pair", VoiceKeys.triggers(k, repeatCount = 1))
        }
    }

    @Test
    fun `media, call and headset keys are not activity Siri keys`() {
        val others =
            listOf(
                KeyEvent.KEYCODE_HEADSETHOOK,
                KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE,
                KeyEvent.KEYCODE_MEDIA_NEXT,
                KeyEvent.KEYCODE_CALL,
                KeyEvent.KEYCODE_HOME,
                KeyEvent.KEYCODE_DPAD_CENTER,
            )
        for (k in others) {
            assertFalse("key $k", VoiceKeys.isSiriKey(k))
            assertFalse("key $k", VoiceKeys.triggers(k, repeatCount = 0))
        }
    }

    @Test
    fun `the emulator-verified key codes are the AOSP values`() {
        // 84 is the one key the AAOS emulator delivers to the activity; 231 is intercepted by CarInputService.
        assertTrue(VoiceKeys.isSiriKey(84))
        assertTrue(VoiceKeys.isSiriKey(231))
        assertTrue(VoiceKeys.isSiriKey(219))
    }
}
