package zeno.gmccpa.av

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.logging.SessionTrace
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * AAC-LC decoder + [AudioTrack] playback for the CarPlay media stream.
 *
 * The seam delivers ADTS (`forward.rs` wraps the AUs), device-confirmed as AAC-LC 48 kHz stereo, which
 * matches what the SETUP dict negotiates (`audioFormat=0x800000`). ADTS carries its own header, so the
 * decoder is configured from the first frame's header rather than from `/info` — if iOS ever picks a
 * different rate the stream stays self-describing.
 *
 * Playback uses `USAGE_MEDIA`, so it routes through the head unit's normal media path and obeys the
 * volume knob. The `AudioTrack` is built from the ADTS header's rate and channel count — speculatively
 * in [prime], then adopted or rebuilt in [configure] — not from the decoder's reported output format;
 * for AAC-LC (no SBR/PS) the two are identical, which is what CarPlay negotiates.
 *
 * ## Media must HOLD audio focus, or the volume knob gets stuck
 *
 * [am] is not optional in practice. AAOS points the hardware volume control at the current audio-focus
 * owner, and it returns focus to the **previous owner** when a transient holder abandons it. VoiceRouter
 * takes `AUDIOFOCUS_GAIN_TRANSIENT` for Siri and calls; if media never held focus there is nothing to
 * hand back to, so after a call or a Siri turn the knob stays pointed at Phone/Siri while media plays —
 * the head unit's volume UI then adjusts a group the driver cannot hear. Holding `AUDIOFOCUS_GAIN` here
 * gives the system a resting owner to return to. It also lets AAOS duck us properly for navigation
 * rather than relying solely on our own software ducking in [setVoiceDucked]/[setFocusDucked].
 */
class AacPlayer(private val am: android.media.AudioManager? = null) {

    private val log = ProbeLog.sub("aac")
    private val running = AtomicBoolean(false)

    // @Volatile is load-bearing, not decoration: `track` is read cross-thread by stop() for the
    // pause/flush unblock below, and without it the UI thread may legally observe a stale null — the
    // consume thread's write has no happens-before edge to it — and silently pause nothing.
    @Volatile private var codec: MediaCodec? = null
    @Volatile private var track: AudioTrack? = null
    @Volatile private var configureFailedAt = 0L
    // Remembered so a dead AudioTrack can be rebuilt in place without waiting for the next configure.
    @Volatile private var cfgRate = 48000
    @Volatile private var cfgChannels = 2

    /** Frames the AudioTrack took IN FULL. Misnamed (it is "played", not "decoded") but exported, so
     *  the name stays. Until 2026-09-10 every non-negative `write()` return incremented this — see
     *  [framesDiscardedPaused] for the ~240 frames that inflated the 2026-09-09 capture's "843". */
    val framesDecoded = AtomicLong(0)
    val bytesIn = AtomicLong(0)
    /** Decoded PCM that reached no LIVE AudioTrack — a failed prime, a rebuild that itself failed, or
     *  a write the track refused outright (`ERROR_DEAD_OBJECT` or any other negative return). The
     *  last case counted as PLAYED before 2026-09-10. */
    val framesDroppedNoTrack = AtomicLong(0)
    /**
     * Decoded PCM handed to a track that was NOT playing, and therefore thrown away.
     *
     * `AudioTrack.write` in blocking mode stops blocking the moment the track is paused or stopped:
     * native `AudioTrack::obtainBuffer` swaps the caller's timeout for `kNonBlocking` whenever
     * `mState != STATE_ACTIVE`, so on a paused track a full buffer returns 0 at once and a partly
     * full one returns a short count. `pause()` also interrupts the ONE write that was in flight, which
     * then returns whatever it had copied so far. [feed] used to test only `w < 0`, so every one of
     * those returns counted as a played frame. Truck capture 2026-09-09: iOS ended media stream 102
     * 17 ms after the focus pause at 22:55:39.579, started a NEW stream at 22:55:47.179, and the
     * track stayed paused until 22:55:52.295 — the "500 audio frames played" checkpoint printed at
     * 22:55:49.252 with the track paused, and ~240 frames (~5 s) of that new stream were discarded
     * and reported as played. Any before/after underrun comparison must read [framesDecoded] net of
     * this counter.
     *
     * Discarding is the right behaviour, not a defect: the phone's playhead has moved on, and
     * queueing during a hold would replay the hold's length after it ends. Only the count was wrong.
     */
    val framesDiscardedPaused = AtomicLong(0)

    /**
     * `underrunCount` at the first PLAYED frame, so every later underrun figure can be read NET of the
     * fill-timeout episodes [buildTrack] documents as expected (the prime's, and the first stream's).
     * -1 until a frame has played. Consume-thread only. Per-track in principle — a rebuilt track
     * starts from 0 — so the net figure is floored at 0 rather than trusted below it.
     */
    @Volatile private var underrunBaseline = -1
    /**
     * Per-CONNECTION latch for [EXPECT_FIRST_PCM]: the counters above are cumulative across every
     * producer re-dial, so "first frame" per session and "first PCM of this connection" are different
     * events. Consume-thread only.
     */
    private var firstPcmThisConn = false

    private val rates = intArrayOf(96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350)

    private companion object {
        /** Never re-attempt configure per ADTS frame — that drains the codec pool in seconds. */
        const val CONFIGURE_RETRY_MS = 5_000L
        /** How often [reclaimFocus] may re-request after a permanent loss. */
        const val FOCUS_RETRY_MS = 3_000L
        /** 0.2, not 0.8: "duck by 20%" is ~2 dB and was reported by users as "does not duck at all". */
        const val DUCK_GAIN = 0.2f
        /**
         * Media AudioTrack depth, in audio time. See [buildTrack] for what it buys and what it costs.
         *
         * Why a time and not a multiple of `getMinBufferSize`: this unit's `mixport_bus0_media_out`
         * is `AUDIO_OUTPUT_FLAG_PRIMARY` only — there is no DEEP_BUFFER mixPort and no
         * COMPRESS_OFFLOAD anywhere in its `audio_policy_configuration.xml` — so this buffer is the
         * ENTIRE cushion between the phone's delivery jitter and the DAC. `getMinBufferSize` is a HAL
         * period multiple (~40–80 ms here) and knows nothing about the source.
         *
         * Why 750 ms: the phone paces media at roughly realtime with gaps (the 2026-09-09 capture
         * shows the second stream arriving at ~1.9 s of audio per 2.07 s of wall clock — no pre-roll
         * burst), so the phone's lead over the DAC at stream start is exactly this depth, and the
         * ONLY way that lead grows afterwards is an audible underrun of the size of the gap that beat
         * it. The old `maxOf(minBuf, 8192) * 2` (85–160 ms) took 5 underruns in the first 9 s to grow
         * a lead that covered the gaps. 750 ms is ExoPlayer `DefaultAudioSink`'s upper clamp and the
         * value `carlink_native_personal` settled on for the same stream; it has margin over the
         * ~200 ms nominal gap for Wi-Fi jitter in a moving truck. Tune DOWN from here if the truck
         * shows 0 underruns with margin to spare — every millisecond here is first-sound delay on
         * each stream start and stale audio on each track skip (see [buildTrack]).
         */
        const val TRACK_BUFFER_MS = 750L
        /** [SessionTrace] name for "this seam connection produced decoded PCM". Armed in [consume];
         *  `av/`-scoped so `CarPlayActivity.stopSession` can drop it when the AirPlay session ends. */
        const val EXPECT_FIRST_PCM = "av/media-first-pcm"
        /**
         * Budget from seam connect to the first decoded output buffer of that connection. Measured on
         * 2026-09-09: `audio seam connected` 22:55:30.365 → `configured AAC-LC` .415 → `FIRST AUDIO
         * FRAME PLAYED` .541 — **176 ms**, and forward.rs only dials once it has an ADTS frame to
         * send, so a connection with no PCM behind it within 3 s is a decoder that did not configure
         * or is producing nothing. Met on ANY output buffer, played or discarded: whether the track is
         * paused for Siri is a separate, already-logged state and must not turn this into a false
         * alarm (the second stream 102 of that session started 5.1 s into a hold).
         */
        const val FIRST_PCM_MS = 3_000L
        /**
         * Net underruns a session may show before the summary is a `W`. Two, not zero, because two
         * episodes are structural and inaudible: the fill timeout of the first stream can land AFTER
         * the baseline is taken at the first played frame, and a stream the PHONE ends while the
         * track is playing (driver taps pause) drains the buffer into one underrun episode of
         * silence-after-silence. Anything beyond that is an audible gap.
         */
        const val UNDERRUN_TOLERANCE = 2
        /** [SessionTrace.Board] entries. */
        const val BOARD_TRACK = "media-track"
        const val BOARD_FOCUS = "media-focus"
    }

