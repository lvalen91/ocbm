package zeno.gmccpa.av

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.9 MED M1 (rev b R29, rev c C2) — the delta merge across a title change, the art carry and
 * the one-slot pending transfer. Records are Maps shaped like `JSONObject.toMap()` output.
 */
class NowPlayingMergeTest {

    private val jA = byteArrayOf(1, 2, 3)
    private val jB = byteArrayOf(4, 5, 6)
    private val jX = byteArrayOf(7, 8, 9)

    private fun rec(vararg kv: Pair<String, Any?>): Map<String, Any?> = mapOf(*kv)

    /** Track A fully described, with its art adopted. */
    private fun playingA(): NowPlayingMerge.State {
        var s = NowPlayingMerge.record(NowPlayingMerge.State(), rec(
            "kind" to "nowPlaying", "title" to "A", "artist" to "Art1", "album" to "Alb1",
            "durationMs" to 200_000, "artworkId" to 10, "playbackStatus" to 1))
        s = NowPlayingMerge.artwork(s, 10, jA)
        assertArrayEquals(jA, s.snapshot.artwork)
        return s
    }

    @Test fun sameArtistTitleChange_keepsArtistAlbumArtworkIdAndArt() {
        val s = NowPlayingMerge.record(playingA(), rec("title" to "B"))
        assertEquals("B", s.snapshot.title)
        assertEquals("Art1", s.snapshot.artist)
        assertEquals("Alb1", s.snapshot.album)
        assertEquals(10, s.snapshot.artworkId)
        assertArrayEquals(jA, s.snapshot.artwork)
        assertEquals(200_000L, s.snapshot.durationMs)   // carried (U59-7: whether iOS resends it)
        assertEquals(1, s.snapshot.playbackStatus)
    }

    @Test fun differentArtist_replaces() {
        val s = NowPlayingMerge.record(playingA(), rec("title" to "B", "artist" to "Art2", "album" to "Alb2"))
        assertEquals("Art2", s.snapshot.artist)
        assertEquals("Alb2", s.snapshot.album)
    }

    @Test fun emptyString_clearsArtistAndAlbum_isIgnoredForTitle() {
        val s = NowPlayingMerge.record(playingA(), rec("title" to "", "artist" to "", "album" to ""))
        assertEquals("A", s.snapshot.title)
        assertNull(s.snapshot.artist)
        assertNull(s.snapshot.album)
        // "" title is not a track change: the art is not marked carried
        assertFalse(s.artCarried)
    }

    @Test fun newArtworkId_dropsTheJpeg_andArtSwapUnderSameTitleWorks() {
        // cp8 20:43:51: iOS swaps the art under the same title.
        var s = NowPlayingMerge.record(playingA(), rec("artworkId" to 11))
        assertEquals("A", s.snapshot.title)
        assertEquals(11, s.snapshot.artworkId)
        assertNull(s.snapshot.artwork)
        s = NowPlayingMerge.artwork(s, 11, jB)
        assertArrayEquals(jB, s.snapshot.artwork)
        // a late transfer for the old id is not shown
        val late = NowPlayingMerge.artwork(s, 10, jA)
        assertArrayEquals(jB, late.snapshot.artwork)
        assertSame(s.snapshot, late.snapshot)
    }

    @Test fun elapsedOnlyTicks_changeNothingDisplayed() {
        val a = playingA()
        val s = NowPlayingMerge.record(a, rec("kind" to "nowPlaying", "elapsedMs" to 5_000))
        assertEquals(a.snapshot, s.snapshot)   // Snapshot.equals excludes elapsedMs by design
        assertEquals(5_000L, s.snapshot.elapsedMs)
        assertEquals("Art1", s.snapshot.artist)
    }

    @Test fun titleChangeWithoutArtworkId_carries_thenNewIdTransferAdopted_thenRecordClearsCarry() {
        var s = NowPlayingMerge.record(playingA(), rec("title" to "B"))
        assertTrue(s.artCarried)
        assertArrayEquals(jA, s.snapshot.artwork)
        s = NowPlayingMerge.artwork(s, 12, jB)
        assertEquals(12, s.snapshot.artworkId)
        assertArrayEquals(jB, s.snapshot.artwork)
        assertFalse("adoption ends the carry", s.artCarried)
        // a late OLD-track transfer after the adoption is parked, never shown
        val late = NowPlayingMerge.artwork(s, 10, jA)
        assertArrayEquals(jB, late.snapshot.artwork)
        // and a record carrying artworkId clears any carry
        var c = NowPlayingMerge.record(playingA(), rec("title" to "C"))
        assertTrue(c.artCarried)
        c = NowPlayingMerge.record(c, rec("artworkId" to 10))
        assertFalse(c.artCarried)
        assertArrayEquals("same id: the art was right", jA, c.snapshot.artwork)
    }

    @Test fun reSendUnderTheCarriedId_keepsTheCarry() {
        var s = NowPlayingMerge.record(playingA(), rec("title" to "B"))
        s = NowPlayingMerge.artwork(s, 10, jX)
        assertTrue(s.artCarried)
        s = NowPlayingMerge.artwork(s, 12, jB)
        assertEquals(12, s.snapshot.artworkId)
        assertArrayEquals(jB, s.snapshot.artwork)
    }

