package com.carlink.media

/**
 * One now-playing snapshot as published to the MediaSession.
 *
 * Mirrors the field set the macOS host parses from iAP2 `NowPlayingUpdate` (`MetadataWindow.swift`
 * `MediaSnapshot`): the text tuple, the track position within its album, and the app that owns the
 * player. `appName` is what iOS reports for PlaybackAttributes id 7 ("Music", "Spotify", …) — carried
 * here so AAOS surfaces can show the source the way a native CarPlay head unit does.
 *
 * Queue index/count, shuffle/repeat and the disc numbers are parsed box-side too but have no
 * MediaMetadata slot worth filling; they stay in the JSON until a consumer appears.
 */
data class NowPlayingInfo(
    val title: String?,
    val artist: String?,
    val album: String?,
    val appName: String?,
    val genre: String? = null,
    val composer: String? = null,
    /** 1-based; 0 = unknown. */
    val trackNumber: Int = 0,
    val trackCount: Int = 0,
    /** 0 = unknown. */
    val durationMs: Long = 0L,
    /** JPEG bytes, or null when the track's art has not arrived (or the track has none). */
    val albumArt: ByteArray? = null,
) {
    /** Text-only equality — album art is compared by identity elsewhere (hash-cached). */
    fun sameTextAs(o: NowPlayingInfo?): Boolean =
        o != null &&
            title == o.title &&
            artist == o.artist &&
            album == o.album &&
            appName == o.appName &&
            genre == o.genre &&
            composer == o.composer &&
            trackNumber == o.trackNumber &&
            trackCount == o.trackCount &&
            durationMs == o.durationMs

    override fun equals(other: Any?): Boolean = other is NowPlayingInfo && sameTextAs(other) && albumArt === other.albumArt

    override fun hashCode(): Int = (title ?: "").hashCode() * 31 + (artist ?: "").hashCode()
}