    @Volatile private var focus: android.media.AudioFocusRequest? = null
    /**
     * Ordering stamp between a focus GRANT and the callbacks that follow it. [requestFocus] bumps
     * [focusRequestSeq] under `this` before its binder call; [focusListener] copies it into
     * [focusCallbackSeq] under `this` on every callback. So when the GRANTED bookkeeping runs and
     * sees `focusCallbackSeq >= seq`, a callback for THIS request has already landed and is newer
     * than the grant — its hold and duck stand, and the grant must not overwrite them with GAIN.
     * Without it the consume thread could clear `pausedForFocus` and `play()` after the main looper
     * had already paused for a LOSS_TRANSIENT. Both are read and written only under `this`.
     */
    private var focusRequestSeq = 0
    private var focusCallbackSeq = -1
    @Volatile private var pausedForAssistant = false
    /** Kept DISTINCT from [pausedForAssistant]: with one flag, a focus GAIN mid-Siri (or Siri ending
     *  under a focus loss) resumes audio the other holder still wants stopped. */
    @Volatile private var pausedForFocus = false
    /** Last focus state seen, so every transition logs old -> new and a wrong mapping is diagnosable
     *  from a capture alone. Written only under `this` (the listener and [requestFocus]), because
     *  [requestFocus] decides from it whether a callback beat the grant (see [focusRequestSeq]). */
    @Volatile private var focusState = android.media.AudioManager.AUDIOFOCUS_GAIN
    /** Rate limit for [reclaimFocus]; a refusing head unit must not be asked once per frame. */
    @Volatile private var lastFocusRetryAt = 0L

    /**
     * Pause playback outright while Siri speaks, instead of only ducking.
     *
     * Ducking is not enough for the head unit's volume knob: AAOS chooses the knob's target from
     * *active players*, a ducked track is still active, and MUSIC outranks VOICE_COMMAND in this
     * unit's priority list — so the knob showed "Audio" even mid-Siri. Pausing removes MUSIC from the
     * active set and lets the voice group win. See VoiceRouter's class docs for the decompiled detail.
     *
     * `pause()` without `flush()`: the PCM already in the track survives, so resume plays it out
     * before anything new. CORRECTED 2026-09-10 — this used to claim the consume loop's writes "block
     * harmlessly until we resume". They do not block at all: `AudioTrack.write` goes NON-blocking on
     * a paused track and returns 0 or a short count at once, so everything the phone delivers during
     * the hold is decoded and thrown away (counted in [framesDiscardedPaused]). What the driver hears
     * on resume is up to [TRACK_BUFFER_MS] of pre-hold audio, then whatever the phone sends AFTER the
     * hold — the audio sent during it is gone. That is the right trade (the phone's playhead has
     * moved on; queueing would replay the hold's length), but it is not "continues where it left
     * off", and the consume thread is parked in the seam `read()`, never in `write()`, while held.
     */
    @Synchronized
    fun setAssistantSpeaking(speaking: Boolean) {
        if (pausedForAssistant == speaking) return
        pausedForAssistant = speaking
        applyPauseState("the assistant")
    }

    /**
     * The single place the track's play/pause state is derived: it plays only when NEITHER hold is
     * set, so the assistant and focus resume paths cannot mask each other.
     *
     * SYNCHRONIZED with the flag writes, and that pairing is the point — reading a flag and acting on
     * it is a check-then-act across two threads. The assistant edge runs on `cp-voice-sweep` and the
     * focus callback on the main looper, and once the ASSISTANT idle window was shortened to just past
     * ASSISTANT_HOLD_MS those two edges land ~1 s apart instead of ~12 s. Interleaved, the sweep thread
     * could clear `pausedForAssistant`, read `(false, true)` and be about to pause, while the looper
     * clears `pausedForFocus`, reads `(false, false)` and plays — leaving the stale pause to land last,
     * with a paused track, both flags false, and nothing scheduled to re-evaluate until the next Siri
     * turn. `AudioTrack.play`/`pause` are cheap, so holding the lock across them costs nothing.
     *
     * The log names the TRIGGER, not the hold that won; both are printed so a pause attributed to the
     * assistant while focus is the real cause can no longer be misread (device log 2026-09-08 showed
     * "assistant done — media resumes" and "media paused for the assistant" in the same millisecond).
     *
     * MUST be called with `this` held. Every caller is: [setAssistantSpeaking] (`@Synchronized`),
     * [focusListener] and the GRANTED block of [requestFocus]. Until 2026-09-10 [reclaimFocus] called
     * it bare after an unsynchronized `pausedForFocus = false`, which was the mirror image of the
     * race above: a LOSS_TRANSIENT pausing on the looper between the consume thread's flag read and
     * its `play()`, leaving the track playing over the new holder with `pausedForFocus == true`.
     *
     * The `play()` branch is gated on [running]: after [stop] has paused and flushed the track, a
     * late AUDIOFOCUS_GAIN or assistant-done edge must not put a discarded player's track back into
     * AAOS's active set — [stop] has already abandoned focus, so the volume knob would follow a track
     * nothing is feeding until the consume thread's [releaseAv] catches up.
     */
    private fun applyPauseState(reason: String) {
        val t = track ?: return
        val held = pausedForAssistant || pausedForFocus
        runCatching {
            if (held) {
                t.pause()
                log.i("media paused for $reason (assistant=$pausedForAssistant focus=$pausedForFocus)")
                SessionTrace.Board.up(BOARD_TRACK, trackDetail(t, "PAUSED for $reason"))
            } else if (!running.get()) {
                log.i("$reason would resume media, but stop() has run — leaving the track paused")
            } else {
                t.play(); log.i("media resumed after $reason")
                SessionTrace.Board.up(BOARD_TRACK, trackDetail(t, "PLAYING"))
            }
        }.onFailure { log.w("$reason pause/resume: ${it.javaClass.simpleName}: ${it.message}") }
    }

