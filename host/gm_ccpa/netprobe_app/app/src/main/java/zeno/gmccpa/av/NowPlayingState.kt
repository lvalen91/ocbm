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
 * Every field is therefore MERGED, never replaced — on an inferred track change too (plan v5.9 MED M1:
 * a same-artist skip carries only the title, and the old blank-base rebuild lost artist, album and
 * art). A field is cleared only when a record carries it empty. The rules are [NowPlayingMerge]'s.
 *
 * ## Wire keys
 * `metadata.rs` `now_playing()`: title, artist, album, genre, composer, appName, durationMs, elapsedMs,
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
         * iAP2 PlaybackAttributes id 0, verbatim: 0 stop, 1 play, 2 pause, 3 seek-forward, 4 seek-back
         * (`metadata.rs` `now_playing()`, group 1 id 0). Null until iOS says. Deliberately never inferred
         * from the audio seam — bytes arriving on `:9002` mean the phone is sending, not that the user
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
     * A `routeGuidance` record (0x5201) arrived; the argument is its `routeGuidanceState`, or -1 when
     * the record did not carry one. Runs on `cp-meta`: the consumer must only stamp (U10, F159).
     */
    @Volatile var onRouteGuidance: ((Int) -> Unit)? = null

    /**
     * Seam entry point. Runs on the `cp-meta` thread: JSON parse inline (cheap), callbacks must post
     * anything expensive elsewhere — see the blocking note on [MetadataSeam].
     */
    fun dispatch(marker: Int, payload: ByteArray) {
        when (marker) {
            MetadataSeam.META_JSON -> {
                val json = runCatching { JSONObject(String(payload, Charsets.UTF_8)) }.getOrNull() ?: return
                // The seam also carries routeGuidance, maneuver, callState, artworkReady and more.
                // Only the media picture is DISPLAYED — by scope decision there is no navigation or
                // telephony surface in this app, and half-handling a kind is worse than ignoring it.
                // One exception (plan v5.7 U10, F159): routeGuidance's state is handed to the voice
                // router as a route-active signal; nothing is shown. The hook only stamps volatiles.
                val kind = json.optString("kind")
                if (kind == "routeGuidance") {
                    val st = if (json.has("routeGuidanceState")) json.optInt("routeGuidanceState", -1) else -1
                    runCatching { onRouteGuidance?.invoke(st) }
                        .onFailure { log.e("routeGuidance consumer threw: ${it.message}") }
                    return
                }
                if (kind != "nowPlaying") return
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

    /**
     * The merge state behind [snapshot]: the displayed picture plus the two art bookkeeping fields
     * ([NowPlayingMerge.State.artCarried], the one-slot pending transfer). Written only under `this`.
     */
    private var merged = NowPlayingMerge.State()

    /**
     * Phone-side playback status hook (plan v5.9 MED M5): called with the new iAP2 playbackStatus each
     * time a record CHANGES it, outside the merge lock, on `cp-meta`. The consumer must only stamp.
     */
    @Volatile var onPlaybackStatus: ((Int?) -> Unit)? = null

    /** Apply one nowPlaying record. Returns the new snapshot if the DISPLAYED picture changed. */
    fun onNowPlaying(json: JSONObject): Snapshot? = onRecord(json.toMap())

    /**
     * The delta merge itself is [NowPlayingMerge.record] (pure; see there for every rule). It works on a
     * Map because android.jar's org.json is a stub at tier-0 runtime.
     */
    fun onRecord(rec: Map<String, Any?>): Snapshot? {
        val prev: Snapshot
        val next: Snapshot
        synchronized(this) {
            prev = snapshot
            merged = NowPlayingMerge.record(merged, rec)
            next = merged.snapshot
            snapshot = next
        }
        if (next.playbackStatus != prev.playbackStatus)
            runCatching { onPlaybackStatus?.invoke(next.playbackStatus) }
                .onFailure { log.e("playbackStatus consumer threw: ${it.message}") }
        return if (next == prev) null else next
    }

    /**
     * Album art arrived on the iAP2 file-transfer session (`metadata.rs`, the "Artwork — iAP2 File
     * Transfer" section), forwarded as `META_ARTWORK`. It can land before OR after the record that
     * references it, and iOS re-runs transfers on reconnect; [NowPlayingMerge.artwork] handles every
     * case (adopt, replace, adopt under a carry, park in the one-slot pending).
     */
    @Synchronized
    fun onArtwork(id: Int, jpeg: ByteArray): Snapshot? {
        val prev = merged
        val next = NowPlayingMerge.artwork(prev, id, jpeg)
        if (next === prev) return null
        merged = next
        if (next.snapshot === prev.snapshot) return null   // parked only: nothing displayed changed
        snapshot = next.snapshot
        return next.snapshot
    }

    /**
     * Session ended. Everything goes (merge base, carry, pending transfer) AND the cleared picture is
     * published. Called from `CarPlayMediaBrowserService.onSessionDown` (plan v5.9 MED M4), i.e. on the
     * SESSION's end, not the screen's: an Activity destroy with the session still up must not blank
     * the merge base the next delta builds on.
     */
    fun clear() {
        // The emit runs OUTSIDE this monitor: the caller holds CMBS's pubLock, and a monitor -> pubLock
        // edge anywhere (emit -> publish) would close a lock cycle with it.
        val s = synchronized(this) { merged = NowPlayingMerge.State(); snapshot = merged.snapshot; snapshot }
        emit(s)
    }

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

// JSONObject -> Map for [NowPlayingMerge]. A JSON null is treated as absent (the key carries over),
// as the old optXOrNull helpers did. Values stay as org.json produced them (String, Int, Long, Double,
// Boolean); the merge reads numbers through Number.
internal fun JSONObject.toMap(): Map<String, Any?> {
    val m = HashMap<String, Any?>()
    val it = keys()
    while (it.hasNext()) {
        val k = it.next()
        val v = opt(k)
        if (v != null && v != JSONObject.NULL) m[k] = v
    }
    return m
}

internal fun ByteArray?.contentEqualsOrBothNull(o: ByteArray?): Boolean =
    if (this == null || o == null) this == null && o == null else contentEquals(o)