    // ---- rev c, C2: a transfer that arrives BEFORE its title-change record --------------------------

    @Test fun earlyTransfer_thenRecordNamingIt_isAdopted() {
        var s = NowPlayingMerge.artwork(playingA(), 20, jX)
        assertArrayEquals("parked, not shown", jA, s.snapshot.artwork)
        assertEquals(20, s.pending?.id)
        s = NowPlayingMerge.record(s, rec("title" to "B", "artworkId" to 20))
        assertEquals(20, s.snapshot.artworkId)
        assertArrayEquals(jX, s.snapshot.artwork)
        assertNull(s.pending)
        assertFalse(s.artCarried)
    }

    @Test fun earlyTransfer_titleChangeWithoutId_keepsCarriedArt_thenRecordNamingItAdopts() {
        var s = NowPlayingMerge.artwork(playingA(), 20, jX)
        s = NowPlayingMerge.record(s, rec("title" to "B"))
        assertArrayEquals("carried art kept", jA, s.snapshot.artwork)
        assertEquals(10, s.snapshot.artworkId)
        assertEquals("pending kept", 20, s.pending?.id)
        s = NowPlayingMerge.record(s, rec("artworkId" to 20))
        assertEquals(20, s.snapshot.artworkId)
        assertArrayEquals(jX, s.snapshot.artwork)
        assertNull(s.pending)
    }

    @Test fun earlyTransfer_twoTitleChangesWithoutAdoption_pendingClearedNeverAdopted() {
        var s = NowPlayingMerge.artwork(playingA(), 20, jX)
        s = NowPlayingMerge.record(s, rec("title" to "C"))
        assertNotNull(s.pending)
        s = NowPlayingMerge.record(s, rec("title" to "D"))
        assertNull(s.pending)
        s = NowPlayingMerge.record(s, rec("artworkId" to 20))
        assertNull("a cleared slot is never adopted", s.snapshot.artwork)
        assertEquals(20, s.snapshot.artworkId)
    }

    @Test fun titleChangeNamingAnotherId_dropsThePending() {
        var s = NowPlayingMerge.artwork(playingA(), 20, jX)
        s = NowPlayingMerge.record(s, rec("title" to "B", "artworkId" to 21))
        assertNull(s.pending)
        assertNull(s.snapshot.artwork)
        assertEquals(21, s.snapshot.artworkId)
    }

    @Test fun artwork_identicalResend_isNoChange_andMinusOneAdoptsAnything() {
        val a = playingA()
        assertSame(a, NowPlayingMerge.artwork(a, 10, jA.copyOf()))
        assertSame(a, NowPlayingMerge.artwork(a, 10, ByteArray(0)))
        val fresh = NowPlayingMerge.artwork(NowPlayingMerge.State(), 33, jB)
        assertEquals(33, fresh.snapshot.artworkId)
        assertArrayEquals(jB, fresh.snapshot.artwork)
    }

    @Test fun numbersOfAnyBoxedType_areRead() {
        val s = NowPlayingMerge.record(NowPlayingMerge.State(),
            rec("title" to "T", "durationMs" to 7L, "trackNumber" to 3.0, "playbackStatus" to 2))
        assertEquals(7L, s.snapshot.durationMs)
        assertEquals(3, s.snapshot.trackNumber)
        assertEquals(2, s.snapshot.playbackStatus)
    }

    // ---- NowPlayingState wiring (the Map path; no org.json at tier-0) ------------------------------

    @Test fun state_parkedTransferPublishesNothing_clearDropsEverything_statusHookOnEdgesOnly() {
        val nps = NowPlayingState()
        val statuses = mutableListOf<Int?>()
        nps.onPlaybackStatus = { statuses.add(it) }
        assertNotNull(nps.onRecord(rec("title" to "A", "artist" to "Art1", "artworkId" to 10, "playbackStatus" to 1)))
        assertNotNull(nps.onArtwork(10, jA))
        assertNull("elapsed-only tick: no metadata change", nps.onRecord(rec("elapsedMs" to 1000)))
        assertNull("parked transfer: nothing displayed changed", nps.onArtwork(20, jX))
        val b = nps.onRecord(rec("title" to "B"))
        assertEquals("Art1", b?.artist)
        assertArrayEquals(jA, b?.artwork)
        nps.onRecord(rec("playbackStatus" to 2))
        nps.onRecord(rec("playbackStatus" to 2))
        assertEquals(listOf<Int?>(1, 2), statuses)
        var published: NowPlayingState.Snapshot? = null
        nps.onMetadataChanged = { published = it }
        nps.clear()
        assertEquals(NowPlayingState.Snapshot(), nps.snapshot)
        assertEquals(NowPlayingState.Snapshot(), published)
        // after clear() the merge base is blank: a title-only record has no artist, no art, no pending
        val c = nps.onRecord(rec("title" to "C", "artworkId" to 20))
        assertNull(c?.artist)
        assertNull("the pre-clear pending slot is gone", c?.artwork)
    }
}