    /** One line of standing state for the [SessionTrace.Board]: format, real depth, play state. */
    private fun trackDetail(t: AudioTrack, state: String): String = runCatching {
        val ms = t.bufferSizeInFrames.toLong() * 1000 / t.sampleRate
        "${t.sampleRate}Hz ${t.channelCount}ch ${ms}ms $state"
    }.getOrElse { "(track unreadable) $state" }

    /** Underruns net of the fill baseline; see [underrunBaseline] and [UNDERRUN_TOLERANCE]. */
    private fun netUnderruns(u: Int): Int = if (u < 0) 0 else (u - underrunBaseline.coerceAtLeast(0)).coerceAtLeast(0)

    fun start() {
        configureFailedAt = 0L
        running.set(true)
        requestFocus()
    }

    /**
     * Take permanent media focus. Idempotent — [start] may be called again on a re-launched screen.
     *
     * Deliberately NOT tied to whether audio is currently flowing: the point is to be the resting focus
     * owner that AAOS returns to when Siri or a call abandons its transient focus. Releasing it between
     * tracks would recreate the stuck-knob bug in the gaps.
     *
     * # Two windows closed 2026-09-10 (both were in the build on the truck)
     *
     * **Grant vs. callback.** The bookkeeping after GRANTED (`focus`, `focusState`, the
     * `pausedForFocus` hold and its `play()`) runs in ONE `synchronized(this)` block, and it defers to
     * any callback that has already landed for this request ([focusRequestSeq]). The old code wrote
     * `pausedForFocus = false` bare and left [reclaimFocus] to `applyPauseState` bare; the looper's
     * LOSS_TRANSIENT could pause in between and the consume thread's `play()` landed last. The
     * remaining orderings are all consistent: a callback that lands AFTER this block is newer and
     * simply wins; one that landed BEFORE it is detected by the stamp, and if it was a permanent LOSS
     * the listener's [abandonFocus] found `focus == null` and abandoned nothing, so the request is
     * abandoned here instead of being cached as held (which would have disabled [reclaimFocus] —
     * `focus != null` — for the rest of the session).
     *
     * **Grant vs. teardown.** [stop] sets `running=false` under `this` and then abandons. A request
     * that was in flight when it did (the [feed] -> [reclaimFocus] path, which is not otherwise gated
     * on teardown) is refused before the binder call, or abandoned at once after it if `running` went
     * false while it was out — because `running.set(false)` and this block share the monitor, either
     * the grant is cached before [stop] runs (and its [abandonFocus] releases it) or this block sees
     * `running == false`. A player that [CarPlayActivity] has already discarded must not end up
     * holding `AUDIOFOCUS_GAIN` with a live listener: AAOS returns focus to the TOP of its stack, and
     * a zombie on top would mean the next real player never gets its GAIN back.
     *
     * The focus-duck clear that used to live here moved into [abandonFocus] — "no focus held means no
     * focus duck" is that function's invariant, and clearing here raced a CAN_DUCK callback the same
     * way the hold did. Lock order: [setFocusDucked] takes `duckLock`, so it is never called under
     * `this` — see [publishTrack].
     */
    private fun requestFocus() {
        val mgr = am ?: run { log.w("no AudioManager — media will not hold focus; the volume knob may stick on Phone/Siri"); return }
        val seq = synchronized(this) {
            if (focus != null) return
            if (!running.get()) { log.i("media focus not re-requested — stop() has run"); return }
            ++focusRequestSeq
        }
        runCatching {
            val req = android.media.AudioFocusRequest.Builder(android.media.AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build()
                )
                // We already duck in software from VoiceRouter, and CarPlay expects the phone to remain
                // the mixing authority — so do not let the system pause us behind our back.
                .setWillPauseWhenDucked(false)
                .setOnAudioFocusChangeListener(focusListener)
                .build()
            val r = mgr.requestAudioFocus(req)
            // Cache ONLY a request that actually took. Caching on AUDIOFOCUS_REQUEST_FAILED made the
            // `focus != null` guard above permanent — no later start() ever retried, and abandonFocus()
            // then abandoned a request we never held.
            // GRANTED only. DELAYED cannot occur — the builder never calls setAcceptsDelayedFocusGain —
            // and treating it as held would be wrong anyway: a DELAYED grant means focus is NOT yet
            // held, so clearing pausedForFocus and unducking on it would play over the current holder.
            if (r != android.media.AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                // E, not W: with no resting owner AAOS has nothing to hand focus back to after Siri or
                // a call, so the knob sticks on Phone/Siri for the session. Nothing retries this until
                // the screen is relaunched (start()) or a permanent LOSS makes reclaimFocus() eligible.
                log.e("media audio focus REQUEST_FAILED (result=$r) — not cached; no retry until the next start()")
                // note(), not failed(): the E above is the severity; a second E would double-count.
                SessionTrace.Board.note(BOARD_FOCUS, "FAILED (AAOS refused AUDIOFOCUS_GAIN result=$r — no resting owner, the volume knob can stick on Phone/Siri)")
                return
            }
            // Non-null means "granted, but do not keep it": the reason is logged and the request is
            // abandoned OUTSIDE the monitor (a binder call, and nothing here may nest a lock).
            var orphanReason: String? = null
            synchronized(this) {
                val landed = focusCallbackSeq >= seq
                when {
                    !running.get() -> orphanReason = "stop() ran while the request was in flight"
                    landed && focusState == android.media.AudioManager.AUDIOFOCUS_LOSS ->
                        orphanReason = "a permanent LOSS landed for it first; its listener had nothing to abandon"
                    landed -> {
                        // Newer than the grant: its hold and duck are already applied and stand.
                        focus = req
                        log.i("media audio focus: GRANTED, but ${focusName(focusState)} landed first " +
                              "for this request — keeping its hold (paused=$pausedForFocus)")
                    }
                    else -> {
                        focus = req
                        focusState = android.media.AudioManager.AUDIOFOCUS_GAIN
                        if (pausedForFocus) { pausedForFocus = false; applyPauseState("focus regained") }
                        log.i("media audio focus: GRANTED")
                    }
                }
            }
            orphanReason?.let { why ->
                runCatching { mgr.abandonAudioFocusRequest(req) }   // deliberate: discarding a request we will not keep
                log.w("media audio focus: GRANTED but abandoned at once — $why")
            }
            if (orphanReason == null) SessionTrace.Board.up(BOARD_FOCUS, "${focusName(focusState)} — AUDIOFOCUS_GAIN held (USAGE_MEDIA)")
        }.onFailure { log.w("media focus request failed: ${it.javaClass.simpleName}: ${it.message}") }
    }

