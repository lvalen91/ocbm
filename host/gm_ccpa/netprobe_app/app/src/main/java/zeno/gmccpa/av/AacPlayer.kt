package zeno.gmccpa.av

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import zeno.gmccpa.ProbeLog
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
 * volume knob. `AudioTrack` is created after the decoder reports its output format, because the channel
 * mask must match what the decoder actually produces, not what ADTS advertised.
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

    val framesDecoded = AtomicLong(0)
    val bytesIn = AtomicLong(0)
    /** Decoded PCM that reached no AudioTrack — a failed prime, or a rebuild that itself failed. */
    val framesDroppedNoTrack = AtomicLong(0)

    private val rates = intArrayOf(96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350)

    private companion object {
        /** Never re-attempt configure per ADTS frame — that drains the codec pool in seconds. */
        const val CONFIGURE_RETRY_MS = 5_000L
        /** How often [reclaimFocus] may re-request after a permanent loss. */
        const val FOCUS_RETRY_MS = 3_000L
        /** 0.2, not 0.8: "duck by 20%" is ~2 dB and was reported by users as "does not duck at all". */
        const val DUCK_GAIN = 0.2f
    }

    @Volatile private var focus: android.media.AudioFocusRequest? = null
    @Volatile private var pausedForAssistant = false
    /** Kept DISTINCT from [pausedForAssistant]: with one flag, a focus GAIN mid-Siri (or Siri ending
     *  under a focus loss) resumes audio the other holder still wants stopped. */
    @Volatile private var pausedForFocus = false
    /** Last focus state seen, so every transition logs old -> new and a wrong mapping is diagnosable
     *  from a capture alone. */
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
     * `pause()` without `flush()`: the buffered media survives, so resume continues where it left off
     * rather than dropping a second of audio. The seam keeps delivering throughout; AudioTrack simply
     * stops consuming, and the consume loop's writes block harmlessly until we resume.
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
     */
    private fun applyPauseState(reason: String) {
        val t = track ?: return
        val held = pausedForAssistant || pausedForFocus
        runCatching {
            if (held) {
                t.pause()
                log.i("media paused for $reason (assistant=$pausedForAssistant focus=$pausedForFocus)")
            } else {
                t.play(); log.i("media resumed after $reason")
            }
        }.onFailure { log.w("$reason pause/resume: ${it.javaClass.simpleName}: ${it.message}") }
    }

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
     */
    private fun requestFocus() {
        val mgr = am ?: run { log.w("no AudioManager — media will not hold focus; the volume knob may stick on Phone/Siri"); return }
        if (focus != null) return
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
            if (r == android.media.AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                focus = req
                focusState = android.media.AudioManager.AUDIOFOCUS_GAIN
                pausedForFocus = false
                // A synchronously GRANTED request gets no GAIN callback, so a focus duck left over from
                // before an abandon would otherwise pin media at DUCK_GAIN with nothing left to clear it.
                setFocusDucked(false)
                log.i("media audio focus: GRANTED")
            } else {
                log.w("media audio focus REQUEST_FAILED (result=$r) — not cached; the next start() retries")
            }
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
     */
    private fun reclaimFocus() {
        val now = android.os.SystemClock.elapsedRealtime()
        if (now - lastFocusRetryAt < FOCUS_RETRY_MS) return
        lastFocusRetryAt = now
        log.i("media is flowing but focus was permanently lost — re-requesting")
        requestFocus()               // clears pausedForFocus itself if the request takes
        if (!pausedForFocus) applyPauseState("focus regained")
    }

    /**
     * Act on focus, do not merely hold it. This head unit ENFORCES focus — `dumpsys car_service`
     * reports `Use hal ducking signals true`, and we were handed a LOSS_TRANSIENT while continuing to
     * play at full gain.
     *
     * Nothing here tears the session down, on any transition: CarPlay expects the phone to stay the
     * mixing authority, so even a permanent LOSS only pauses, unducks and abandons — the seam, the
     * decoder and the track stay live and the next [start] re-requests.
     */
    private val focusListener = android.media.AudioManager.OnAudioFocusChangeListener { change ->
        val from = focusState
        focusState = change
        log.i("media focus ${focusName(from)} -> ${focusName(change)}")
        when (change) {
            android.media.AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> setFocusDucked(true)
            android.media.AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                synchronized(this@AacPlayer) { pausedForFocus = true; applyPauseState("focus loss") }
            }
            android.media.AudioManager.AUDIOFOCUS_GAIN -> {
                // Clears ONLY the focus duck. A voice duck the energy gate still holds stays in force
                // (see [setVoiceDucked]); before 2026-09-09 this call restored unity over a live prompt.
                setFocusDucked(false)
                synchronized(this@AacPlayer) { pausedForFocus = false; applyPauseState("focus gain") }
            }
            android.media.AudioManager.AUDIOFOCUS_LOSS -> {
                synchronized(this@AacPlayer) { pausedForFocus = true; applyPauseState("permanent focus loss") }
                // No focus held means no focus-derived duck: without this a CAN_DUCK followed by LOSS
                // resumed via [reclaimFocus] at DUCK_GAIN for the rest of the session.
                setFocusDucked(false)
                abandonFocus()
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

    private fun abandonFocus() {
        val mgr = am ?: return
        focus?.let { runCatching { mgr.abandonAudioFocusRequest(it) } }
        focus = null
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
            runCatching { track?.setVolume(g) }
            log.i("media ${if (g < 1.0f) "ducked to $g" else "restored to 1.0"} by $source " +
                  "(voice=$voiceDuck focus=$focusDuck)")
        }
    }

    /** Re-assert the current gain onto a freshly built track. Without this a track created while a
     *  duck was in flight starts at unity and the duck is silently lost. Reads the EFFECTIVE gain,
     *  under the same lock, so it is the min() of both sources at that instant. */
    private fun applyGain(t: AudioTrack) {
        synchronized(duckLock) { runCatching { t.setVolume(duckGain) } }
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
     *  neither. No `else t.play()`: [buildTrack] already called `play()`, and an else here would
     *  resume a track [stop] deliberately paused. */
    private fun publishTrack(t: AudioTrack) {
        track = t
        applyGain(t)                                      // duckLock, released before the next line
        synchronized(this) {                              // same monitor as the flag writers
            if (pausedForAssistant || pausedForFocus) {
                runCatching { t.pause() }
                log.i("new track starts paused (assistant=$pausedForAssistant focus=$pausedForFocus)")
            }
        }
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
     * `read()`, but never one parked in the blocking `write()`. `pause()` is documented to interrupt an
     * in-flight write; `flush()` then drops queued PCM so nothing plays out after stop and no write is
     * left waiting on buffer space that would never free.
     *
     * Silence the track BEFORE abandoning focus — the same order as `VoiceRouter.Sink.release()`
     * (pause → flush → stop → release, then abandon). AAOS hands focus to the next owner the moment we
     * abandon; until 2026-09-09 that came first, so for the window before `pause()` landed we were
     * still writing PCM at full gain while another app had already been told it owned focus.
     */
    fun stop() {
        val hadConsumer = consumeStarted
        // Sample before the release below nulls the track; -1 means "no track to ask".
        val underruns = runCatching { track?.underrunCount }.getOrNull() ?: -1
        running.set(false)
        runCatching { track?.pause() }
        runCatching { track?.flush() }
        // If consume() never ran, nothing else will EVER release the primed track: releaseAv() is
        // reachable only from the consume thread. That happens on every path where :9002 never
        // connected — a bind failure, or a session torn down before the producer dialled — and
        // AudioTrack instances come from a small global pool, so a retry loop exhausts it and every
        // later build() in the process throws.
        if (!hadConsumer) {
            val t = track; track = null
            runCatching { t?.stop() }; runCatching { t?.release() }
            log.i("released the primed track (the media seam never connected)")
        }
        // Last, once no track of ours is still playing (or, with a consumer, is paused and flushed —
        // its release follows on the consume thread, which `running=false` has already told to stop
        // writing).
        abandonFocus()
        log.i("stopping — ${framesDecoded.get()} frames played, ${framesDroppedNoTrack.get()} dropped " +
              "(no track), $underruns underruns, ${bytesIn.get()} bytes")
    }

    /** Consume ADTS off the seam until it closes. Blocking; call on its own thread. */
    @Volatile private var consumeStarted = false

    fun consume(ins: InputStream) {
        consumeStarted = true
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
        } catch (e: Exception) {
            // Release BOTH or the native side leaks — `it.play()` can throw after build() succeeded,
            // which would strand a live AudioTrack if only the codec were released.
            runCatching { c?.release() }
            runCatching { t?.release() }
            configureFailedAt = now
            log.e("configure failed (retry in ${CONFIGURE_RETRY_MS}ms): ${e.message}")
        }
    }

    /** Track construction, shared by [prime], [configure] and the ERROR_DEAD_OBJECT rebuild in [feed].
     *
     *  Returns an UNPUBLISHED, playing track at unity gain. The caller MUST hand it to [publishTrack];
     *  nothing here reads the duck or hold flags, because doing so before the publish is exactly the
     *  window that lost edges (see [publishTrack]). */
    private fun buildTrack(sampleRate: Int, channels: Int): AudioTrack {
        val chMask = if (channels >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val minBuf = AudioTrack.getMinBufferSize(sampleRate, chMask, AudioFormat.ENCODING_PCM_16BIT)
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
            .setBufferSizeInBytes(maxOf(minBuf, 8192) * 2)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
            // `play()` can throw AFTER build() succeeded. `.also { it.play() }` would propagate before
            // the track is ever returned, so the caller's `t` is still null and its `t?.release()`
            // releases nothing — stranding a live native AudioTrack. Release it here, where it is
            // still in hand, then rethrow. (The caller's comment claimed this was handled; it was not.)
            .also { trk ->
                try { trk.play() } catch (e: Throwable) { runCatching { trk.release() }; throw e }
            }
    }

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
                var written = false
                c.getOutputBuffer(outIdx)?.let { ob ->
                    val pcm = ByteArray(info.size)
                    ob.position(info.offset); ob.get(pcm)
                    track?.let { t ->
                        written = true
                        val w = t.write(pcm, 0, pcm.size)
                        if (w == AudioTrack.ERROR_DEAD_OBJECT) {
                            // An audioserver restart or a route teardown invalidates the track. Every
                            // later write then fails silently while the decoder keeps running — audio
                            // is gone for the rest of the session with a single log line. Rebuild in
                            // place; this runs on the consume thread, the only legal owner.
                            log.e("AudioTrack ERROR_DEAD_OBJECT — rebuilding")
                            track = null
                            runCatching { t.release() }
                            runCatching { buildTrack(cfgRate, cfgChannels) }
                                .onSuccess { publishTrack(it) }
                                .onFailure { log.e("track rebuild failed: ${it.message}") }
                        } else if (w < 0) {
                            log.e("AudioTrack.write returned $w")
                        }
                    }
                }
                c.releaseOutputBuffer(outIdx, false)
                // Count only what an AudioTrack actually took. A null track — a prime that failed, or
                // the rebuild above failing — used to increment here too, so "FIRST AUDIO FRAME PLAYED"
                // printed with nothing playing and the SESSION frame count was pure fiction.
                if (!written) {
                    if (track == null) framesDroppedNoTrack.incrementAndGet()
                    continue
                }
                val n = framesDecoded.incrementAndGet()
                if (n == 1L) log.i("FIRST AUDIO FRAME PLAYED")
                if (n % 500 == 0L) log.i("$n audio frames played, ${framesDroppedNoTrack.get()} dropped " +
                                         "(no track), ${track?.underrunCount ?: -1} underruns")
            }
        } catch (e: Exception) {
            log.e("feed: ${e.javaClass.simpleName}: ${e.message}")
            // A mid-session codec error otherwise keeps the broken codec forever: processAdts never
            // reconfigures (codec != null), the seam stays open, and every later frame throws — dead
            // audio with per-frame spam. Release both (consume thread, the only legal owner) so the
            // next frame rebuilds via the codec == null path.
            if (e is MediaCodec.CodecException || e is IllegalStateException) {
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
