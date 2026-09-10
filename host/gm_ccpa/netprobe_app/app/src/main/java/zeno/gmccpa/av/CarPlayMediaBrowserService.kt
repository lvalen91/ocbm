package zeno.gmccpa.av

import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.MediaMetadata
import android.media.browse.MediaBrowser
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Bundle
import android.os.SystemClock
import android.service.media.MediaBrowserService
import android.view.KeyEvent
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.pair.NativeCore
import java.lang.ref.WeakReference
import java.util.concurrent.Executors

/**
 * Registers the app as an AAOS **media source**, and publishes the CarPlay now-playing state to it.
 *
 * ## Why this exists at all
 *
 * AAOS does not decide "this app plays audio, therefore it is a media source". It enumerates media
 * sources by querying for services that answer `android.media.browse.MediaBrowserService`. Without one,
 * the app plays audio perfectly and is still invisible to the source switcher and the homescreen Media
 * card — selecting FM then has no way back to us except relaunching the app by hand.
 *
 * This is not theoretical: with the service registered, `dumpsys car_service` shows
 * `Current playback media component: zeno.gmccpa/.av.CarPlayMediaBrowserService` and six controllers
 * subscribed to the session below (device-observed 2026-09-08). They were being answered with a
 * constant title "CarPlay" and `STATE_STOPPED` while HEVC and AAC were streaming; this class now
 * answers them with what the phone actually reports.
 *
 * ## Framework class, not the AndroidX one
 *
 * This extends the platform's `android.service.media.MediaBrowserService`, NOT
 * `MediaBrowserServiceCompat`. `tools/build_apk.sh` compiles against `android.jar` + `kotlin-stdlib`
 * only — there is no AndroidX on the classpath and no Gradle dependency resolution — so the compat
 * class is unavailable. The framework class has answered the same intent action since API 21 and AAOS
 * accepts it; the compat library's value is pre-21 support and Media3 features we do not use.
 *
 * ## Deliberately not browsable
 *
 * CarPlay is a projection surface: the media library lives on the phone and is drawn by the phone. So
 * [onLoadChildren] returns an empty list. That is a legitimate media source — AAOS shows it as a source
 * with playback controls rather than a browse tree. Returning `null` instead would make AAOS treat the
 * connection as failed and drop us from the source list.
 *
 * A real browse tree would need iAP2 `MediaLibraryUpdate` (0x4C04), which is `Tier::Capability` and so
 * requires `tier: all` — a declaration iOS has already REFUTED on the wireless tunnel arm with an
 * unrecoverable `0x1D03`. The stub is the correct end state here, not a placeholder.
 *
 * ## Where the data comes from
 *
 * The `:9004` seam ([MetadataSeam]) feeds [NowPlayingState], which calls [publish] and
 * [publishPlaybackState] here. AAOS owns THIS service's lifetime — it is constructed when Media Center
 * binds, not by us — so the seam reader cannot hold an instance. Hence the process-wide [live] weak
 * reference, the same idiom as `CarPlayActivity.live`.
 */
class CarPlayMediaBrowserService : MediaBrowserService() {

    private val log = ProbeLog.sub("mbs")
    private var session: MediaSession? = null

    @Volatile private var artBitmap: Bitmap? = null
    @Volatile private var artFor: ByteArray? = null
    private var warnedNoPlaybackStatus = false

    /**
     * Transport sends block on the native event-channel lock, and callbacks arrive on a binder thread.
     * One daemon thread keeps a slow phone from tying up the binder pool.
     */
    private val tx = Executors.newSingleThreadExecutor { Thread(it, "cp-media-btn").apply { isDaemon = true } }

    companion object {
        /** Any stable non-empty id. AAOS only uses it to scope onLoadChildren. */
        private const val ROOT_ID = "carplay_root"

        /**
         * Album art is downscaled to this before it reaches the session. iOS ships up to ~1400 px
         * (~100 KB JPEG), which is several MB decoded, and `MediaSession.setMetadata` would downscale
         * it again anyway against `config_mediaMetadataBitmapMaxSize`. 512 px covers every card on
         * this head unit.
         */
        private const val ART_MAX_PX = 512

        private const val ACTIONS = PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or
            PlaybackState.ACTION_PLAY_PAUSE or PlaybackState.ACTION_SKIP_TO_NEXT or
            PlaybackState.ACTION_SKIP_TO_PREVIOUS

        @Volatile private var live: WeakReference<CarPlayMediaBrowserService>? = null

        /**
         * Last snapshot, so a service bound AFTER the session came up still shows the current track.
         * AAOS binds Media Center on its own schedule, which is routinely later than RECORD.
         */
        @Volatile private var last: NowPlayingState.Snapshot? = null
        @Volatile private var sessionUp = false

        /** Seam entry point — the displayed metadata changed. Callable from any thread. */
        fun publish(s: NowPlayingState.Snapshot) {
            if (!sessionUp) return
            last = s
            live?.get()?.publishNow(s)
        }

        /** Seam entry point — the ~2 Hz elapsed-only tick. Never rebuilds `MediaMetadata`. */
        fun publishPlaybackState(s: NowPlayingState.Snapshot) {
            if (!sessionUp) return
            last = s
            live?.get()?.publishState(s)
        }

        /**
         * A CarPlay session came up. Note this does NOT flip the card to PLAYING: playback state is
         * whatever iOS reports in `playbackStatus`, never inferred from the existence of a session or
         * from bytes on the audio seam. The phone may hand us a paused player.
         */
        fun onSessionUp() { sessionUp = true }

        /**
         * The session ended. This IS an inference we are entitled to make — the phone will never send
         * a final "stopped" record, it simply stops — so idle is asserted here. Clearing [sessionUp]
         * also drops any seam record still in flight from the dead session.
         */
        fun onSessionDown() { sessionUp = false; last = null; live?.get()?.publishIdle() }
    }

