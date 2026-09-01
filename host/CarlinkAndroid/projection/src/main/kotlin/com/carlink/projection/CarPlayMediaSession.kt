package com.carlink.projection

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.SystemClock
import android.view.KeyEvent
import com.carlink.logging.ProbeLog
import com.carlink.ocbm.Ocbm

/**
 * Publishes CarPlay's now-playing state to AAOS as a `MediaSession`, and routes AAOS media buttons
 * back to the phone.
 *
 * This is what makes CarPlay a **media source the head unit understands** rather than an opaque
 * video window: with a session published, the AAOS now-playing card, the steering-wheel transport
 * keys and anything else watching `MediaSessionManager` all see the track the phone is playing.
 *
 * ## The platform MediaSession, deliberately, not Media3 or a MediaBrowserService
 *
 * `:app` uses Media3 because it is a media *app*. This is not one: the phone owns the content, and
 * there is nothing to browse — a `MediaBrowserService` would advertise a library we cannot enumerate
 * and would classify projection as a media-template app, which §1 explicitly decided against. A bare
 * `MediaSession` publishes exactly what is true (there is a track, here is its metadata, here are
 * the transport controls) and nothing that is not.
 *
 * ## Transport controls go to the phone, not to a local player
 *
 * Every callback here forwards a HID media-button report through [InputSender] and changes no local
 * state. iOS owns playback; guessing locally would desynchronise the card from the phone within one
 * press. The state shown updates when the phone tells us it changed, which is the same order of
 * events a real head unit sees.
 */
