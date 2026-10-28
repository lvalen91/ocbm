package zeno.gmccpa.av

/**
 * The now-playing DELTA merge, pure (plan v5.9 MED M1). [NowPlayingState] owns the lock and the
 * callbacks; this object owns only the rules, so they run at tier-0 with no Android on the runtime
 * classpath (android.jar's org.json is a stub there, hence the `Map` input).
 *
 * ## The stream is a delta stream — on a title change too
 * `metadata.rs` `now_playing()` documents the producer as "emit only what this frame carried; the HOST
 * merges". The previous merge rebuilt the record on a BLANK base whenever the title changed, so a
 * same-artist skip (iOS sends only the title) lost artist, album, artworkId and the JPEG
 * (media_audit §1: cp8 20:44:55, card `size=6`). Now every field carries over from the previous
 * state unless the record carries it:
 * - `title: ""` is ignored (a blank title is never shown over a good one);
 * - `artist`/`album`/`genre`/`composer`/`appName` = `""` CLEAR the field (the record carried it, with
 *   no value). Whether iOS clears by `""` or by omission is UNKNOWN (U59-7); with omission a stale
 *   artist persists under an artist-less track — the recorded residual, not guessed away.
 * - durationMs/elapsedMs/track numbers carry (a stale duration is corrected by the next record);
 * - playbackStatus carries, as before (it describes the player, not the item).
 *
 * ## Artwork
 * A JPEG is valid only for the id it arrived under.
 * - A record carrying a NEW `artworkId` drops the JPEG (the art-swap-under-one-title rule).
 * - A title change WITHOUT `artworkId` keeps the previous id and JPEG, marked [State.artCarried]:
 *   the next track may share the art (then nothing else arrives and the art is right), or its own
 *   `artworkId` record / transfer follows. Intended transient: the previous art may show under the
 *   new title until then.
 * - While [State.artCarried], a transfer with a new id is ADOPTED (it can only be the new track's:
 *   the old track's transfer carries the carried id). Adoption ends the carry; a re-send under the
 *   carried id replaces the JPEG and keeps the carry.
 * - Any record carrying `artworkId` ends the carry.
 * - Otherwise a transfer whose id is not the current one goes into the ONE-slot [State.pending]
 *   (rev c, C2), adopted only by a later record with `artworkId == pending.id`. An early new-track
 *   transfer and a late old-track transfer cannot be told apart, so a title change WITHOUT
 *   `artworkId` keeps the carried art and leaves the slot as it is; the slot survives exactly one
 *   title change, and is dropped at the next one that does not adopt it (or at once by a title
 *   change carrying a different `artworkId`), and by `clear()`.
 */
object NowPlayingMerge {

    /** A transfer parked until a record names its id. [survivedTitleChange]: see the class KDoc. */
    class PendingArt(val id: Int, val jpeg: ByteArray, val survivedTitleChange: Boolean = false)

    class State(
        val snapshot: NowPlayingState.Snapshot = NowPlayingState.Snapshot(),
        val artCarried: Boolean = false,
        val pending: PendingArt? = null,
    )

    /** Apply one `kind: nowPlaying` record. Keys absent from [rec] carry over. */
    fun record(prev: State, rec: Map<String, Any?>): State {
        val p = prev.snapshot
        // "" title is ignored: it never counts as carrying a title, so it is never a track change.
        val incomingTitle = str(rec, "title")?.takeIf { it.isNotEmpty() }
        // Track change is inferred from a different non-empty title. NOT from artworkId: that key is
        // absent from elapsed-only frames.
        val titleChange = incomingTitle != null && incomingTitle != p.title

        val rid = int(rec, "artworkId")
        val pend = prev.pending
        var artworkId = p.artworkId
        var artwork = p.artwork
        var carried = prev.artCarried
        var pending = pend
        if (rid != null) {
            carried = false
            when {
                pend != null && pend.id == rid -> { artworkId = rid; artwork = pend.jpeg; pending = null }
                rid == p.artworkId -> Unit   // same id: keep the JPEG
                else -> { artworkId = rid; artwork = null }
            }
            // A title change naming a different id says the parked transfer is not this track's.
            if (titleChange && pending != null) pending = null
        } else if (titleChange) {
            // Keep the previous id and JPEG; mark the carry only if there is something carried.
            carried = p.artworkId >= 0 || p.artwork != null
            if (pending != null) pending = if (pending.survivedTitleChange) null
                else PendingArt(pending.id, pending.jpeg, survivedTitleChange = true)
        }

        val next = NowPlayingState.Snapshot(
            title = incomingTitle ?: p.title,
            artist = clearable(rec, "artist", p.artist),
            album = clearable(rec, "album", p.album),
            genre = clearable(rec, "genre", p.genre),
            composer = clearable(rec, "composer", p.composer),
            appName = clearable(rec, "appName", p.appName),
            durationMs = long(rec, "durationMs") ?: p.durationMs,
            elapsedMs = long(rec, "elapsedMs") ?: p.elapsedMs,
            trackNumber = int(rec, "trackNumber") ?: p.trackNumber,
            trackCount = int(rec, "trackCount") ?: p.trackCount,
            artworkId = artworkId,
            artwork = artwork,
            playbackStatus = int(rec, "playbackStatus") ?: p.playbackStatus,
        )
        return State(next, carried, pending)
    }

    /**
     * Album art arrived (`META_ARTWORK`, id masked to 0..255 by the caller). Returns [prev] itself
     * (identity) when nothing changes, so the caller can tell "no publish" by reference.
     */
    fun artwork(prev: State, id: Int, jpeg: ByteArray): State {
        if (jpeg.isEmpty()) return prev
        val p = prev.snapshot
        // Byte-identical re-send under the current id is not a change (no re-decode).
        if (p.artworkId == id && p.artwork.contentEqualsOrBothNull(jpeg)) return prev
        // -1 (nothing referenced yet) adopts anything.
        if (p.artworkId < 0)
            return State(p.copy(artwork = jpeg, artworkId = id), artCarried = false, pending = prev.pending)
        // The current id replaces its own JPEG. Under a carry that is the OLD track's art re-sent, so
        // the carry stays (the new track's own transfer must still be adoptable).
        if (p.artworkId == id)
            return State(p.copy(artwork = jpeg), prev.artCarried, prev.pending)
        // Under a carry a new id can only be the new track's. Adopting it ends the carry, so a late
        // old-track transfer after this is parked, not adopted.
        if (prev.artCarried)
            return State(p.copy(artwork = jpeg, artworkId = id), artCarried = false, pending = prev.pending)
        // Not the current id and no carry: an early new-track transfer or a late old-track one. Park
        // it (one slot, newest wins); only a record naming its id adopts it.
        return State(p, prev.artCarried, PendingArt(id, jpeg))
    }

    // ---- typed reads of the JSON-derived map ------------------------------------------------------

    private fun str(m: Map<String, Any?>, k: String): String? = m[k] as? String

    /** Absent → [prev]; `""` → cleared (null); otherwise the value. */
    private fun clearable(m: Map<String, Any?>, k: String, prev: String?): String? {
        if (!m.containsKey(k)) return prev
        val v = m[k] as? String ?: return prev   // a non-string (or null) value is not a carried string
        return v.ifEmpty { null }
    }

    private fun long(m: Map<String, Any?>, k: String): Long? = (m[k] as? Number)?.toLong()

    private fun int(m: Map<String, Any?>, k: String): Int? = (m[k] as? Number)?.toInt()
}