    /**
     * Re-take media focus after a PERMANENT loss, so media is not silent for the rest of the session.
     *
     * # The bug this fixes
     *
     * `AUDIOFOCUS_LOSS` sets `pausedForFocus` and then calls [abandonFocus]. Abandoning means the
     * listener can never be called again — there is no focus left to gain — so `pausedForFocus`
     * stayed true FOREVER and the media track never played again. The comment used to claim "the
     * next start() re-requests", but [start] runs only when the CarPlay screen is (re)launched, not
     * while a session continues. So a single permanent loss — GM's own media app, a navigation
     * prompt, a phone call that takes focus outright — killed CarPlay media for the whole session.
     *
     * The tell is precise, and is how this was reported: **Siri and phone calls are still audible
     * while media is silent.** Those run through [VoiceRouter] on a different track and never touch
     * this hold, so every other symptom looks healthy. Device-reported 2026-08-28.
     *
     * # Why here
     *
     * Called from [feed], i.e. only when audio is actually arriving from the phone. That is the
     * honest signal that media is *meant* to be playing — polling a timer would re-take focus during
     * a deliberate pause and steal it from whatever the driver is actually listening to.
     *
     * Rate-limited because a focus request is a binder call on the audio path, and a head unit that
     * is refusing focus will keep refusing it: one attempt per [FOCUS_RETRY_MS], not one per frame.
     *
     * # How often this really runs (settled 2026-09-10)
     *
     * It was proposed that the consume thread parks in the blocking `AudioTrack.write` for the whole
     * hold, so [feed] is never re-entered and this gets ONE attempt. That is not how AudioTrack
     * behaves: on a paused track `write` returns short without parking (see
     * [framesDiscardedPaused]), so the loop keeps turning at the phone's delivery rate and the retry
     * cadence promised above holds — as long as the phone is delivering. When the phone stops
     * sending (it ends its media stream for Siri and starts a new one afterwards, per the 2026-09-09
     * capture), the thread parks in the seam `read()` and there are no attempts, which is the right
     * outcome: no media arriving is exactly the signal that it is not meant to be playing. The one
     * true limit is the refusal case: a head unit that refuses every request keeps media silent for
     * as long as it refuses, one attempt per [FOCUS_RETRY_MS].
     *
     * The hold is cleared and the track resumed by [requestFocus] itself, under `this`; this used to
     * do both bare afterwards, which is the race described on [applyPauseState].
     */
    private fun reclaimFocus() {
        val now = android.os.SystemClock.elapsedRealtime()
        if (now - lastFocusRetryAt < FOCUS_RETRY_MS) return
        lastFocusRetryAt = now
        log.i("media is flowing but focus was permanently lost — re-requesting")
        requestFocus()
    }

    /**
     * Act on focus, do not merely hold it. This head unit ENFORCES focus — `dumpsys car_service`
     * reports `Use hal ducking signals true`, and we were handed a LOSS_TRANSIENT while continuing to
     * play at full gain.
     *
     * Nothing here tears the session down, on any transition: CarPlay expects the phone to stay the
     * mixing authority, so even a permanent LOSS only pauses, unducks and abandons — the seam, the
     * decoder and the track stay live and [reclaimFocus] re-requests when media flows again.
     *
     * Shape: the stamp ([focusCallbackSeq]), the state transition and the hold it implies are ONE
     * `synchronized(this)` block, so [requestFocus]'s GRANTED block sees either none of this callback
     * or all of it. Duck changes and [abandonFocus] stay outside the monitor — both take `duckLock`
     * (see [publishTrack] for the never-nested rule).
     */
    private val focusListener = android.media.AudioManager.OnAudioFocusChangeListener { change ->
        // Un-duck BEFORE the resume below, so the track never plays at DUCK_GAIN for even a period.
        // Clears ONLY the focus duck. A voice duck the energy gate still holds stays in force (see
        // [setVoiceDucked]); before 2026-09-09 this call restored unity over a live prompt.
        if (change == android.media.AudioManager.AUDIOFOCUS_GAIN) setFocusDucked(false)
        synchronized(this@AacPlayer) {
            focusCallbackSeq = focusRequestSeq
            log.i("media focus ${focusName(focusState)} -> ${focusName(change)}")
            focusState = change
            if (change != android.media.AudioManager.AUDIOFOCUS_LOSS) {
                SessionTrace.Board.up(BOARD_FOCUS, "${focusName(change)} — AUDIOFOCUS_GAIN held (USAGE_MEDIA)")
            }
            when (change) {
                android.media.AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> { pausedForFocus = true; applyPauseState("focus loss") }
                android.media.AudioManager.AUDIOFOCUS_GAIN -> { pausedForFocus = false; applyPauseState("focus gain") }
                android.media.AudioManager.AUDIOFOCUS_LOSS -> { pausedForFocus = true; applyPauseState("permanent focus loss") }
                // CAN_DUCK changes no hold; it is ducked below, outside the monitor.
            }
        }
        when (change) {
            android.media.AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> setFocusDucked(true)
            android.media.AudioManager.AUDIOFOCUS_LOSS -> {
                // Also clears the focus duck: without that a CAN_DUCK followed by LOSS resumed via
                // [reclaimFocus] at DUCK_GAIN for the rest of the session.
                abandonFocus("permanent LOSS — abandoned; re-requested by reclaimFocus() when media flows again")
                // NOT "the next start() re-requests" — see [reclaimFocus] for why that was a lie and
                // what actually recovers it now.
                log.e("media focus LOST for good — paused and abandoned; will re-request when media resumes")
            }
        }
    }

    private fun focusName(c: Int) = when (c) {
        android.media.AudioManager.AUDIOFOCUS_GAIN -> "GAIN"
        android.media.AudioManager.AUDIOFOCUS_LOSS -> "LOSS"
        android.media.AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> "LOSS_TRANSIENT"
        android.media.AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> "LOSS_TRANSIENT_CAN_DUCK"
        else -> "state=$c"
    }

    /** Release focus, and with it the focus-derived duck: no focus held means no focus duck, and a
     *  synchronously GRANTED re-request gets no GAIN callback that would clear one. Both the LOSS
     *  handler and [requestFocus] used to clear it themselves; centralised here 2026-09-10 so the
     *  grant path no longer races a CAN_DUCK callback. Takes `duckLock` via [setFocusDucked] — never
     *  call this under `this`. Callers: [stop], the LOSS branch of [focusListener]. [why] is what the
     *  [SessionTrace.Board] shows for the focus entry from here on. */
    private fun abandonFocus(why: String) {
        // deliberate: abandoning a request AAOS may already have dropped; nothing to do if it throws.
        am?.let { mgr -> focus?.let { runCatching { mgr.abandonAudioFocusRequest(it) } } }
        focus = null
        setFocusDucked(false)
        SessionTrace.Board.down(BOARD_FOCUS, why)
    }

    /**
     * Build the [AudioTrack] before any audio arrives, so the first ADTS frame is decoded straight
     * into a track that is already playing.
     *
     * Safe to do speculatively because the format is not a guess: CarPlay's audio ceiling is a
     * WIRE-FORMAT limit of stereo AAC-LC 48 kHz (`kAirPlayAudioFormat_*` has no entry above 2
     * channels), the SETUP dict negotiates exactly that, and every output thread on this head unit
     * reports 48 kHz stereo. The decoder is deliberately NOT pre-configured — [configure] still
     * builds it from the first frame's own ADTS header, so a surprise rate still works; it simply
     * reuses this track instead of building one.
     *
     * Measured saving: ~134 ms of the 172 ms seam-connect -> first-audio path (2026-08-12).
     */
    fun prime() {
        if (track != null) return
        // Goes through [publishTrack] like every other builder. Today this path cannot race a hold or
        // a duck edge by construction — prime() runs on the main looper before VoiceRouter exists and
        // before any focus request is made — so the helper is here for a later reordering, not for a
        // defect this path has.
        runCatching { buildTrack(cfgRate, cfgChannels) }
            .onSuccess { publishTrack(it); log.i("primed AudioTrack ${cfgRate}Hz ${cfgChannels}ch — waiting for the stream") }
            .onFailure { log.w("prime failed (harmless; configure will build one): ${it.message}") }
    }