class CarPlayMediaSession(
    private val ctx: Context,
    private val input: InputSender,
) {
    private val log = ProbeLog.sub("media")

    @Volatile private var session: MediaSession? = null

    /** Decoded once per artwork blob rather than per publish — a 1080p-era JPEG decode is not free. */
    @Volatile private var artBitmap: Bitmap? = null

    @Volatile private var artFor: ByteArray? = null

    /** One-shot, so a missing playbackStatus is reported once rather than twice a second. */
    private var warnedNoPlaybackStatus = false

    fun start() {
        if (session != null) return
        val s =
            MediaSession(ctx, "CarPlay").apply {
                setCallback(callback)
                // FLAG_HANDLES_* are how the platform knows to route transport keys and
                // media-button intents here at all. Without them the session shows metadata but the
                // steering-wheel keys go nowhere.
                @Suppress("DEPRECATION")
                setFlags(MediaSession.FLAG_HANDLES_MEDIA_BUTTONS or MediaSession.FLAG_HANDLES_TRANSPORT_CONTROLS)
            }
        session = s
        log.i("MediaSession created")
    }

    fun stop() {
        session?.let {
            runCatching { it.isActive = false }
            runCatching { it.release() }
        }
        session = null
        artBitmap = null
        artFor = null
        log.i("MediaSession released")
    }

    /**
     * Publish a snapshot.
     *
     * The session is only made active once there is real content. An active session with an empty
     * title makes AAOS show a blank now-playing card the moment the app starts, before any phone is
     * even connected.
     */
    fun publish(s: NowPlayingState.Snapshot) {
        val sess = session ?: return
        if (!s.hasContent) {
            if (sess.isActive) {
                sess.isActive = false
                log.i("no content — session deactivated")
            }
            return
        }

        val b =
            MediaMetadata
                .Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, s.title.orEmpty())
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE, s.title.orEmpty())
                .putString(MediaMetadata.METADATA_KEY_ARTIST, s.artist.orEmpty())
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, s.artist.orEmpty())
                .putString(MediaMetadata.METADATA_KEY_ALBUM, s.album.orEmpty())
        s.genre?.let { b.putString(MediaMetadata.METADATA_KEY_GENRE, it) }
        s.composer?.let { b.putString(MediaMetadata.METADATA_KEY_COMPOSER, it) }
        if (s.durationMs > 0) b.putLong(MediaMetadata.METADATA_KEY_DURATION, s.durationMs)
        if (s.trackNumber > 0) b.putLong(MediaMetadata.METADATA_KEY_TRACK_NUMBER, s.trackNumber.toLong())
        if (s.trackCount > 0) b.putLong(MediaMetadata.METADATA_KEY_NUM_TRACKS, s.trackCount.toLong())

        bitmapFor(s.artwork)?.let {
            // ALBUM_ART and ART both: AAOS consumers are inconsistent about which they read, and a
            // card with no image because it looked at the other key is a common integration bug.
            b.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, it)
            b.putBitmap(MediaMetadata.METADATA_KEY_ART, it)
        }

        runCatching { sess.setMetadata(b.build()) }
            .onFailure { log.e("setMetadata failed: $it") }

        publishPlaybackState(s)

        if (!sess.isActive) {
            sess.isActive = true
            log.i("session ACTIVE — \"${s.title}\" / ${s.artist}")
        }
    }

    /**
     * Playback position and available actions.
     *
     * Separated from [publish] because elapsed time changes about twice a second while the metadata
     * does not: pushing a whole `MediaMetadata` at that rate would churn every consumer for nothing.
     */
    fun publishPlaybackState(s: NowPlayingState.Snapshot) {
        val sess = session ?: return
        // iAP2 PlaybackStatus maps onto PlaybackState 1:1, so map it rather than collapsing seek
        // into "playing" — a card showing a normal play head during a scrub is a visible lie, and
        // the platform already has states for it.
        val (state, rate) =
            when (s.playbackStatus) {
                NowPlayingState.PLAY -> PlaybackState.STATE_PLAYING to 1.0f
                NowPlayingState.SEEK_FWD -> PlaybackState.STATE_FAST_FORWARDING to 2.0f
                NowPlayingState.SEEK_BACK -> PlaybackState.STATE_REWINDING to -2.0f
                NowPlayingState.PAUSE -> PlaybackState.STATE_PAUSED to 0.0f
                NowPlayingState.STOP -> PlaybackState.STATE_STOPPED to 0.0f
                // Never told. Say so once: if the pushed metadata tier ever stops declaring
                // NowPlayingUpdate's PlaybackAttributes, the card silently freezes at "paused" and
                // nothing anywhere reports why.
                else -> {
                    if (s.hasContent && !warnedNoPlaybackStatus) {
                        warnedNoPlaybackStatus = true
                        log.w(
                            "publishing a track with no iAP2 playbackStatus — check the pushed " +
                                "metadata tier still declares NowPlayingUpdate PlaybackAttributes.",
                        )
                    }
                    PlaybackState.STATE_PAUSED to 0.0f
                }
            }
        val ps =
            PlaybackState
                .Builder()
                .setActions(
                    PlaybackState.ACTION_PLAY or
                        PlaybackState.ACTION_PAUSE or
                        PlaybackState.ACTION_PLAY_PAUSE or
                        PlaybackState.ACTION_SKIP_TO_NEXT or
                        PlaybackState.ACTION_SKIP_TO_PREVIOUS,
                )
                // Rate 1.0 while playing lets consumers interpolate the position between updates
                // instead of showing a counter that jumps twice a second.
                .setState(state, s.elapsedMs, rate, SystemClock.elapsedRealtime())
                .build()
        runCatching { sess.setPlaybackState(ps) }
            .onFailure { log.e("setPlaybackState failed: $it") }
    }

    private fun bitmapFor(jpeg: ByteArray?): Bitmap? {
        if (jpeg == null) {
            artBitmap = null
            artFor = null
            return null
        }
        if (artFor === jpeg && artBitmap != null) return artBitmap
        // Subsampled, not full size. iOS ships artwork up to ~1400 px square; a full decode is
        // several MB and tens of milliseconds on the METADATA SEAM THREAD, whose own contract says
        // nothing slow may run there. Every AAOS consumer scales it to a card anyway.
        val bm = runCatching { decodeBounded(jpeg) }.getOrNull()
        if (bm == null) {
            log.w("artwork (${jpeg.size} B) failed to decode — publishing without an image")
        }
        artBitmap = bm
        artFor = jpeg
        return bm
    }

    private fun decodeBounded(jpeg: ByteArray): Bitmap? {
        val probe = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size, probe)
        var sample = 1
        while (probe.outWidth / sample > ART_MAX_PX || probe.outHeight / sample > ART_MAX_PX) {
            sample *= 2
        }
        return BitmapFactory.decodeByteArray(
            jpeg,
            0,
            jpeg.size,
            BitmapFactory.Options().apply { inSampleSize = sample },
        )
    }

    // ---- transport controls -> the phone ---------------------------------------------------------

    private val callback =
        object : MediaSession.Callback() {
            override fun onPlay() = send(Ocbm.MEDIA_BTN_PLAY, "play")

            override fun onPause() = send(Ocbm.MEDIA_BTN_PAUSE, "pause")

            override fun onSkipToNext() = send(Ocbm.MEDIA_BTN_NEXT, "next")

            override fun onSkipToPrevious() = send(Ocbm.MEDIA_BTN_PREV, "prev")

            override fun onStop() = send(Ocbm.MEDIA_BTN_PAUSE, "stop->pause")

            /**
             * Raw media-button KeyEvents, which is how a steering wheel usually arrives when the key
             * is not one of the two `CarProjectionManager` gives us.
             *
             * Handled explicitly rather than left to the platform's default mapping so that
             * PLAY_PAUSE reaches iOS as the dedicated toggle rather than being split into a
             * play-or-pause guess based on state we do not own.
             */
            override fun onMediaButtonEvent(intent: android.content.Intent): Boolean {
                val ev =
                    @Suppress("DEPRECATION")
                    intent.getParcelableExtra<KeyEvent>(android.content.Intent.EXTRA_KEY_EVENT)
                        ?: return super.onMediaButtonEvent(intent)
                if (ev.action != KeyEvent.ACTION_DOWN) return true
                val idx =
                    when (ev.keyCode) {
                        KeyEvent.KEYCODE_MEDIA_PLAY -> Ocbm.MEDIA_BTN_PLAY
                        KeyEvent.KEYCODE_MEDIA_PAUSE -> Ocbm.MEDIA_BTN_PAUSE
                        KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, KeyEvent.KEYCODE_HEADSETHOOK -> Ocbm.MEDIA_BTN_PLAY_PAUSE
                        KeyEvent.KEYCODE_MEDIA_NEXT -> Ocbm.MEDIA_BTN_NEXT
                        KeyEvent.KEYCODE_MEDIA_PREVIOUS -> Ocbm.MEDIA_BTN_PREV
                        else -> return super.onMediaButtonEvent(intent)
                    }
                send(idx, "key ${ev.keyCode}")
                return true
            }
        }

    private companion object {
        /** 512 px covers every AAOS now-playing card on this build with room to spare. */
        const val ART_MAX_PX = 512
    }

    private fun send(
        index: Byte,
        what: String,
    ) {
        // Fire-and-forget to the phone. No local state changes here on purpose — iOS owns playback,
        // and optimistically flipping the card would desynchronise it from the phone on the first
        // press that iOS handles differently than we guessed.
        input.mediaButton(index)
        log.i("transport: $what -> HID media button $index")
    }
}
