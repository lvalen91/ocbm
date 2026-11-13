package zeno.gmccpa.av

import android.app.PendingIntent
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
import zeno.gmccpa.MainActivity
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.SessionTrace
import zeno.gmccpa.pair.NativeCore
import java.lang.ref.WeakReference

/**
 * Registers the app as an AAOS **media source**, and publishes the CarPlay now-playing state to it.
 *
 * ## Why this exists at all
 *
 * AAOS does not decide "this app plays audio, therefore it is a media source". It enumerates media
 * sources by querying for services that answer `android.media.browse.MediaBrowserService`. Without one,
 * the app plays audio perfectly and is still invisible to the source switcher — selecting FM then has
 * no way back to us except relaunching the app by hand. (GM HomeScreen also lists every such service as
 * an app-grid tile; the manifest's `com.gm.homescreen.visibility=HIDDEN` on this service removes that
 * second icon and leaves the source registration alone.)
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

    /** Written and read only under [pubLock] (onCreate/onDestroy and every publish path). */
    private var session: MediaSession? = null
    private var warnedNoPlaybackStatus = false

    /** Decoded artwork and the JPEG it came from, as one immutable pair (F098; see `art`). */
    private class Art(val jpeg: ByteArray, val bitmap: Bitmap?)

    /**
     * Transport sends block on the native event-channel lock, and callbacks arrive on a binder thread.
     * They go to the process-wide [CarPlayEventTx] (one daemon thread), which keeps a slow phone from
     * tying up the binder pool AND orders a PLAY behind the OD-9C MainAudio give-back that the resume
     * intent queued just before it (plan v5.9 MED M5).
     */
    private val tx = CarPlayEventTx

    companion object {
        /** Any stable non-empty id. AAOS only uses it to scope onLoadChildren. */
        private const val ROOT_ID = "carplay_root"
        /** [publishState]: at most one elapsed-only publish per whole second of position. See there. */
        private const val FLOOR_MS = 1_000L
        /** [publishState]: a reported position this far from the last publish's prediction is a seek. */
        private const val DRIFT_MS = 1_500L

        /**
         * Album art is downscaled to this before it reaches the session. iOS ships up to ~1400 px
         * (~100 KB JPEG), which is several MB decoded, and `MediaSession.setMetadata` would downscale
         * it again anyway against `config_mediaMetadataBitmapMaxSize`. 512 px covers every card on
         * this head unit.
         */
        private const val ART_MAX_PX = 512

        /**
         * Transport actions while a CarPlay session is UP (between [onSessionUp] and [onSessionDown]).
         * Idle advertises 0 — OUR choice (plan v5.9 MED M2), not stock's: CT5 Cinemo's
         * `CarPlayMediaSession.getPlaybackActions()` returns the constant 55L (STOP|PAUSE|PLAY|PREV|NEXT)
         * in every state, and its session end only clears metadata and calls setActive(false)
         * (corrected 2026-10-08, D-DOC 2). The reasoning stands on its own: an idle source with
         * actions never becomes "playing", so AAOS's media-button selection can prefer a source
         * that did play.
         */
        private const val ACTIONS = PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or
            PlaybackState.ACTION_PLAY_PAUSE or PlaybackState.ACTION_SKIP_TO_NEXT or
            PlaybackState.ACTION_SKIP_TO_PREVIOUS

        /** [SessionTrace.Board] entry. Bound/unbound is AAOS's decision, so the board is the only
         *  place "is the app in the source switcher right now" is answerable from a capture. */
        private const val BOARD = "media-browser"

        private val log = ProbeLog.sub("mbs")

        /**
         * One lock for "is the session up" and "what the card shows" (F097/F098, 2026-10-07). Before
         * it, publish checked [sessionUp] and then published with nothing held, so a seam thread that
         * had passed the check could land the dead session's track and PLAYING AFTER onSessionDown's
         * publishIdle, and the card stayed on a stale PLAYING track. Every write of [sessionUp],
         * [last], [art], [live] and the instance's `session` happens under it, and so does every
         * publish to the MediaSession. Order: pubLock, then the instance monitor (publishState,
         * publishIdle, resetPublishCache); nothing takes them the other way round. A JPEG decode is
         * never done under it (see [publish]).
         */
        private val pubLock = Any()

        @Volatile private var live: WeakReference<CarPlayMediaBrowserService>? = null

        /**
         * Last snapshot, so a service bound AFTER the session came up still shows the current track.
         * AAOS binds Media Center on its own schedule, which is routinely later than RECORD.
         */
        @Volatile private var last: NowPlayingState.Snapshot? = null
        @Volatile private var sessionUp = false

        /**
         * Bumped by [onSessionDown] under [pubLock]. [publish] (before its unlocked decode) and
         * [publishPlaybackState] sample it at entry and drop the record if it moved, so a record from a session that ended during the decode
         * cannot pass the re-check by landing after the NEXT session's [onSessionUp].
         */
        @Volatile private var gen = 0

        /**
         * Decoded artwork, keyed by the identity of the JPEG it came from. One immutable pair in one
         * field (F098): the old `artBitmap`/`artFor` fields were written as two unsynchronized
         * stores, so a reader between them could pair one track's blob with another's bitmap.
         * Written only under [pubLock]. Process-wide, not per instance, so a re-bound service's late
         * bind has the picture without decoding; dropped at [onSessionDown].
         */
        @Volatile private var art: Art? = null

        /**
         * Resume-intent hook (plan v5.9 MED M5): invoked on a MediaSession play / play-pause (callback
         * or key), on a binder thread, before the HID send. The media player's focus-reclaim rule
         * ([FocusReclaimRule]) counts it as the user asking for CarPlay audio back after a permanent
         * focus loss. Wired by CarPlayActivity; the consumer must only stamp.
         */
        @Volatile var onResumeIntent: (() -> Unit)? = null

        /** Seam entry point — the displayed metadata changed. Callable from any thread. */
        fun publish(s: NowPlayingState.Snapshot) {
            // gen BEFORE sessionUp (v5.7w1): read after, a down+up landing between the two reads
            // hands an old-session record the NEW gen, and it passes the re-check below.
            val g = gen
            if (!sessionUp) return
            // Decode OUTSIDE the lock: decodeBounded is pure, and the seam thread must never wait
            // behind a decode running on another thread. Done whether or not a service is bound, so
            // the picture is ready for a late bind (a new blob only; the identity cache absorbs the
            // rest, as before).
            val a = if (s.hasContent) artFor(s.artwork) else null
            synchronized(pubLock) {
                if (!sessionUp || gen != g) return
                last = s
                if (s.hasContent) art = a
                live?.get()?.publishNow(s, a)
            }
        }

        /** Seam entry point — the ~2 Hz elapsed-only tick. Never rebuilds `MediaMetadata`. */
        fun publishPlaybackState(s: NowPlayingState.Snapshot) {
            val g = gen   // before sessionUp — see [publish]
            if (!sessionUp) return
            synchronized(pubLock) {
                if (!sessionUp || gen != g) return
                last = s
                live?.get()?.publishState(s)
            }
        }

        /** The cached pair if it is for [jpeg], else a fresh decode. Writes nothing. */
        private fun artFor(jpeg: ByteArray?): Art? {
            if (jpeg == null) return null
            art?.let { if (it.jpeg === jpeg) return it }   // decode once per blob, including a failed decode
            val bm = runCatching { decodeBounded(jpeg) }.getOrNull()
            if (bm == null) log.w("artwork (${jpeg.size} B) failed to decode — publishing without an image")
            return Art(jpeg, bm)
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

        /**
         * A CarPlay session came up. Note this does NOT flip the card to PLAYING: playback state is
         * whatever iOS reports in `playbackStatus`, never inferred from the existence of a session or
         * from bytes on the audio seam. The phone may hand us a paused player.
         */
        fun onSessionUp() {
            synchronized(pubLock) {
                val wasUp = sessionUp
                sessionUp = true
                live?.get()?.resetPublishCache()
                // STATE_NONE + ACTIONS + "CarPlay" for the gap between session up and the first track
                // (stock: STATE_NONE at start, actions while started), so "select CarPlay, press play"
                // offers PLAY. ONLY for a genuinely new session (rev c, C1): this is also reached on
                // every launcher resume during a live session and from the carplay_ui verb (both via
                // SC.showCarPlayScreen), where [sessionUp] is already true and [last] holds the live
                // track — publishing here would blank a playing card until the next metadata change.
                // onSessionDown resets both, so the next real session passes.
                if (!wasUp && last == null) live?.get()?.publishSessionReady()
            }
            // Not a fault at this instant — AAOS binds Media Center on its own schedule, routinely
            // after RECORD — but a session that ends with this still DOWN never appeared in the
            // source switcher, and the board is the only place that is visible.
            if (live?.get() != null) SessionTrace.Board.up(BOARD, "bound by AAOS; session up")
            else SessionTrace.Board.down(BOARD, "not bound by AAOS at session up — not in the source switcher until Media Center binds")
        }

        /**
         * The session ended. This IS an inference we are entitled to make — the phone will never send
         * a final "stopped" record, it simply stops — so idle is asserted here. Clearing [sessionUp]
         * also drops any seam record still in flight from the dead session.
         */
        fun onSessionDown() {
            // Under the lock, so no seam publish can interleave between clearing and the idle card.
            val bound = synchronized(pubLock) {
                sessionUp = false; last = null; art = null; gen++
                // The merge base is SESSION-scoped (plan v5.9 MED M4, rev b R22): cleared here, on the
                // session's end, and no longer in CarPlayActivity.stopSession, which an Activity destroy
                // with the session still up also reaches. Every session end with a receiver arrives here
                // through SC.tearDownSessionConsumers. clear() publishes the empty picture with its
                // own monitor released; [sessionUp] is already false, so that publish is dropped and
                // never takes [pubLock] — the idle card below is what AAOS sees.
                CarPlayActivity.nowPlaying.clear()
                live?.get()?.also { it.publishIdle() }
            }
            if (bound != null) SessionTrace.Board.up(BOARD, "bound by AAOS; session idle (card cleared)")
        }

        /**
         * The CarPlay SCREEN went away while the session stays up (plan v5.9 MED M4 (b)); called from
         * CarPlayActivity.onDestroy when the receiver still reports `sessionLive`. stopSession has
         * stopped the AAC track, so PLAYING would be false: the same metadata is kept and the state
         * becomes STATE_PAUSED. The next record from a recreated screen republishes the phone's own
         * state (the merge base is intact — see [onSessionDown]).
         */
        fun onScreenGone() {
            synchronized(pubLock) {
                if (!sessionUp) return
                val cur = last ?: return
                if (!cur.hasContent) return
                val paused = cur.copy(playbackStatus = NowPlayingState.PAUSE)
                last = paused
                live?.get()?.publishState(paused, force = true)
            }
            log.i("CarPlay screen gone with the session up — card held as PAUSED")
        }
    }

    override fun onCreate() {
        super.onCreate()
        // BROWSE_SERVICE as stock does it (CarPlayMediaSession.createMediaSession): the SAME extras as
        // the constructor's sessionInfo (API 29+; minSdk is 32) AND through setExtras. What GM does
        // with it is UNKNOWN (media_audit D7); stock sets both.
        val extras = Bundle().apply {
            putString("android.media.session.BROWSE_SERVICE", CarPlayMediaBrowserService::class.java.name)
        }
        val s = MediaSession(this, "CarPlayRx", extras)
        s.setExtras(extras)
        // Deprecated (API 26: always set), kept so getFlags() reports them; suppressed here only.
        @Suppress("DEPRECATION")
        s.setFlags(MediaSession.FLAG_HANDLES_MEDIA_BUTTONS or MediaSession.FLAG_HANDLES_TRANSPORT_CONTROLS)
        s.setCallback(callback)
        // F101 (2026-10-07): tapping the media card had nowhere to go. An intent for MainActivity:
        // singleTask, so a running instance gets onNewIntent and its onResume restores the CarPlay
        // screen while the session is live. Tagged [MainActivity.EXTRA_FROM_MEDIA_CARD] (v5.7w1, R3):
        // a tap only brings the app forward — dispatchIntent skips autoStart/runAll for it, so a tap
        // with no live session cannot kick off a connect. No setMediaButtonReceiver: that needs a new
        // exported receiver for no shown benefit. Its own request code and action ([TapTarget],
        // D-F1): the status notification's PendingIntent targets the same component, and extras do
        // not distinguish two PendingIntents — with code 0 on both, a card tap could be handed the
        // notification's untagged intent and auto-start over an on-screen Stop.
        s.setSessionActivity(PendingIntent.getActivity(this, TapTarget.MEDIA_CARD_REQUEST,
            Intent(this, MainActivity::class.java).setAction(TapTarget.MEDIA_CARD_ACTION)
                .putExtra(MainActivity.EXTRA_FROM_MEDIA_CARD, true),
            PendingIntent.FLAG_IMMUTABLE))
        sessionToken = s.sessionToken
        // One lock section from "live" to the late bind: a seam publish lands either wholly before it
        // (then the late bind repeats it from [last]) or wholly after (then it publishes to this
        // instance itself), never between publishIdle and the late bind.
        synchronized(pubLock) {
            session = s
            live = WeakReference(this)
            resetPublishCache()
            // The session stays ACTIVE for the service's whole life rather than being deactivated when
            // there is no content (owner decision 2026-10-07, plan v5.9 MED M2: keep the source
            // selectable): the source switcher must be able to select CarPlay BEFORE any audio flows,
            // and a source that only appears once it is already playing cannot be switched *to*.
            // Idle is therefore expressed as STATE_STOPPED, actions 0 and placeholder metadata, not as
            // an inactive session. Stock deviation, recorded: stock also calls setActive(false) at
            // session end; whether GM still routes wheel keys to an active idle actions-0 session is
            // UNKNOWN (U59-8).
            publishIdle()
            s.isActive = true
            // Late bind: session was already up. With a track, publish it: [art] was committed with the
            // last metadata publish, so no decode happens here; a mismatch (none expected) publishes
            // without an image. With no track yet (or only contentless ticks), the session-ready card,
            // as onSessionUp would have published to a bound instance.
            if (sessionUp) {
                val cur = last
                if (cur != null && cur.hasContent) publishNow(cur, art?.takeIf { it.jpeg === cur.artwork })
                else publishSessionReady()
            }
        }
        log.i("registered as an AAOS media source (session active)")
        SessionTrace.Board.up(BOARD, "bound by AAOS; session ${if (sessionUp) "up" else "idle"}")
        // The media host creates this process (`Start proc … for service`) before any Activity runs,
        // so capture starts here too. start() is idempotent, non-blocking and self-reaping.
        runCatching { LogCapture.start(applicationContext, CapturePrefs.config(applicationContext)) }
        // plan v5.9 D6 trigger T3: at boot this bind (~+7 s, when CCPA is the media source) is the
        // first thing that runs in this process — claim an adapter that is already present.
        zeno.gmccpa.AutoClaim.trigger(applicationContext, zeno.gmccpa.AutoClaim.Origin.MEDIA_BIND)
    }

    override fun onDestroy() {
        // Under the lock, so a seam publish never reaches a released session. The process-wide [art]
        // is kept: a re-bind during the same session reuses it, and onSessionDown drops it.
        synchronized(pubLock) {
            if (live?.get() === this) live = null
            session?.let { it.isActive = false; it.release() }
            session = null
        }
        // No executor shutdown: [CarPlayEventTx] is process-wide and also carries the player's MainAudio
        // commands; AAOS unbinding the browser must not stop those.
        SessionTrace.Board.down(BOARD, "unbound by AAOS — not in the source switcher until it re-binds")
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

    /**
     * No CarPlay session: STATE_STOPPED with actions 0 (ours, see [ACTIONS]; stock keeps 55L) and the
     * placeholder title. Caller holds [pubLock]. Leaves [art] alone: onCreate calls this while a
     * session may be up (the late bind then publishes over it).
     */
    @Synchronized
    private fun publishIdle() = publishPlaceholder(PlaybackState.STATE_STOPPED, 0L)

    /**
     * A session is up and no track has been published yet: STATE_NONE with [ACTIONS] (stock:
     * STATE_NONE at session start, actions while started). Caller holds [pubLock].
     */
    @Synchronized
    private fun publishSessionReady() = publishPlaceholder(PlaybackState.STATE_NONE, ACTIONS)

    /** Caller holds [pubLock] and this monitor. */
    private fun publishPlaceholder(state: Int, actions: Long) {
        lastPub = null
        val sess = session ?: return
        runCatching {
            sess.setMetadata(
                MediaMetadata.Builder().putString(MediaMetadata.METADATA_KEY_TITLE, "CarPlay").build()
            )
            sess.setPlaybackState(
                PlaybackState.Builder().setActions(actions)
                    .setState(state, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 0f,
                        SystemClock.elapsedRealtime())
                    .build()
            )
        }.onFailure { log.e("placeholder publish (state $state) failed: ${it.message}") }
    }

    /** Caller holds [pubLock]. [a] is the pre-decoded artwork for `s.artwork` (null: no image). */
    private fun publishNow(s: NowPlayingState.Snapshot, a: Art?) {
        val sess = session ?: return
        // Keep the previous card rather than flashing the "CarPlay" placeholder mid-track: an
        // artwork-only or status-only update legitimately carries no title.
        if (!s.hasContent) return
        val b = MediaMetadata.Builder()
            .putString(MediaMetadata.METADATA_KEY_TITLE, s.title.orEmpty())
            .putString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE, s.title.orEmpty())
        // Absent fields are OMITTED, as stock does (plan v5.9 MED M6), not published as "".
        s.artist?.takeIf { it.isNotEmpty() }?.let {
            b.putString(MediaMetadata.METADATA_KEY_ARTIST, it)
            b.putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, it)
        }
        s.album?.takeIf { it.isNotEmpty() }?.let { b.putString(MediaMetadata.METADATA_KEY_ALBUM, it) }
        s.genre?.let { b.putString(MediaMetadata.METADATA_KEY_GENRE, it) }
        s.composer?.let { b.putString(MediaMetadata.METADATA_KEY_COMPOSER, it) }
        if (s.durationMs > 0) b.putLong(MediaMetadata.METADATA_KEY_DURATION, s.durationMs)
        if (s.trackNumber > 0) b.putLong(MediaMetadata.METADATA_KEY_TRACK_NUMBER, s.trackNumber.toLong())
        if (s.trackCount > 0) b.putLong(MediaMetadata.METADATA_KEY_NUM_TRACKS, s.trackCount.toLong())
        a?.bitmap?.let {
            // BOTH keys: AAOS consumers disagree on which they read, and a card with no image where
            // one exists is the usual symptom of picking only one.
            b.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, it)
            b.putBitmap(MediaMetadata.METADATA_KEY_ART, it)
        }
        runCatching { sess.setMetadata(b.build()) }.onFailure { log.e("setMetadata failed: ${it.message}") }
        publishState(s, force = true)
    }

    /**
     * What [publishState] last handed the session, so the ~2 Hz elapsed tick can be skipped while
     * nothing a controller shows has changed. Guarded by this instance's monitor (and, since F097, by
     * [pubLock] at every caller): publishState runs on cp-meta (the seam) and on main (onCreate ->
     * publishNow).
     */
    private class Published(val state: Int, val rate: Float, val posMs: Long, val atMs: Long)
    private var lastPub: Published? = null

    @Synchronized
    private fun resetPublishCache() { lastPub = null }

    /**
     * Separate from [publishNow] on purpose: position moves ~2 Hz and the metadata does not. Rebuilding
     * a `MediaMetadata` at that rate would churn every subscribed controller for a value that belongs
     * in the playback state.
     */
    @Synchronized
    private fun publishState(s: NowPlayingState.Snapshot, force: Boolean = false) {
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
        // Throttle (2026-10-06, F4): each publish is one CAR.MEDIA savePlaybackState() plus one GM
        // DataRepStateReceiver line, ~2.2 Hz on the elapsed tick. GM DataRep logs a new whole-second
        // mElapsedTime on every publish, 1:1, and whether the GM card and the cluster interpolate
        // between publishes is NOT known — "controllers interpolate" was our own assumption — so the
        // floor is one publish per whole second (FLOOR_MS), never longer. Skipped only when state and
        // rate are unchanged, the reported position is within DRIFT_MS of where the last publish
        // predicts, AND the whole second has not changed. Raising the floor needs on-card evidence.
        val now = SystemClock.elapsedRealtime()
        val prev = lastPub
        if (!force && prev != null && prev.state == state && prev.rate == rate &&
            kotlin.math.abs(s.elapsedMs - (prev.posMs + (prev.rate * (now - prev.atMs)).toLong())) < DRIFT_MS &&
            s.elapsedMs / FLOOR_MS == prev.posMs / FLOOR_MS) return
        val ps = PlaybackState.Builder().setActions(ACTIONS)
            .setState(state, s.elapsedMs, rate, now)
            .build()
        runCatching { sess.setPlaybackState(ps) }
            .onSuccess { lastPub = Published(state, rate, s.elapsedMs, now) }
            .onFailure { log.e("setPlaybackState failed: ${it.message}") }
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
        override fun onPlay() { resumeIntent(); send(NativeCore.MediaBtn.PLAY, "play") }
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
            if (idx == NativeCore.MediaBtn.PLAY || idx == NativeCore.MediaBtn.PLAY_PAUSE) resumeIntent()
            send(idx, "key ${ev.keyCode}")
            return true
        }
    }

    /**
     * See [onResumeIntent]. Never lets a consumer fault reach the binder thread.
     *
     * OD-9C first: if a permanent focus loss made us take MainAudio from the phone, give it back
     * BEFORE the caller queues its PLAY key — same [CarPlayEventTx] queue, so the phone sees the
     * Untake first. Not through the player: the session (and the take) can outlive the CarPlay
     * screen that owned the player.
     */
    private fun resumeIntent() {
        runCatching { CarPlayEventTx.mainAudio.giveBack("media play") }.onFailure { log.e("MainAudio give-back threw: ${it.message}") }
        runCatching { onResumeIntent?.invoke() }.onFailure { log.e("resume-intent consumer threw: ${it.message}") }
    }

    private fun send(index: Int, what: String) {
        runCatching {
            tx.execute {
                val ok = NativeCore.mediaButton(index)
                // A refused send on a LIVE session is a dead event channel under a wheel button —
                // a W. With no session it is AAOS probing the source at bind time (2026-09-09,
                // 22:51:14.898, `play -> sent=false` four minutes before any phone) and stays I.
                if (ok) log.i("transport: $what -> HID media $index sent=true")
                else if (sessionUp) log.w("transport: $what -> HID media $index sent=false — the event channel refused it on a live session")
                else log.i("transport: $what -> HID media $index sent=false (no session; nothing to send to)")
            }
        }.onFailure { log.e("transport $what dropped: ${it.message}") }
    }
}