    /**
     * Duck the media track. Only MEDIA is ever ducked — every other purpose plays at unity — and the
     * effective gain is min(voiceDuck, focusDuck): the track sits at [DUCK_GAIN] while EITHER source
     * wants it ducked and returns to unity only when NEITHER does.
     *
     * Two sources, two entry points, because two independent things legitimately ask and a caller
     * must not be able to clear the other one's duck: VoiceRouter's energy gate ([setVoiceDucked] — a
     * nav prompt or Siri turn is audibly loud on the voice track) and this player's own focus listener
     * ([setFocusDucked] — AAOS handed us LOSS_TRANSIENT_CAN_DUCK). Until 2026-09-09 both wrote one
     * shared boolean, last writer wins: an AUDIOFOCUS_GAIN landing mid-prompt restored media to unity
     * over the prompt, and the gate's un-duck at the end of a prompt cancelled a focus duck AAOS had
     * not lifted. The KDoc promised min() from the start; it was never implemented.
     *
     * Each entry point is idempotent per SOURCE (VoiceRouter re-asserts `onDuck(false)` at 1 Hz from
     * its idle sweep), and a source edge that does not change the effective gain — the other source
     * still holds — is logged once, not applied, so a capture shows which source is holding the duck.
     */
    fun setVoiceDucked(ducked: Boolean) = setDucked(DuckSource.VOICE, ducked)
    fun setFocusDucked(ducked: Boolean) = setDucked(DuckSource.FOCUS, ducked)

    private enum class DuckSource { VOICE, FOCUS }

    private fun setDucked(source: DuckSource, ducked: Boolean) {
        // synchronized, not a volatile compare-and-set: this is called from the voice decode thread,
        // the main looper (focus callback) AND the UI thread (teardown). The read-compare-write-apply
        // sequence being non-atomic let an interleaving leave duckGain=1.0 with the hardware at 0.2 —
        // after which every later un-duck early-returns and the duck is unrecoverable for the life of
        // the player, while the log claims it was restored. The lock also makes the two source flags
        // and the gain derived from them a single atomic state, which is what the min() relies on.
        synchronized(duckLock) {
            when (source) {
                DuckSource.VOICE -> { if (voiceDuck == ducked) return; voiceDuck = ducked }
                DuckSource.FOCUS -> { if (focusDuck == ducked) return; focusDuck = ducked }
            }
            val g = if (voiceDuck || focusDuck) DUCK_GAIN else 1.0f
            if (g == duckGain) {
                log.i("media duck: $source ${if (ducked) "on" else "off"}, gain stays $g " +
                      "(voice=$voiceDuck focus=$focusDuck)")
                return
            }
            duckGain = g
            // A refused setVolume used to be swallowed while the next line claimed the duck was
            // applied — media at full gain over Siri, with a log saying otherwise. Per edge, not per
            // frame, so a W here is cheap.
            runCatching { track?.setVolume(g) }
                .onFailure { log.w("setVolume($g) refused: ${it.javaClass.simpleName}: ${it.message} — the duck below was NOT applied") }
            log.i("media ${if (g < 1.0f) "ducked to $g" else "restored to 1.0"} by $source " +
                  "(voice=$voiceDuck focus=$focusDuck)")
        }
    }

    /** Re-assert the current gain onto a freshly built track. Without this a track created while a
     *  duck was in flight starts at unity and the duck is silently lost. Reads the EFFECTIVE gain,
     *  under the same lock, so it is the min() of both sources at that instant. */
    private fun applyGain(t: AudioTrack) {
        synchronized(duckLock) {
            runCatching { t.setVolume(duckGain) }
                .onFailure { log.w("re-asserting gain $duckGain on a new track refused: ${it.message} — it plays at unity") }
        }
    }

    private val duckLock = Any()
    // All three are written only under duckLock — and READ only under it too ([setDucked],
    // [applyGain]), so none needs @Volatile. duckGain is the min() of the two source flags and
    // is the ONLY value ever pushed to a track.
    private var voiceDuck = false
    private var focusDuck = false
    private var duckGain = 1.0f

    /** The ONLY way a built track becomes [track]. Publish FIRST, then re-derive gain and hold state
     *  from the shared flags: an edge that landed before the publish saw `track == null` and pushed
     *  nothing, so the re-derivation picks it up; an edge after the publish pushes to the live track
     *  itself. Deriving BEFORE publishing (buildTrack used to) lost any edge in between — for gain a
     *  stale volume, for the hold a track paused by the builder while a concurrent AUDIOFOCUS_GAIN
     *  ran [applyPauseState], hit `track ?: return`, and left it paused with both flags false.
     *
     *  Lock order: `duckLock` (inside [applyGain]) is released before `this` is taken; the two are
     *  never nested, here or anywhere else. Callers ([prime], [configure], the [feed] rebuild) hold
     *  neither. The full list of `duckLock` takers is [setDucked], [applyGain] and — since 2026-09-10
     *  — [abandonFocus]; none is called under `this`, and [requestFocus]'s and [focusListener]'s
     *  `this` blocks call none of them. No `else t.play()`: [buildTrack] already called `play()`, and an else here would
     *  resume a track [stop] deliberately paused. */
    private fun publishTrack(t: AudioTrack) {
        track = t
        applyGain(t)                                      // duckLock, released before the next line
        var state = "PLAYING"
        synchronized(this) {                              // same monitor as the flag writers
            if (pausedForAssistant || pausedForFocus) {
                runCatching { t.pause() }
                    .onFailure { log.w("new track pause() refused: ${it.message} — it is PLAYING through a hold") }
                log.i("new track starts paused (assistant=$pausedForAssistant focus=$pausedForFocus)")
                state = "PAUSED (assistant=$pausedForAssistant focus=$pausedForFocus)"
            }
        }
        SessionTrace.Board.up(BOARD_TRACK, trackDetail(t, state))
    }