    override fun onCreate() {
        super.onCreate()
        val s = MediaSession(this, "CarPlayRx")
        s.setFlags(MediaSession.FLAG_HANDLES_MEDIA_BUTTONS or MediaSession.FLAG_HANDLES_TRANSPORT_CONTROLS)
        s.setCallback(callback)
        session = s
        sessionToken = s.sessionToken
        live = WeakReference(this)
        // The session stays ACTIVE for the service's whole life rather than being deactivated when
        // there is no content: the source switcher must be able to select CarPlay BEFORE any audio
        // flows, and a source that only appears once it is already playing cannot be switched *to*.
        // Idle is therefore expressed as STATE_STOPPED plus placeholder metadata, not as an inactive
        // session.
        publishIdle()
        s.isActive = true
        last?.let { if (sessionUp) publishNow(it) }   // late bind: session was already up
        log.i("registered as an AAOS media source (session active)")
    }

    override fun onDestroy() {
        if (live?.get() === this) live = null
        session?.let { it.isActive = false; it.release() }
        session = null
        artBitmap = null; artFor = null
        tx.shutdownNow()
        super.onDestroy()
    }

    /**
     * Every caller is allowed. This is a projection surface with no library to protect, and rejecting
     * unknown callers (the usual package-allowlist pattern) is how an app silently disappears from the
     * source list on an OEM build whose Media Center package you did not anticipate.
     */
    override fun onGetRoot(clientPackageName: String, clientUid: Int, rootHints: Bundle?): BrowserRoot =
        BrowserRoot(ROOT_ID, null)

    /** Empty, not null — see the class docs. Null reads as a failed connection. */
    override fun onLoadChildren(parentId: String, result: Result<MutableList<MediaBrowser.MediaItem>>) {
        result.sendResult(mutableListOf())
    }

    // ---- publishing ------------------------------------------------------------------------------

    private fun publishIdle() {
        val sess = session ?: return
        artBitmap = null; artFor = null
        sess.setMetadata(
            MediaMetadata.Builder().putString(MediaMetadata.METADATA_KEY_TITLE, "CarPlay").build()
        )
        sess.setPlaybackState(
            PlaybackState.Builder().setActions(ACTIONS)
                .setState(PlaybackState.STATE_STOPPED, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 0f,
                    SystemClock.elapsedRealtime())
                .build()
        )
    }

