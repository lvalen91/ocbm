package com.carlink.media

import android.view.KeyEvent
import com.carlink.media.MediaKeyDecoder.Decoded
import org.junit.Assert.assertEquals
import org.junit.Test

class MediaKeyDecoderTest {
    private val d = MediaKeyDecoder()

    private fun down(
        code: Int,
        repeat: Int = 0,
        long: Boolean = false,
    ) = d.decode(code, KeyEvent.ACTION_DOWN, repeat, long)

    private fun up(code: Int) = d.decode(code, KeyEvent.ACTION_UP, 0, false)

    @Test
    fun transportKeysFireOnceOnFirstDown() {
        assertEquals(Decoded.Fire(MediaKeyAction.PLAY_PAUSE), down(KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE))
        assertEquals(Decoded.Ignore, down(KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, repeat = 1))
        assertEquals(Decoded.Ignore, up(KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE))
        assertEquals(Decoded.Fire(MediaKeyAction.NEXT), down(KeyEvent.KEYCODE_MEDIA_NEXT))
        assertEquals(Decoded.Fire(MediaKeyAction.PREVIOUS), down(KeyEvent.KEYCODE_MEDIA_PREVIOUS))
        assertEquals(Decoded.Fire(MediaKeyAction.PLAY), down(KeyEvent.KEYCODE_MEDIA_PLAY))
        assertEquals(Decoded.Fire(MediaKeyAction.PAUSE), down(KeyEvent.KEYCODE_MEDIA_PAUSE))
        assertEquals(Decoded.Fire(MediaKeyAction.PAUSE), down(KeyEvent.KEYCODE_MEDIA_STOP))
    }

    @Test
    fun headsetHookShortPressFiresOnRelease() {
        assertEquals(Decoded.Ignore, down(KeyEvent.KEYCODE_HEADSETHOOK))
        assertEquals(Decoded.Fire(MediaKeyAction.HEADSET_HOOK), up(KeyEvent.KEYCODE_HEADSETHOOK))
    }

    @Test
    fun headsetHookLongPressFiresVoiceAssistOnceAndSwallowsTheRelease() {
        assertEquals(Decoded.Ignore, down(KeyEvent.KEYCODE_HEADSETHOOK))
        assertEquals(Decoded.Fire(MediaKeyAction.VOICE_ASSIST), down(KeyEvent.KEYCODE_HEADSETHOOK, repeat = 1, long = true))
        assertEquals(Decoded.Ignore, down(KeyEvent.KEYCODE_HEADSETHOOK, repeat = 2))
        assertEquals(Decoded.Ignore, up(KeyEvent.KEYCODE_HEADSETHOOK))
        // The state machine is reset: the next short press works again.
        assertEquals(Decoded.Ignore, down(KeyEvent.KEYCODE_HEADSETHOOK))
        assertEquals(Decoded.Fire(MediaKeyAction.HEADSET_HOOK), up(KeyEvent.KEYCODE_HEADSETHOOK))
    }

    @Test
    fun explicitLongPressFlagOnFirstDownAlsoCounts() {
        assertEquals(Decoded.Fire(MediaKeyAction.VOICE_ASSIST), down(KeyEvent.KEYCODE_HEADSETHOOK, repeat = 0, long = true))
        assertEquals(Decoded.Ignore, up(KeyEvent.KEYCODE_HEADSETHOOK))
    }

    @Test
    fun voiceKeysAndUnknownKeys() {
        assertEquals(Decoded.Fire(MediaKeyAction.VOICE_ASSIST), down(KeyEvent.KEYCODE_VOICE_ASSIST))
        assertEquals(Decoded.Unhandled, down(KeyEvent.KEYCODE_VOLUME_UP))
        assertEquals(Decoded.Unhandled, up(KeyEvent.KEYCODE_MEDIA_FAST_FORWARD))
    }
}