    /**
     * Flag-only teardown, plus the one cross-thread call audio genuinely needs.
     *
     * It releases NOTHING: [consume] owns both resources and releases them on its own thread. Releasing
     * a MediaCodec from here races a live `feed()` — best case a message-less IllegalStateException,
     * worst case a native crash, since `release()` unmaps the direct input ByteBuffer `feed()` may be
     * writing into. And nulling the fields here is exactly what let the still-running loop observe
     * `codec == null` and rebuild a codec + playing track that nothing would ever free.
     *
     * `pause()`+`flush()` ARE safe cross-thread (AudioTrack is thread-safe for these) and are the audio
     * analogue of the caller closing the socket: a socket close unblocks a consume thread parked in
     * `read()`, but never one parked in the blocking `write()`. `pause()` interrupts the ONE write that
     * may be in flight (native `AudioTrack::pause` -> `proxy->interrupt()`, the write returns short);
     * `flush()` then drops queued PCM so nothing plays out after stop. CORRECTED 2026-09-10: this used
     * to model the consume thread as parked in `write()` for as long as the track stayed paused. It
     * never is — once paused, every later `write` returns short at once instead of parking (see
     * [framesDiscardedPaused]), and the loop leaves on its next `running` check in [processAdts].
     * The pause is still required for the in-flight write; it is just not a standing property.
     *
     * Silence the track BEFORE abandoning focus — the same order as `VoiceRouter.Sink.release()`
     * (pause → flush → stop → release, then abandon). AAOS hands focus to the next owner the moment we
     * abandon; until 2026-09-09 that came first, so for the window before `pause()` landed we were
     * still writing PCM at full gain while another app had already been told it owned focus.
     *
     * `running=false` is written under `this` so that [requestFocus]'s GRANTED block, which reads it
     * under the same monitor, cannot cache a grant after this has decided there is nothing to
     * abandon (see the "grant vs. teardown" window on [requestFocus]). [abandonFocus] itself stays
     * outside the monitor — it takes `duckLock`, which is never nested with `this`.
     *
     * The summary is JUDGED: `E` if any decoded PCM reached no live track ([framesDroppedNoTrack] —
     * that is silent media), `W` if underruns net of the fill baseline exceed [UNDERRUN_TOLERANCE]
     * (audible gaps), `I` only when clean. [framesDiscardedPaused] is NOT a fault and never lifts the
     * severity — it is the Siri hold working as designed. [why] is the caller's reason.
     */
    fun stop(why: String) {
        val hadConsumer = consumeStarted
        // Sample before the release below nulls the track; -1 means "no track to ask".
        val underruns = runCatching { track?.underrunCount }.getOrNull() ?: -1
        synchronized(this) { running.set(false) }
        // deliberate: silencing a track we are discarding; the consume thread's releaseAv() owns the rest.
        runCatching { track?.pause() }
        runCatching { track?.flush() }
        // If consume() never ran, nothing else will EVER release the primed track: releaseAv() is
        // reachable only from the consume thread. That happens on every path where :9002 never
        // connected — a bind failure, or a session torn down before the producer dialled — and
        // AudioTrack instances come from a small global pool, so a retry loop exhausts it and every
        // later build() in the process throws.
        if (!hadConsumer) {
            val t = track; track = null
            runCatching { t?.stop() }; runCatching { t?.release() }   // deliberate: teardown
            log.i("released the primed track (the media seam never connected)")
        }
        // Last, once no track of ours is still playing (or, with a consumer, is paused and flushed —
        // its release follows on the consume thread, which `running=false` has already told to stop
        // writing).
        abandonFocus("stopped — $why")
        val dropped = framesDroppedNoTrack.get()
        val net = netUnderruns(underruns)
        val msg = "stopping ($why) — ${framesDecoded.get()} frames played, ${framesDiscardedPaused.get()} discarded " +
                  "(paused), $dropped dropped (no track), $underruns underruns ($net net of the fill baseline), " +
                  "${bytesIn.get()} bytes"
        when {
            dropped > 0 -> log.e("$msg — decoded media reached NO live track: that was silent media")
            net > UNDERRUN_TOLERANCE -> log.w("$msg — each net underrun is an audible gap unless it coincides with a `[rust] stream 102 ended`")
            else -> log.i(msg)
        }
        SessionTrace.Board.down(BOARD_TRACK, "stopped — $why")
    }

    /** Set on the consume thread's entry; read by [stop] to decide whether the primed track has an owner. */
    @Volatile private var consumeStarted = false

    /** Consume ADTS off the seam until it closes. Blocking; call on its own thread. */
    fun consume(ins: InputStream) {
        consumeStarted = true
        firstPcmThisConn = false
        // Per connection: forward.rs dials only once it holds an ADTS frame, so a connection with no
        // decoded PCM behind it is a decoder fault, not an idle phone. Met in [feed] on the first
        // output buffer of any outcome; cancelled below if the connection ends first.
        SessionTrace.expect(EXPECT_FIRST_PCM, FIRST_PCM_MS,
            "the media seam :9002 connected, and the producer dials only with an ADTS frame in hand, " +
            "so the AAC-LC decoder should configure and emit PCM at once (176ms on 2026-09-09)")
        val buf = ByteArray(32 * 1024)
        val acc = java.io.ByteArrayOutputStream(64 * 1024)
        try {
            while (running.get()) {
                // A SocketException here during teardown is the caller's deliberate close, not a fault.
                val n = try { ins.read(buf) } catch (e: Exception) {
                    if (running.get()) log.e("read: ${e.message}"); break
                }
                if (n <= 0) break
                bytesIn.addAndGet(n.toLong())
                acc.write(buf, 0, n)
                val data = acc.toByteArray()
                val used = processAdts(data)
                acc.reset()
                if (used < data.size) acc.write(data, used, data.size - used)
            }
        } finally {
            releaseAv()
            if (!firstPcmThisConn) {
                SessionTrace.cancel(EXPECT_FIRST_PCM,
                    if (running.get()) "the producer dropped the media connection first" else "player stopped")
            }
            if (running.get()) {
                // Mid-session drop: the track went with the codec and both rebuild on the re-dial.
                SessionTrace.Board.down(BOARD_TRACK, "media seam connection ended — released; rebuilt when the producer re-dials")
            }
            log.i("seam ended — ${framesDecoded.get()} frames decoded")
        }
    }

    /**
     * The ONLY place codec/track are torn down, and only ever on the consume thread.
     *
     * Both fields are nulled: `CarPlayActivity.serve()` calls `consume()` again on this same instance
     * every time the producer re-dials the seam, and a non-null released codec would make the next
     * connection feed a corpse — an exception per frame and permanently dead audio after the first
     * transient reconnect. Each call is independently wrapped: with two resources, a throw releasing
     * the codec must not skip the track. Order per resource is stop-then-release — the authority never
     * bare-releases, and a bare release can click on some HALs.
     */
    private fun releaseAv() {
        // deliberate, all six: teardown of resources being discarded, each wrapped on its own so a
        // throw from one never skips the next (see the KDoc).
        val c = codec; codec = null
        runCatching { c?.stop() }
        runCatching { c?.release() }
        val t = track; track = null
        runCatching { t?.pause() }
        runCatching { t?.flush() }
        runCatching { t?.stop() }
        runCatching { t?.release() }
    }

    /** Walk complete ADTS frames; returns how many bytes were consumed. */
    private fun processAdts(data: ByteArray): Int {
        var i = 0
        while (i + 7 <= data.size) {
            // stop() may have run mid-buffer: a full 32 KB read holds many frames, and continuing to
            // decode+write them into a paused/flushed track is the write-after-stop deadlock. Bail.
            if (!running.get()) return i
            if ((data[i].toInt() and 0xFF) != 0xFF || (data[i + 1].toInt() and 0xF0) != 0xF0) { i++; continue }
            val frameLen = ((data[i + 3].toInt() and 0x03) shl 11) or
                           ((data[i + 4].toInt() and 0xFF) shl 3) or
                           ((data[i + 5].toInt() and 0xE0) ushr 5)
            if (frameLen < 7) { i++; continue }
            if (i + frameLen > data.size) return i          // incomplete tail — keep it
            if (codec == null) {
                if (!running.get()) return i   // tearing down — don't configure just to release
                val b2 = data[i + 2].toInt() and 0xFF
                val rateIdx = (b2 shr 2) and 0x0F
                val ch = ((b2 and 0x01) shl 2) or ((data[i + 3].toInt() and 0xC0) ushr 6)
                configure(if (rateIdx < rates.size) rates[rateIdx] else 48000, if (ch in 1..8) ch else 2)
            }
            // Strip the 7-byte ADTS header: the codec is configured with csd-0, so it wants raw AAC.
            feed(data, i + 7, frameLen - 7)
            i += frameLen
        }
        return i
    }

