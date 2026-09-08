package zeno.gmccpa.av

import org.json.JSONObject
import zeno.gmccpa.ProbeLog

/**
 * The merged now-playing picture assembled from the `:9004` metadata seam.
 *
 * ## Records are DELTAS — this is the whole reason the class exists
 * `metadata.rs:19` states the prerequisite outright: *the updates are DELTAS — merge non-empty fields
 * (an elapsed-only frame must not blank the title)*. iOS sends a full record on a track change and then
 * roughly 2 Hz frames carrying only `elapsedMs`; device-observed here on 2026-09-08 as 543 twelve-byte
 * `0x5001 NowPlayingUpdate` messages against a single 78-byte one. Publishing each record verbatim
 * would blank the title and artist twice a second — a merge bug that presents as a metadata bug.
 *
 * Every field is therefore MERGED, never replaced, and fields are cleared only on an inferred track
 * change, because iAP2 sends no "new track" marker.
 *
 * ## Wire keys
 * `metadata.rs:333-418`: title, artist, album, genre, composer, appName, durationMs, elapsedMs,
 * trackNumber, trackCount, discNumber, discCount, queueIndex, queueCount, shuffleMode, repeatMode,
 * playbackStatus, playbackSpeed, artworkId. Only the subset an AAOS media card renders is kept.
 */
class NowPlayingState {
    private val log = ProbeLog.sub("np")