    private fun publishNow(s: NowPlayingState.Snapshot) {
        val sess = session ?: return
        // Keep the previous card rather than flashing the "CarPlay" placeholder mid-track: an
        // artwork-only or status-only update legitimately carries no title.
        if (!s.hasContent) return
        val b = MediaMetadata.Builder()
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
            // BOTH keys: AAOS consumers disagree on which they read, and a card with no image where
            // one exists is the usual symptom of picking only one.
            b.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, it)
            b.putBitmap(MediaMetadata.METADATA_KEY_ART, it)
        }
        runCatching { sess.setMetadata(b.build()) }.onFailure { log.e("setMetadata failed: ${it.message}") }
        publishState(s)
    }

    /**
     * Separate from [publishNow] on purpose: position moves ~2 Hz and the metadata does not. Rebuilding
     * a `MediaMetadata` at that rate would churn every subscribed controller for a value that belongs
     * in the playback state.
     */
    private fun publishState(s: NowPlayingState.Snapshot) {
        val sess = session ?: return
        // iAP2 PlaybackStatus maps 1:1. A scrub reported as "playing" is a visible lie on the card.
        val (state, rate) = when (s.playbackStatus) {
            NowPlayingState.PLAY -> PlaybackState.STATE_PLAYING to 1.0f
            NowPlayingState.SEEK_FWD -> PlaybackState.STATE_FAST_FORWARDING to 2.0f
            NowPlayingState.SEEK_BACK -> PlaybackState.STATE_REWINDING to -2.0f
            NowPlayingState.PAUSE -> PlaybackState.STATE_PAUSED to 0.0f
            NowPlayingState.STOP -> PlaybackState.STATE_STOPPED to 0.0f
            else -> {
                // Never told. Say so once: otherwise the card silently freezes at "paused" with no
                // trace of why, and the cause (PlaybackAttributes missing from the subscribe) is
                // three layers away.
                if (s.hasContent && !warnedNoPlaybackStatus) {
                    warnedNoPlaybackStatus = true
                    log.w("track with no iAP2 playbackStatus — check NowPlayingUpdate PlaybackAttributes are subscribed")
                }
                PlaybackState.STATE_PAUSED to 0.0f
            }
        }
        // Rate + elapsedRealtime let each controller interpolate position locally instead of stepping
        // it twice a second.
        val ps = PlaybackState.Builder().setActions(ACTIONS)
            .setState(state, s.elapsedMs, rate, SystemClock.elapsedRealtime())
            .build()
        runCatching { sess.setPlaybackState(ps) }.onFailure { log.e("setPlaybackState failed: ${it.message}") }
    }

    private fun bitmapFor(jpeg: ByteArray?): Bitmap? {
        if (jpeg == null) { artBitmap = null; artFor = null; return null }
        if (artFor === jpeg && artBitmap != null) return artBitmap   // decode once per blob
        val bm = runCatching { decodeBounded(jpeg) }.getOrNull()
        if (bm == null) log.w("artwork (${jpeg.size} B) failed to decode — publishing without an image")
        artBitmap = bm; artFor = jpeg
        return bm
    }

    /**
     * Bounds-probe then power-of-two subsample, so a 1400 px JPEG never decodes at full size. The box
     * deliberately forwards over-long artwork buffers (it has no link-layer duplicate suppression), so
     * a corrupt image is an expected input, not an exceptional one — hence the null return rather than
     * a throw.
     */
    private fun decodeBounded(jpeg: ByteArray): Bitmap? {
        val probe = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size, probe)
        if (probe.outWidth <= 0 || probe.outHeight <= 0) return null
        var sample = 1
        while (probe.outWidth / sample > ART_MAX_PX || probe.outHeight / sample > ART_MAX_PX) sample *= 2
        return BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size,
            BitmapFactory.Options().apply { inSampleSize = sample })
    }

    // ---- transport controls ----------------------------------------------------------------------

    /**
     * Transport controls travel to the phone as taps on the uid-2 Consumer-Control HID device.
     *
     * This replaces an earlier claim in this file that "the existing OCBM INPUT_MEDIA_BTN path already
     * carries wheel buttons to the phone". That was inherited from the WIRED design and is false here:
     * `INPUT_MEDIA_BTN` is consumed by the BOX's `carplayd`, which in this app's bridge role never
     * spawns its A/V layer and therefore holds no event channel — the report would go nowhere. Nothing
     * in this app ever emitted the opcode either. Meanwhile wheel keys arrive as `KeyEvent`s routed to
     * the active `MediaSession` — this one — where the previously empty callback swallowed them. The
     * buttons were dead twice over.
     *
     * No local state changes here: iOS owns playback, and the card updates when the phone's next
     * nowPlaying record says it did. That is the same ordering a built-in head unit sees.
     */
    private val callback = object : MediaSession.Callback() {
        override fun onPlay() = send(NativeCore.MediaBtn.PLAY, "play")
        override fun onPause() = send(NativeCore.MediaBtn.PAUSE, "pause")
        override fun onStop() = send(NativeCore.MediaBtn.PAUSE, "stop->pause")
        override fun onSkipToNext() = send(NativeCore.MediaBtn.NEXT, "next")
        override fun onSkipToPrevious() = send(NativeCore.MediaBtn.PREV, "prev")

        /**
         * Handled explicitly so PLAY_PAUSE reaches iOS as the dedicated toggle rather than being
         * guessed into play-or-pause from a state we do not own.
         */
        override fun onMediaButtonEvent(intent: Intent): Boolean {
            @Suppress("DEPRECATION")
            val ev = intent.getParcelableExtra<KeyEvent>(Intent.EXTRA_KEY_EVENT)
                ?: return super.onMediaButtonEvent(intent)
            if (ev.action != KeyEvent.ACTION_DOWN) return true
            val idx = when (ev.keyCode) {
                KeyEvent.KEYCODE_MEDIA_PLAY -> NativeCore.MediaBtn.PLAY
                KeyEvent.KEYCODE_MEDIA_PAUSE -> NativeCore.MediaBtn.PAUSE
                KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, KeyEvent.KEYCODE_HEADSETHOOK -> NativeCore.MediaBtn.PLAY_PAUSE
                KeyEvent.KEYCODE_MEDIA_NEXT -> NativeCore.MediaBtn.NEXT
                KeyEvent.KEYCODE_MEDIA_PREVIOUS -> NativeCore.MediaBtn.PREV
                else -> return super.onMediaButtonEvent(intent)
            }
            send(idx, "key ${ev.keyCode}")
            return true
        }
    }

    private fun send(index: Int, what: String) {
        runCatching {
            tx.execute {
                val ok = NativeCore.mediaButton(index)
                log.i("transport: $what -> HID media $index sent=$ok")
            }
        }.onFailure { log.e("transport $what dropped: ${it.message}") }
    }
}