    /**
     * All-or-nothing. The fields are assigned only once BOTH resources are live, so no path exists
     * where a started codec has no track (the old code assigned `codec` first, so a throw from the
     * AudioTrack builder stranded a started decoder with no track, no retry and no release owner).
     * Failure releases both locals and backs off — without the backoff this retried on every ADTS
     * frame and drained the global codec pool in seconds.
     */
    private fun configure(sampleRate: Int, channels: Int) {
        val now = android.os.SystemClock.elapsedRealtime()
        if (configureFailedAt != 0L && now - configureFailedAt < CONFIGURE_RETRY_MS) return
        var c: MediaCodec? = null
        var t: AudioTrack? = null
        try {
            val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, sampleRate, channels)
            fmt.setInteger(MediaFormat.KEY_AAC_PROFILE, android.media.MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            // csd-0 for raw AAC: 5 bits objectType(2=LC) | 4 bits rateIdx | 4 bits channelCfg.
            val rateIdx = rates.indexOf(sampleRate).let { if (it < 0) 3 else it }
            val csd = byteArrayOf(
                (((2 shl 3) or (rateIdx shr 1)) and 0xFF).toByte(),
                ((((rateIdx and 1) shl 7) or (channels shl 3)) and 0xFF).toByte()
            )
            fmt.setByteBuffer("csd-0", ByteBuffer.wrap(csd))
            c = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
            c.configure(fmt, null, null, 0)
            c.start()

            // Adopt the primed track when the stream matches what was primed (the normal case);
            // otherwise discard it and build for what actually arrived — a primed track must never
            // silently impose the wrong rate or channel count on the stream.
            val primed = track
            val built: AudioTrack = if (primed != null && sampleRate == cfgRate && channels == cfgChannels) {
                log.i("adopting the primed AudioTrack")
                primed
            } else {
                if (primed != null) {
                    log.i("primed track was ${cfgRate}Hz ${cfgChannels}ch but the stream is " +
                          "${sampleRate}Hz ${channels}ch — rebuilding")
                    track = null
                    runCatching { primed.pause() }; runCatching { primed.flush() }
                    runCatching { primed.stop() }; runCatching { primed.release() }
                }
                buildTrack(sampleRate, channels)
            }
            t = built

            codec = c
            // Idempotent on an adopted primed track: it re-derives the same gain and hold it already has.
            publishTrack(built)
            cfgRate = sampleRate
            cfgChannels = channels
            configureFailedAt = 0L
            log.i("configured AAC-LC ${sampleRate}Hz ${channels}ch → AudioTrack (USAGE_MEDIA), decoder=${c.name}")
        } catch (e: Throwable) {
            // Throwable, not Exception, for the same reason as HevcRenderer.maybeConfigure: an
            // OutOfMemoryError while the framework allocates codec or track buffers is an Error, and
            // letting it escape strands whatever got built before the throw — before `codec = c` above,
            // the codec only; after it, both fields may point at what this catch releases, so null
            // them too or the next feed() dereferences a released codec/track (self-heals via the
            // resulting IllegalStateException, but there is no reason to leave the dangling reference).
            runCatching { c?.release() }
            runCatching { t?.release() }
            if (codec === c) codec = null
            if (track === t) track = null
            configureFailedAt = now
            log.e("configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
        }
    }

    /** Track construction, shared by [prime], [configure] and the ERROR_DEAD_OBJECT rebuild in [feed].
     *
     *  Returns an UNPUBLISHED, playing track at unity gain. The caller MUST hand it to [publishTrack];
     *  nothing here reads the duck or hold flags, because doing so before the publish is exactly the
     *  window that lost edges (see [publishTrack]).
     *
     *  # Buffer depth: [TRACK_BUFFER_MS] of audio, floored at 2 × `getMinBufferSize` (2026-09-10)
     *
     *  Was `maxOf(minBuf, 8192) * 2` — 85–160 ms at 48 kHz stereo, rate-blind, and smaller than a
     *  single delivery gap from the phone; the 2026-09-09 truck capture paid 6 underruns for it, 5 of
     *  them in the first 9 s. The reasoning for the number is on the constant. What it COSTS, stated
     *  so nobody "optimises" it back down without re-measuring:
     *
     *  - **First sound waits for a full buffer.** AudioFlinger's `Track::isReady()` refuses to mix a
     *    freshly (re)added `MODE_STREAM` track until `framesReady() >= bufferSizeInFrames` — and
     *    `addTrack_l` resets that FILLING state on EVERY add, including a `play()` after `pause()`
     *    removed the track from the active list. With the phone pacing at realtime, that is
     *    ~[TRACK_BUFFER_MS] of wall clock from the first frame of a stream to the first audible one
     *    (was ~85–160 ms). After a Siri hold the buffer is normally still full from before the
     *    pause, so resume is immediate.
     *  - **A track skip replays what is buffered.** Up to [TRACK_BUFFER_MS] of the old song plays
     *    after the phone has moved on. Nothing upstream flushes on our behalf.
     *  - **One benign `AudioFlinger: BUFFER TIMEOUT: remove(N)` per stream start, followed by
     *    `AudioTrack: restartIfDisabled(...) disabled due to previous underrun, restarting`.** The
     *    mixer gives a filling track ~450 ms on this unit (prime at 22:51:14.443, timeout at
     *    22:51:14.899 in the capture) before parking it; the next `write` restarts it with its
     *    contents intact and the fill continues. That pair is now EXPECTED at every stream start and
     *    is not the underrun it is named after — `underrunCount` at the first played frame is logged
     *    so the two are not confused.
     *
     *  The log line here prints what was asked, what `getMinBufferSize` said, and what
     *  `bufferSizeInFrames` reports was actually granted — the server may round, and a capture must
     *  show the real depth rather than the requested one. */
    private fun buildTrack(sampleRate: Int, channels: Int): AudioTrack {
        val chMask = if (channels >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val minBuf = AudioTrack.getMinBufferSize(sampleRate, chMask, AudioFormat.ENCODING_PCM_16BIT)
        val frameBytes = 2 * (if (channels >= 2) 2 else 1)
        val wantBytes = (sampleRate.toLong() * TRACK_BUFFER_MS / 1000).toInt() * frameBytes
        // minBuf is negative on ERROR / ERROR_BAD_VALUE; maxOf then simply keeps the time-derived size.
        val bufBytes = maxOf(minBuf * 2, wantBytes)
        fun ms(bytes: Int) = if (bytes <= 0) -1L else bytes.toLong() / frameBytes * 1000 / sampleRate
        return AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(sampleRate)
                    .setChannelMask(chMask)
                    .build()
            )
            .setBufferSizeInBytes(bufBytes)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
            // `play()` can throw AFTER build() succeeded. `.also { it.play() }` would propagate before
            // the track is ever returned, so the caller's `t` is still null and its `t?.release()`
            // releases nothing — stranding a live native AudioTrack. Release it here, where it is
            // still in hand, then rethrow. (The caller's comment claimed this was handled; it was not.)
            .also { trk ->
                val granted = runCatching { trk.bufferSizeInFrames }.getOrNull() ?: -1
                log.i("AudioTrack ${sampleRate}Hz ${channels}ch buffer: asked $bufBytes B (${ms(bufBytes)} ms), " +
                      "granted $granted frames (${if (granted > 0) granted.toLong() * 1000 / sampleRate else -1} ms), " +
                      "getMinBufferSize=$minBuf B (${ms(minBuf)} ms)")
                try { trk.play() } catch (e: Throwable) { runCatching { trk.release() }; throw e }
            }
    }

    /** What became of one decoded output buffer in [feed]; exactly one counter moves per value. */
    private enum class WriteOutcome { NO_PCM, NO_TRACK, DISCARDED_PAUSED, PLAYED }

    private fun feed(data: ByteArray, off: Int, len: Int) {
        val c = codec ?: return
        if (len <= 0) return
        // Media is arriving, so the phone believes it is playing. If we are sitting on a
        // paused-for-focus hold with no focus request outstanding, retry it. See [reclaimFocus].
        if (pausedForFocus && focus == null) reclaimFocus()
        try {
            val inIdx = c.dequeueInputBuffer(5_000)
            if (inIdx >= 0) {
                // A dequeued index MUST be queued back on EVERY path, including the null-buffer
                // one. Input buffers are a fixed pool of 4-8; leaking them makes dequeueInputBuffer
                // return TRY_AGAIN_LATER forever — silent dead audio with a full timeout per frame.
                // Same fix as VoiceRouter; it had only been applied there.
                val ib = c.getInputBuffer(inIdx)
                if (ib == null) {
                    c.queueInputBuffer(inIdx, 0, 0, 0, 0)
                } else {
                    ib.clear()
                    if (ib.remaining() >= len) {
                        ib.put(data, off, len)
                        c.queueInputBuffer(inIdx, 0, len, System.nanoTime() / 1000, 0)
                    } else c.queueInputBuffer(inIdx, 0, 0, 0, 0)
                }
            }
            val info = MediaCodec.BufferInfo()
            while (true) {
                // Never issue the blocking AudioTrack.write below once stop() has paused the track —
                // that is the deadlock. Drain no further after teardown is requested.
                if (!running.get()) break
                val outIdx = c.dequeueOutputBuffer(info, 0)
                if (outIdx < 0) break
                // Exactly one of the three counters moves per output buffer, and only PLAYED means
                // the track took every byte. Was a boolean `written` set BEFORE the write, so any
                // non-negative return — including the 0 / short count a paused track answers with —
                // counted as played; see [framesDiscardedPaused] for the capture that exposed it.
                var outcome = WriteOutcome.NO_PCM
                c.getOutputBuffer(outIdx)?.let { ob ->
                    val pcm = ByteArray(info.size)
                    ob.position(info.offset); ob.get(pcm)
                    val t = track
                    outcome = if (t == null) WriteOutcome.NO_TRACK else {
                        val w = t.write(pcm, 0, pcm.size)
                        when {
                            w == AudioTrack.ERROR_DEAD_OBJECT -> {
                                // An audioserver restart or a route teardown invalidates the track.
                                // Every later write then fails silently while the decoder keeps
                                // running — audio is gone for the rest of the session with a single
                                // log line. Rebuild in place; this runs on the consume thread, the
                                // only legal owner.
                                log.e("AudioTrack ERROR_DEAD_OBJECT — rebuilding")
                                track = null
                                runCatching { t.release() }
                                runCatching { buildTrack(cfgRate, cfgChannels) }
                                    .onSuccess { publishTrack(it) }
                                    .onFailure { log.e("track rebuild failed: ${it.message}") }
                                WriteOutcome.NO_TRACK
                            }
                            w < 0 -> { log.e("AudioTrack.write returned $w"); WriteOutcome.NO_TRACK }
                            // A blocking write on an ACTIVE track returns only when every byte is in;
                            // anything less means the track was paused (or stopped) — before the
                            // write, or by the pause() that interrupted it — and the remainder is gone.
                            w < pcm.size -> WriteOutcome.DISCARDED_PAUSED
                            else -> WriteOutcome.PLAYED
                        }
                    }
                }
                c.releaseOutputBuffer(outIdx, false)
                if (outcome != WriteOutcome.NO_PCM && !firstPcmThisConn) {
                    // Per-connection latch: the decoder produced PCM, whatever the track did with it.
                    firstPcmThisConn = true
                    SessionTrace.met(EXPECT_FIRST_PCM)
                }
                if (outcome != WriteOutcome.PLAYED) {
                    // A null track — a prime that failed, or the rebuild above failing — used to
                    // count as played too, so "FIRST AUDIO FRAME PLAYED" printed with nothing playing.
                    when (outcome) {
                        WriteOutcome.NO_TRACK -> framesDroppedNoTrack.incrementAndGet()
                        WriteOutcome.DISCARDED_PAUSED -> framesDiscardedPaused.incrementAndGet()
                        else -> {}                        // NO_PCM: nothing was decoded, nothing to count
                    }
                    continue
                }
                val n = framesDecoded.incrementAndGet()
                // The underrun count here is the baseline for the session: it already includes the
                // one AudioFlinger tallies while a primed track sits empty (see [buildTrack]), so
                // "underruns at stop minus underruns here" is the number that actually happened
                // while audio was flowing.
                if (n == 1L) {
                    underrunBaseline = track?.underrunCount ?: -1
                    log.i("FIRST AUDIO FRAME PLAYED ($underrunBaseline underruns before it — the fill baseline)")
                }
                if (n % 500 == 0L) {
                    // Judged like the stop() summary: no-track drops are an E, net underruns past the
                    // tolerance a W. framesDiscardedPaused is printed but never judged — it is the hold.
                    val u = track?.underrunCount ?: -1
                    val dropped = framesDroppedNoTrack.get()
                    val net = netUnderruns(u)
                    val msg = "$n audio frames played, ${framesDiscardedPaused.get()} discarded (paused), " +
                              "$dropped dropped (no track), $u underruns ($net net of the fill baseline)"
                    when {
                        dropped > 0 -> log.e("$msg — decoded media is reaching NO live track")
                        net > UNDERRUN_TOLERANCE -> log.w("$msg — audible gaps unless each coincides with a stream end")
                        else -> log.i(msg)
                    }
                }
            }
        } catch (e: Exception) {
            log.e("feed: ${e.javaClass.simpleName}: ${e.message}")
            // A mid-session codec error otherwise keeps the broken codec forever: processAdts never
            // reconfigures (codec != null), the seam stays open, and every later frame throws — dead
            // audio with per-frame spam. Release both (consume thread, the only legal owner) so the
            // next frame rebuilds via the codec == null path.
            if (e is IllegalStateException) {   // covers MediaCodec.CodecException, which extends it
                releaseAv()
                // Arm the backoff. releaseAv() nulls `codec`, so without this the very next ADTS
                // frame takes the codec == null path and reconfigures — and configureFailedAt is 0
                // there because the previous configure SUCCEEDED, so the guard never engages. A
                // persistent codec fault then rebuilt a decoder + AudioTrack ~47 times a second:
                // the codec-pool drain the backoff exists to prevent, plus audible clicking.
                configureFailedAt = android.os.SystemClock.elapsedRealtime()
            }
        }
    }
}
