package com.carlink.projection

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The delta merge, which the producer explicitly warns about:
 *
 * > the updates are DELTAS — merge non-empty fields (an elapsed-only frame must not blank the title)
 * > — `iap2-core/src/metadata.rs`
 *
 * iOS sends one full record per track and then a stream of elapsed-only frames. Publishing each
 * verbatim blanks the AAOS now-playing card about twice a second, which reads as a metadata bug and
 * is really a merge bug. Nothing else in the system catches it — the seam is healthy, the JSON is
 * well-formed, and the card just flickers.
 */
class NowPlayingStateTest {
    private fun j(vararg pairs: Pair<String, Any>) =
        JSONObject().apply {
            put("kind", "nowPlaying")
            pairs.forEach { (k, v) -> put(k, v) }
        }

    @Test
    fun `an elapsed-only delta does not blank the title`() {
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "Sabotage", "artist" to "Beastie Boys", "durationMs" to 178000L))
        assertEquals("Sabotage", s.snapshot.title)

        // The frame that arrives twice a second. org.json's optString would return "" for the
        // absent title; the whole point of optStringOrNull is that this must not overwrite.
        s.onNowPlaying(j("elapsedMs" to 42000L))

        assertEquals("Sabotage", s.snapshot.title)
        assertEquals("Beastie Boys", s.snapshot.artist)
        assertEquals(178000L, s.snapshot.durationMs)
        assertEquals(42000L, s.snapshot.elapsedMs)
    }

    @Test
    fun `an elapsed-only delta reports NO metadata change`() {
        // THE test for this class's stated purpose, and it was missing. The original suite issued an
        // elapsed-only delta and discarded the return value, so a snapshot equality that INCLUDED
        // elapsedMs passed everything — while republishing a full MediaMetadata to every AAOS
        // consumer about twice a second, which is precisely what this class exists to prevent.
        val s = NowPlayingState()
        assertNotNull(s.onNowPlaying(j("title" to "A", "artist" to "B")))

        assertNull("elapsed-only must not report a metadata change", s.onNowPlaying(j("elapsedMs" to 42000L)))
        assertNull(s.onNowPlaying(j("elapsedMs" to 43000L)))

        // ...but the value is still merged, so the caller can publish PlaybackState from it.
        assertEquals(43000L, s.snapshot.elapsedMs)
        assertEquals("A", s.snapshot.title)
    }

    @Test
    fun `a real metadata change still reports, even alongside elapsed`() {
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A"))
        assertNotNull(s.onNowPlaying(j("title" to "B", "elapsedMs" to 1L)))
    }

    @Test
    fun `an unchanged delta reports no change so the caller can skip publishing`() {
        val s = NowPlayingState()
        assertNotNull(s.onNowPlaying(j("title" to "A")))
        // Same content again: republishing a whole MediaMetadata for this would churn every AAOS
        // consumer for nothing.
        assertNull(s.onNowPlaying(j("title" to "A")))
    }

    @Test
    fun `a track change drops the previous artwork immediately`() {
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A", "artworkId" to 7))
        s.onArtwork(7, byteArrayOf(1, 2, 3))
        assertArrayEquals(byteArrayOf(1, 2, 3), s.snapshot.artwork)

        // New track. The new JPEG arrives later over the file-transfer session, so keeping the old
        // one would show the previous album next to the new title for the length of the transfer.
        s.onNowPlaying(j("title" to "B", "artworkId" to 8))
        assertNull(s.snapshot.artwork)
        assertEquals("B", s.snapshot.title)
    }

    @Test
    fun `artwork for a stale transfer id is ignored`() {
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "B", "artworkId" to 8))
        // A late transfer for the track we already moved off. Applying it would put the previous
        // album's art against the current title.
        assertNull(s.onArtwork(7, byteArrayOf(9, 9)))
        assertNull(s.snapshot.artwork)

        assertNotNull(s.onArtwork(8, byteArrayOf(4, 5)))
        assertArrayEquals(byteArrayOf(4, 5), s.snapshot.artwork)
    }

    @Test
    fun `a repeated title is not treated as a track change`() {
        // Repeat-one, or simply a full record resent. Dropping artwork here would make the art
        // disappear and reload on every full frame.
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A", "artworkId" to 1))
        s.onArtwork(1, byteArrayOf(7))
        s.onNowPlaying(j("title" to "A", "elapsedMs" to 5000L))
        assertArrayEquals(byteArrayOf(7), s.snapshot.artwork)
    }

    @Test
    fun `playing comes from the phone's own playbackStatus`() {
        // NOT from the audio socket. `airplayd` holds that connection for the whole session, so it
        // never goes quiet and the card read PLAYING through every pause. iOS sends PlaybackStatus
        // in the nowPlaying record and it is in the `proven` tier, so it arrives by default.
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A"))
        assertEquals(false, s.snapshot.playing)

        assertNotNull(s.onNowPlaying(j("playbackStatus" to NowPlayingState.PLAY)))
        assertEquals(true, s.snapshot.playing)

        // An elapsed-only delta must not disturb it — those arrive twice a second while paused too.
        assertNull(s.onNowPlaying(j("elapsedMs" to 1000L)))
        assertEquals(true, s.snapshot.playing)

        assertNotNull(s.onNowPlaying(j("playbackStatus" to NowPlayingState.PAUSE)))
        assertEquals(false, s.snapshot.playing)

        // Seeking counts as playing for the boolean, but the session maps it to its own transport
        // state so the card does not show a normal play head during a scrub.
        s.onNowPlaying(j("playbackStatus" to NowPlayingState.SEEK_FWD))
        assertEquals(true, s.snapshot.playing)
    }

    @Test
    fun `a track change does not carry stale fields onto the new track`() {
        // iOS sends a FULL record on a track change, so a field the new record omits is genuinely
        // absent for the new track. Merging onto the previous snapshot kept the old artist, album
        // and duration alive next to the new title whenever the new record was partial.
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A", "artist" to "X", "album" to "Y", "durationMs" to 1000L))
        s.onNowPlaying(j("playbackStatus" to NowPlayingState.PLAY))

        s.onNowPlaying(j("title" to "B"))

        assertEquals("B", s.snapshot.title)
        assertNull("stale artist survived a track change", s.snapshot.artist)
        assertNull("stale album survived a track change", s.snapshot.album)
        assertEquals(0L, s.snapshot.durationMs)
        // playbackStatus describes the PLAYER, not the item, so it carries across.
        assertEquals(true, s.snapshot.playing)
    }

    @Test
    fun `a byte-identical artwork resend reports no change`() {
        // iOS re-runs the file transfer on reconnects; reporting one rebuilds a whole MediaMetadata
        // and re-decodes the JPEG for a picture that did not move.
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A", "artworkId" to 3))
        assertNotNull(s.onArtwork(3, byteArrayOf(1, 2)))
        assertNull(s.onArtwork(3, byteArrayOf(1, 2)))
        assertNotNull(s.onArtwork(3, byteArrayOf(1, 3)))
    }

    @Test
    fun `clear removes everything so a card cannot outlive the session`() {
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A", "artist" to "B"))
        s.onArtwork(-1, byteArrayOf(1))
        s.clear()
        assertNull(s.snapshot.title)
        assertNull(s.snapshot.artwork)
        assertEquals(false, s.snapshot.hasContent)
    }

    @Test
    fun `hasContent gates publishing an empty session`() {
        val s = NowPlayingState()
        assertEquals(false, s.snapshot.hasContent)
        // An active MediaSession with an empty title makes AAOS show a blank now-playing card
        // before any phone is connected.
        s.onNowPlaying(j("elapsedMs" to 10L))
        assertEquals(false, s.snapshot.hasContent)
        s.onNowPlaying(j("title" to "A"))
        assertEquals(true, s.snapshot.hasContent)
    }

    @Test
    fun `an empty string field is treated as absent, not as a value`() {
        // The producer omits fields it has no value for, but a defensive read matters: overwriting
        // a good title with "" is exactly the flicker this class exists to prevent.
        val s = NowPlayingState()
        s.onNowPlaying(j("title" to "A"))
        s.onNowPlaying(j("title" to "", "elapsedMs" to 1L))
        assertEquals("A", s.snapshot.title)
    }
}
