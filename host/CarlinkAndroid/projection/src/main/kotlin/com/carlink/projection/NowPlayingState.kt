package com.carlink.projection

import org.json.JSONObject

/**
 * The merged now-playing picture, assembled from the iAP2 metadata seam.
 *
 * ## Updates are DELTAS — this is the whole reason this class exists
 *
 * `iap2-core/src/metadata.rs` says it in its own module doc: *"the updates are DELTAS — merge
 * non-empty fields (an elapsed-only frame must not blank the title)"*. iOS sends a full record when
 * the track changes and then a stream of tiny frames carrying only elapsed time. Publishing each
 * frame verbatim to AAOS makes the title and artist flicker to empty roughly twice a second, which
 * looks like a metadata bug and is really a merge bug.
 *
 * So every setter here is *merge, not replace*, and a field is only cleared by an explicit track
 * change — [onNowPlaying] detects that from the artwork id and the title together, because iOS does
 * not send an explicit "new track" marker.
 */
class NowPlayingState {
    data class Snapshot(
        val title: String? = null,
        val artist: String? = null,
        val album: String? = null,
        val genre: String? = null,
        val composer: String? = null,
        val durationMs: Long = 0,
        val elapsedMs: Long = 0,
        val trackNumber: Int = 0,
        val trackCount: Int = 0,
        val artworkId: Int = -1,
        /** JPEG bytes for [artworkId], once the file transfer completes. */
        val artwork: ByteArray? = null,
        /**
         * iAP2 `NowPlayingUpdate` PlaybackAttributes id 0, verbatim:
         * `0 stop · 1 play · 2 pause · 3 seekFwd · 4 seekBack` (`iap2-core/src/metadata.rs`).
         *
         * Null until iOS has told us — never guessed. Subscribed by `start_now_playing()` and part
         * of the `proven` tier, so it arrives in the DEFAULT configuration.
         */
        val playbackStatus: Int? = null,
    ) {
        /**
         * What iOS says its own player is doing.
         *
         * **Not derived from the audio socket.** It used to be, and the comment claimed the audio
         * lane going quiet was "the honest signal" — but that flag is a TCP connect/disconnect and
         * `airplayd` holds the socket for the whole session, so it never went quiet and the card
         * read PLAYING through every pause. It is also not derived from decoded-frame counters,
         * which cannot tell a pause from a stalled decoder.
         *
         * Limitation, stated rather than hidden: this is the PHONE's view. A wedged AacPlayer still
         * reads playing here, so the audio counters in `ProjectionSeamServer.diagnostics()` remain
         * the place to look for "the card says playing and I hear nothing".
         */
        val playing: Boolean
            get() = playbackStatus == PLAY || playbackStatus == SEEK_FWD || playbackStatus == SEEK_BACK

        /** True once there is anything worth showing — used to avoid publishing an empty session. */
        val hasContent: Boolean get() = !title.isNullOrEmpty() || !artist.isNullOrEmpty()

        /**
         * Equality for the purpose of "does the DISPLAYED metadata differ".
         *
         * **[elapsedMs] is deliberately excluded**, and that exclusion is the whole point. iOS sends
         * an elapsed-only delta about twice a second; if elapsed counted here, every one of them
         * would compare unequal, [onNowPlaying] would return non-null, and the caller would rebuild
         * and republish a whole `MediaMetadata` at that rate — exactly the churn this class exists
         * to prevent. Elapsed position belongs in the PlaybackState, which is cheap and is published
         * separately.
         *
         * A hand-written equals is needed anyway: the generated one compares `artwork` by identity,
         * which would make every snapshot unequal and defeat the check entirely.
         */
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Snapshot) return false
            return title == other.title &&
                artist == other.artist &&
                album == other.album &&
                genre == other.genre &&
                composer == other.composer &&
                durationMs == other.durationMs &&
                trackNumber == other.trackNumber &&
                trackCount == other.trackCount &&
                artworkId == other.artworkId &&
                playbackStatus == other.playbackStatus &&
                artwork.contentEqualsOrBothNull(other.artwork)
        }

        override fun hashCode(): Int {
            var r = title?.hashCode() ?: 0
            r = 31 * r + (artist?.hashCode() ?: 0)
            r = 31 * r + (album?.hashCode() ?: 0)
            r = 31 * r + durationMs.hashCode()
            // No elapsedMs — must agree with equals().
            r = 31 * r + artworkId
            r = 31 * r + (playbackStatus ?: -1)
            return r
        }
    }

    @Volatile
    var snapshot: Snapshot = Snapshot()
        private set

    /**
     * Apply one `{"kind":"nowPlaying",...}` record. Returns the new snapshot if anything the UI
     * cares about changed, or null if it did not — so the caller can skip a needless publish.
     *
     * A publish per elapsed-time tick would push a MediaMetadata update to every AAOS media
     * consumer about twice a second for no visible benefit; elapsed time belongs in the
     * PlaybackState, which the caller updates separately and cheaply.
     */
    @Synchronized
    fun onNowPlaying(json: JSONObject): Snapshot? {
        val prev = snapshot

        // A track change is inferred, not announced: iOS sends no "new track" marker. A different
        // non-empty title is the reliable signal; artworkId alone is not, because it is absent from
        // elapsed-only frames and would read as a change to -1 on every tick.
        val incomingTitle = json.optStringOrNull("title")
        val trackChanged = incomingTitle != null && incomingTitle != prev.title

        // On a track change, merge onto a BLANK record rather than onto the previous one.
        //
        // iOS sends a full record when the track changes, so a field the new record omits is
        // genuinely absent for the new track. Merging onto `prev` kept the OLD artist, album,
        // duration and track numbers alive next to the new title whenever the new record was
        // partial — the same bug the artwork clear already fixed, applied to one field out of nine.
        // `playbackStatus` carries across because it describes the PLAYER, not the item.
        val base = if (trackChanged) Snapshot(playbackStatus = prev.playbackStatus) else prev

        val next =
            Snapshot(
                // MERGE: a field absent from this delta keeps its previous value.
                title = incomingTitle ?: base.title,
                artist = json.optStringOrNull("artist") ?: base.artist,
                album = json.optStringOrNull("album") ?: base.album,
                genre = json.optStringOrNull("genre") ?: base.genre,
                composer = json.optStringOrNull("composer") ?: base.composer,
                durationMs = json.optLongOrNull("durationMs") ?: base.durationMs,
                elapsedMs = json.optLongOrNull("elapsedMs") ?: base.elapsedMs,
                trackNumber = json.optIntOrNull("trackNumber") ?: base.trackNumber,
                trackCount = json.optIntOrNull("trackCount") ?: base.trackCount,
                artworkId = json.optIntOrNull("artworkId") ?: base.artworkId,
                // On a track change the OLD artwork must go immediately, even though the new JPEG
                // arrives later over the file-transfer session. Keeping it would show the previous
                // album next to the new title for as long as the transfer takes.
                artwork = base.artwork,
                playbackStatus = json.optIntOrNull("playbackStatus") ?: base.playbackStatus,
            )
        snapshot = next
        return if (next == prev) null else next
    }

    /** Album art arrived on the file-transfer session. Ignored unless it matches the current track. */
    @Synchronized
    fun onArtwork(
        id: Int,
        jpeg: ByteArray,
    ): Snapshot? {
        val prev = snapshot
        // An id we are not expecting is a late transfer for a track that has already changed.
        // Applying it would put the previous album's art against the current title.
        if (prev.artworkId >= 0 && id != prev.artworkId) return null
        if (jpeg.isEmpty()) return null
        // A byte-identical re-send is not a change. iOS re-runs the file transfer on reconnects and
        // some app switches; reporting one rebuilds a whole MediaMetadata and re-decodes the JPEG
        // for a picture that did not move.
        if (prev.artworkId == id && prev.artwork.contentEqualsOrBothNull(jpeg)) return null
        // ADOPT the id. Until the first nowPlaying record carrying artworkId arrives it is -1 and
        // the guard above accepts any blob; recording what we accepted is what lets the NEXT stale
        // transfer be rejected rather than pasted onto the current track.
        val next = prev.copy(artwork = jpeg, artworkId = id)
        snapshot = next
        return next
    }

    /** A session ended: everything goes, so a stale card cannot outlive the phone. */
    @Synchronized
    fun clear() {
        snapshot = Snapshot()
    }

    companion object {
        // iAP2 PlaybackStatus (iap2-core/src/metadata.rs), named so the mapping reads at the call site.
        const val STOP = 0
        const val PLAY = 1
        const val PAUSE = 2
        const val SEEK_FWD = 3
        const val SEEK_BACK = 4
    }
}

// ---- JSON helpers -------------------------------------------------------------------------------
//
// org.json's optString returns "" for a missing key and optLong returns 0, which is exactly wrong
// for a delta merge: both are indistinguishable from a real value and would overwrite a good field
// with a blank one. These return null for absent so `?:` can fall through to the previous value.

internal fun JSONObject.optStringOrNull(key: String): String? = if (has(key) && !isNull(key)) optString(key).takeIf { it.isNotEmpty() } else null

internal fun JSONObject.optLongOrNull(key: String): Long? = if (has(key) && !isNull(key)) optLong(key) else null

internal fun JSONObject.optIntOrNull(key: String): Int? = if (has(key) && !isNull(key)) optInt(key) else null

internal fun ByteArray?.contentEqualsOrBothNull(other: ByteArray?): Boolean =
    when {
        this == null && other == null -> true
        this == null || other == null -> false
        else -> this.contentEquals(other)
    }