    data class Snapshot(
        val title: String? = null,
        val artist: String? = null,
        val album: String? = null,
        val genre: String? = null,
        val composer: String? = null,
        val appName: String? = null,
        val durationMs: Long = 0,
        val elapsedMs: Long = 0,
        val trackNumber: Int = 0,
        val trackCount: Int = 0,
        val artworkId: Int = -1,
        /** Raw JPEG for [artworkId]. Decoding belongs to the consumer, off the seam thread. */
        val artwork: ByteArray? = null,
        /**
         * iAP2 PlaybackAttributes id 0, verbatim: 0 stop, 1 play, 2 pause, 3 seek-forward,
         * 4 seek-back (`metadata.rs:374-378`). Null until iOS says. Deliberately never inferred from
         * the audio seam — bytes arriving on `:9002` mean the phone is sending, not that the user
         * considers it playing.
         */
        val playbackStatus: Int? = null,
    ) {
        val playing: Boolean
            get() = playbackStatus == PLAY || playbackStatus == SEEK_FWD || playbackStatus == SEEK_BACK

        /** Gates publishing: an empty card is worse than the previous one. */
        val hasContent: Boolean get() = !title.isNullOrEmpty() || !artist.isNullOrEmpty()

        /**
         * "Has the DISPLAYED metadata changed."
         *
         * [elapsedMs] is deliberately excluded. With it, every 2 Hz tick compares unequal and the
         * consumer rebuilds a `MediaMetadata` at that rate — which churns all six subscribed
         * controllers for a value that belongs in `PlaybackState` instead. Hand-written because the
         * generated `equals` would compare [artwork] by identity and defeat the check entirely.
         */
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Snapshot) return false
            return title == other.title && artist == other.artist && album == other.album &&
                genre == other.genre && composer == other.composer && appName == other.appName &&
                durationMs == other.durationMs && trackNumber == other.trackNumber &&
                trackCount == other.trackCount && artworkId == other.artworkId &&
                playbackStatus == other.playbackStatus &&
                artwork.contentEqualsOrBothNull(other.artwork)
        }

        override fun hashCode(): Int {   // must agree with equals(): no elapsedMs
            var r = title?.hashCode() ?: 0
            r = 31 * r + (artist?.hashCode() ?: 0)
            r = 31 * r + (album?.hashCode() ?: 0)
            r = 31 * r + durationMs.hashCode()
            r = 31 * r + artworkId
            r = 31 * r + (playbackStatus ?: -1)
            return r
        }
    }

    @Volatile var snapshot: Snapshot = Snapshot(); private set

    /** The displayed picture changed. Expensive path: a full `MediaMetadata` rebuild. */
    @Volatile var onMetadataChanged: ((Snapshot) -> Unit)? = null

    /**
     * A record arrived that the merge judged NOT worth a metadata publish — the ~2 Hz elapsed-only
     * deltas. Split from [onMetadataChanged] because the two costs differ by orders of magnitude: a
     * `PlaybackState` carries position and rate and is what a card interpolates from. Wiring only the
     * expensive half is how a position ends up refreshing once per track.
     */
    @Volatile var onPlaybackTick: ((Snapshot) -> Unit)? = null

    /**
     * Seam entry point. Runs on the `cp-meta` thread: JSON parse inline (cheap), callbacks must post
     * anything expensive elsewhere — see the blocking note on [MetadataSeam].
     */
    fun dispatch(marker: Int, payload: ByteArray) {
        when (marker) {
            MetadataSeam.META_JSON -> {
                val json = runCatching { JSONObject(String(payload, Charsets.UTF_8)) }.getOrNull() ?: return
                // The seam also carries routeGuidance, maneuver, callState, artworkReady and more.
                // Only the media picture is consumed — by scope decision there is no navigation or
                // telephony surface in this app, and half-handling a kind is worse than ignoring it.
                if (json.optString("kind") != "nowPlaying") return
                val changed = onNowPlaying(json)
                if (changed != null) emit(changed)
                else runCatching { onPlaybackTick?.invoke(snapshot) }
                    .onFailure { log.e("tick consumer threw: ${it.message}") }
            }
            MetadataSeam.META_ARTWORK -> {
                if (payload.size < 2) return
                // Mask: Kotlin's Byte is signed, so an id >= 128 would read negative and never match
                // the JSON's int. The same compare cost the macOS host a bug once already.
                val id = payload[0].toInt() and 0xFF
                onArtwork(id, payload.copyOfRange(1, payload.size))?.let { emit(it) }
            }
            else -> Unit   // META_CMD, META_CORNERMASK: not consumed
        }
    }

    /** Apply one nowPlaying record. Returns the new snapshot if the DISPLAYED picture changed. */
    @Synchronized
    fun onNowPlaying(json: JSONObject): Snapshot? {
        val prev = snapshot
        // Track change is inferred from a different non-empty title. NOT from artworkId: that key is
        // absent from elapsed-only frames, so it would read as a change to -1 on every single tick.
        val incomingTitle = json.optStringOrNull("title")
        val trackChanged = incomingTitle != null && incomingTitle != prev.title
        // On a track change, merge onto a BLANK record: a field the full record omits is genuinely
        // absent for the new track. Merging onto the previous snapshot instead kept the old artist,
        // album and duration alive beside the new title. playbackStatus carries across because it
        // describes the PLAYER, not the item.
        val base = if (trackChanged) Snapshot(playbackStatus = prev.playbackStatus) else prev
        val next = Snapshot(
            title = incomingTitle ?: base.title,
            artist = json.optStringOrNull("artist") ?: base.artist,
            album = json.optStringOrNull("album") ?: base.album,
            genre = json.optStringOrNull("genre") ?: base.genre,
            composer = json.optStringOrNull("composer") ?: base.composer,
            appName = json.optStringOrNull("appName") ?: base.appName,
            durationMs = json.optLongOrNull("durationMs") ?: base.durationMs,
            elapsedMs = json.optLongOrNull("elapsedMs") ?: base.elapsedMs,
            trackNumber = json.optIntOrNull("trackNumber") ?: base.trackNumber,
            trackCount = json.optIntOrNull("trackCount") ?: base.trackCount,
            artworkId = json.optIntOrNull("artworkId") ?: base.artworkId,
            // Old art is dropped NOW on a track change even though the new JPEG arrives later over the
            // file transfer: a brief blank cover is honest, the previous album beside a new title is not.
            artwork = base.artwork,
            playbackStatus = json.optIntOrNull("playbackStatus") ?: base.playbackStatus,
        )
        snapshot = next
        return if (next == prev) null else next
    }

    /**
     * Album art arrived on the iAP2 file-transfer session (`metadata.rs:1262-1272`), forwarded as
     * `META_ARTWORK`. It can land before OR after the record that references it, and iOS re-runs
     * transfers on reconnect, so all three cases are handled here rather than assumed away.
     */
    @Synchronized
    fun onArtwork(id: Int, jpeg: ByteArray): Snapshot? {
        val prev = snapshot
        // A mismatched id is a late transfer for a track that has already changed.
        if (prev.artworkId >= 0 && id != prev.artworkId) return null
        if (jpeg.isEmpty()) return null
        // Byte-identical re-send is not a change, and republishing would re-decode the bitmap.
        if (prev.artworkId == id && prev.artwork.contentEqualsOrBothNull(jpeg)) return null
        // ADOPT the id. While it is -1 the guard above accepts anything; recording what we took is
        // exactly what lets the next stale transfer be rejected.
        val next = prev.copy(artwork = jpeg, artworkId = id)
        snapshot = next
        return next
    }

    /**
     * Session ended. Everything goes AND the cleared picture is published — a card still showing the
     * last track over a dead session is the metadata twin of the frozen-frame bug that
     * `CarPlayActivity.onSessionEnded` was written to fix.
     */
    @Synchronized
    fun clear() { snapshot = Snapshot(); emit(snapshot) }

    private fun emit(s: Snapshot) {
        runCatching { onMetadataChanged?.invoke(s) }
            .onFailure { log.e("metadata consumer threw: ${it.message}") }
    }

    companion object {
        const val STOP = 0
        const val PLAY = 1
        const val PAUSE = 2
        const val SEEK_FWD = 3
        const val SEEK_BACK = 4
    }
}

// org.json returns "" from optString and 0 from optLong for a MISSING key — indistinguishable from a
// real value, and exactly wrong for a delta merge. These return null for absent so `?:` falls through
// to the previous value. An empty string counts as absent too: iOS sends "" for a field it has no
// value for, and letting that through would blank a good title.
internal fun JSONObject.optStringOrNull(k: String): String? =
    if (has(k) && !isNull(k)) optString(k).takeIf { it.isNotEmpty() } else null

internal fun JSONObject.optLongOrNull(k: String): Long? = if (has(k) && !isNull(k)) optLong(k) else null

internal fun JSONObject.optIntOrNull(k: String): Int? = if (has(k) && !isNull(k)) optInt(k) else null

internal fun ByteArray?.contentEqualsOrBothNull(o: ByteArray?): Boolean =
    if (this == null || o == null) this == null && o == null else contentEquals(o)
