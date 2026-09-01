package com.carlink.ocbm

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

/**
 * The OCBM host client — the Kotlin equivalent of `ocbm-host`'s `Link` and the macOS
 * `OCBMClient.swift`, driving any [RawBulkTransport].
 *
 * Bring-up order, which is load-bearing:
 *
 *   1. CT_HELLO, retransmitted until CT_HELLO_ACK arrives. Every other frame is DISCARDED while
 *      waiting — a prior session can leave kernel-buffered A/V frames queued ahead of the ACK.
 *   2. CT_SETTIME immediately. The box has no RTC battery, so its clock is bogus at every boot and
 *      CarPlay's TLS pairing needs a real one.
 *   3. CT_SUBSCRIBE with the config blob. This is the presence latch: `/tmp/host_present` goes to 1
 *      and the box supervisor brings the radios up from that edge. THE APP IS THE IGNITION.
 *   4. CT_HEARTBEAT at 1 Hz. Miss the box's 10 s watchdog and it emits SEV_HOST_GONE, clears its
 *      own `subscribed` flag, and IGNORES further heartbeats until a fresh SUBSCRIBE.
 */
class OcbmClient(
    private val transport: RawBulkTransport,
    private val log: com.carlink.logging.ProbeLog.Logger,
) {
    private val reasm = Reassembler()
    private val seq = AtomicInteger(0)
    private val running = AtomicBoolean(false)

    private val ctrlQ = LinkedBlockingQueue<ByteArray>()
    private val mfiQ = LinkedBlockingQueue<ByteArray>()
    private val mgmtQ = LinkedBlockingQueue<ByteArray>()

    @Volatile var helloAcked = false
        private set

    @Volatile var caps = 0
        private set

    @Volatile var activeMode: Byte = Ocbm.MODE_PROJECTION
        private set

    @Volatile var subscribed = false
        private set

    @Volatile var lastSessionEvent: Byte = 0
        private set

    @Volatile var framesIn = 0L
        private set

    @Volatile var bytesIn = 0L
        private set

    val hasMfi: Boolean get() = (caps and Ocbm.CAP_MFI) != 0

    /** Config blob last sent with CT_SUBSCRIBE; re-sent verbatim if the box drops us. */
    @Volatile private var configBlob: ByteArray = ByteArray(0)

    private var heartbeatThread: Thread? = null

    /** Optional observer for session events, so the UI can react to HOST_GONE / PHONE_* live. */
    var onSessionEvent: ((Byte) -> Unit)? = null

    /**
     * Optional observer for the box's pairing code. Empty string means "cleared". This is the one
     * value that MUST reach a human — it is matched against a prompt on the iPhone, and until it is
     * on screen the only place it exists is a logcat line nobody is watching from the driver's seat.
     */
    var onPairingCode: ((String) -> Unit)? = null

    /**
     * Host presence as the BOX sees it: true on SEV_HOST_PRESENT, false on SEV_HOST_GONE.
     *
     * A `false` here is recoverable — the heartbeat's backoff re-subscribes. Do NOT tear a session down
     * on it; that is [onLinkDead]'s job.
     */
    var onHostPresence: ((Boolean) -> Unit)? = null

    /**
     * iPhone presence on the BOX's own USB bus (SEV_PHONE_*), not Bluetooth. Lets the UI say "waiting
     * for phone" truthfully within a second or two instead of inferring it from an A/V timeout.
     */
    var onPhonePresence: ((Boolean) -> Unit)? = null

    /**
     * Mic-uplink gate (CT_UPLINK): the box raises this when the iPhone opens an input SETUP and lowers
     * it on teardown, carrying the negotiated `(rate, channels)`. Capture ONLY while on, at exactly
     * that format — guessing is how an uplink ends up pitch-shifted.
     */
    var onUplinkGate: ((on: Boolean, rate: Int, channels: Int) -> Unit)? = null

    /**
     * Bluetooth/iAP2 handshake progress (CT_BT_PHASE). The only signal the host has for the whole BT
     * phase. Advisory: treat an unknown value as progress and never gate on ordering.
     */
    var onBtPhase: ((Byte) -> Unit)? = null

    /**
     * The connected phone's identity as a raw JSON string, or "" when cleared (CT_PHONE_IDENT).
     *
     * Fires on the read thread — hand off, do not block. Raw rather than parsed because this layer
     * carries no JSON dependency by design; the session layer already parses META_JSON and owns the
     * decision about what an unexpected shape means.
     */
    var onPhoneIdentity: ((String) -> Unit)? = null

    /** Last identity seen this session, so a late attacher is not blind until the next change. */
    @Volatile
    var lastPhoneIdentity: String = ""
        private set

    /**
     * The link is FATALLY dead — the read loop died, or writes failed persistently. Fired at most once.
     *
     * Distinct from SEV_HOST_GONE on purpose: that one is a box-side drop the client recovers from
     * itself, while this one means the transport is gone and the session must be rebuilt from a fresh
     * transport + client pair.
     */
    var onLinkDead: ((String) -> Unit)? = null

    @Volatile var uplinkOn = false
        private set

    @Volatile var uplinkRate = 0
        private set

    @Volatile var uplinkChannels = 0
        private set

    @Volatile var lastBtPhase: Byte = Ocbm.BTP_IDLE
        private set

    @Volatile var phonePresent = false
        private set

    /**
     * True once [stop] has run. The instance is single-use BY CONTRACT: `UsbBulkTransport.stop()` nulls
     * its connection and may defer the interface release, and there is no reopen — so a "restartable"
     * client would be a lie that fails at the first write. Reconnecting means a fresh transport and a
     * fresh client, which is cheap: this object holds no Context and no native resources.
     */
    @Volatile var stopped = false
        private set

    val isRunning: Boolean get() = running.get()

    private val linkDeadAnnounced = AtomicBoolean(false)

    /** Fire [onLinkDead] at most once, whichever detector gets there first. */
    private fun announceLinkDead(reason: String) {
        if (!linkDeadAnnounced.compareAndSet(false, true)) return
        log.e("!! LINK DEAD: $reason")
        // NOT on the caller's thread. The natural reaction is stop(), which joins the heartbeat and tx
        // threads — and one of those is often the very thread detecting the death, so an inline call
        // would join itself and burn the full timeout. The transport's dead handler avoids this the
        // same way.
        val cb = onLinkDead ?: return
        Thread({ runCatching { cb(reason) } }, "ocbm-linkdead").apply { isDaemon = true }.start()
    }

    // ---- A/V lanes ------------------------------------------------------------------------------

    /**
     * The live A/V generation, and the **arming guard** for every A/V channel.
     *
     * Non-null only between a successful CT_SUBSCRIBE and the next STOP or SEV_HOST_GONE. Because
     * [dispatch] routes A/V only while this is non-null, a frame arriving before HELLO_ACK or before
     * SUBSCRIBE is counted and dropped instead of reaching a decoder with a stale or absent key. That
     * used to be true only by accident — [dispatch] discarded A/V unconditionally — and would have
     * become a silent bug the moment the channels were wired.
     */
    @Volatile
    var lanes: OcbmAvLanes? = null
        private set

    /** Per-channel counts of A/V frames dropped because no generation was armed. */
    private val avDropped = HashMap<Int, Long>()
    private var lastAvDropLogNs = 0L

    /**
     * A fresh A/V generation is live. The session layer attaches a video pipe and starts its audio
     * consumers here. Invoked on whichever thread called [subscribe] — i.e. the bring-up thread, or the
     * heartbeat thread on a re-subscribe after SEV_HOST_GONE.
     */
    var onLanesArmed: ((OcbmAvLanes) -> Unit)? = null

    /** The previous generation is being torn down; release anything attached to it. */
    var onLanesRetired: (() -> Unit)? = null

    /** CH_METADATA messages: `(marker, payload)`. Fires on the read thread — hand off, do not block. */
    var onMetadata: ((marker: Int, payload: ByteArray) -> Unit)? = null

    /**
     * The first video key of a generation arrived: a paired, keyed session now exists.
     *
     * This is the earliest truthful "device connected" edge available — earlier than the first decoded
     * frame, and unlike a phone-presence event it proves the crypto handshake completed. Fires on the
     * read thread, once per generation.
     */
    var onSessionKeyed: (() -> Unit)? = null

    private val lanesLock = Any()

    /** Reset per generation, so a re-subscribe re-announces the keyed edge for the new session. */
    private val videoKeyedThisGeneration = AtomicBoolean(false)

    private fun armLanes() {
        synchronized(lanesLock) {
            if (lanes != null) return
            videoKeyedThisGeneration.set(false)
            // Re-check under the lock. `subscribe()` tested `running` on entry, but it can block for
            // seconds on txLock behind a slow write — long enough for stop() to time out its heartbeat
            // join and complete teardown. Without this, a late re-subscribe arms a generation AFTER the
            // client is stopped: the session layer gets told to attach renderers to a dead client, and
            // nothing will ever retire them because stop() has already run.
            if (!running.get()) {
                log.w("armLanes after stop — refusing (late re-subscribe raced teardown)")
                return
            }
            val l =
                OcbmAvLanes(
                    log = log,
                    onKeyframeNeeded = { requestKeyframe() },
                    onMetadata = { m, p -> onMetadata?.invoke(m, p) },
                    onVideoKeyed = { if (videoKeyedThisGeneration.compareAndSet(false, true)) onSessionKeyed?.invoke() },
                )
            lanes = l
            log.i("A/V lanes armed")
            val armed = runCatching { onLanesArmed?.invoke(l) }
            if (armed.isFailure) {
                // The session layer failed to attach consumers. Leaving the generation armed would let
                // dispatch fill pipes nobody drains, and SeamPipe.write would then park the USB read
                // thread forever — taking every channel down. Retire it and treat the link as dead:
                // a session that cannot render is not recoverable by retrying frames at it.
                log.e("onLanesArmed threw (${armed.exceptionOrNull()?.javaClass?.simpleName}) — retiring")
                lanes = null
                Thread({ runCatching { l.close() } }, "ocbm-lanes-close").apply { isDaemon = true }.start()
                announceLinkDead("A/V consumers failed to start")
            }
        }
    }

    private fun retireLanes(why: String) {
        val l = synchronized(lanesLock) { lanes.also { lanes = null } } ?: return
        log.i("retiring A/V lanes — $why")
        runCatching { onLanesRetired?.invoke() }
        // close() joins consumer threads, so never call it from a consumer or the read thread would be
        // waiting on work it is itself blocking. Retirement is driven from the CTRL path, which is the
        // read thread — so hand the close off rather than joining inline.
        Thread({ runCatching { l.close() } }, "ocbm-lanes-close").apply { isDaemon = true }.start()
    }

    // ---- wire helpers ---------------------------------------------------------------------------

    /**
     * Frame and write, with sequence assignment INSIDE the lock.
     *
     * `seq` being atomic is not enough on its own: two threads could each take a number and then race
     * into `writeBulk`, putting frame N on the wire after N+1. Nothing validates `seq` today (it is
     * debug-only and outside the header checksum), but a monotonic wire order costs one lock on a path
     * that is already doing a USB transfer, and it makes a capture readable.
     */
    private fun sendSync(
        channel: Int,
        payload: ByteArray,
    ): Boolean =
        synchronized(txLock) {
            transport.writeBulk(Framing.frame(channel, Ocbm.F_BOTH, seq.getAndIncrement(), payload))
        }

    private fun send(
        channel: Int,
        payload: ByteArray,
    ): Boolean = sendSync(channel, payload)

    private val txLock = Any()

    /**
     * Identity of the host SESSION this client belongs to, sent in [Ocbm.CT_HELLO].
     *
     * Deliberately settable rather than per-client: the contract is one client per USB session, so a
     * client-scoped value would change on every reattach and the box would re-arm projection for a
     * mere USB blip — a ~45 s rebuild instead of a warm reuse. The owner (one `CarlinkManager`) holds
     * it stable across the clients it builds, so "same nonce" means "same host, reattaching" and
     * "different nonce" means "the previous host is gone".
     *
     * 0 = not supplied; the box then behaves exactly as it did before this existed.
     */
    @Volatile var instanceNonce: Int = 0

    fun start() {
        if (stopped) {
            log.e("start() on a stopped client — construct a fresh transport + client")
            return
        }
        if (!running.compareAndSet(false, true)) return
        transport.setReadHandler { data, len ->
            bytesIn += len
            reasm.push(data, len)
            while (true) {
                val f = reasm.next() ?: break
                framesIn++
                dispatch(f)
            }
        }
        transport.setDeadHandler { announceLinkDead("read loop died") }
        startTx()
        transport.start()
    }

    /**
     * Route one frame. Runs on the transport read thread, and A/V is fed **inline** on purpose:
     * `SeamPipe`'s blocking write is the closed-loop backpressure (lane backs up → USB fills → the
     * box's read-gate stops pulling → the iPhone throttles that encoder). A handoff queue here would
     * replace that loop with an unbounded buffer and merely relocate the head-of-line stall.
     *
     * Safe because `Reassembler.next()` hands out an owned copy — only the transport's own read buffer
     * is reused, and `push` copies it synchronously.
     */
    private fun dispatch(f: OcbmFrame) {
        when (f.channel) {
            Ocbm.CH_CTRL -> handleCtrl(f.payload)
            Ocbm.CH_MFI -> mfiQ.offer(f.payload)
            Ocbm.CH_MGMT -> mgmtQ.offer(f.payload)
            Ocbm.CH_VIDEO -> lanes?.feedVideo(f.payload) ?: dropAv(f)
            Ocbm.CH_MEDIA_AUDIO -> lanes?.feedMedia(f.payload) ?: dropAv(f)
            Ocbm.CH_ALT_AUDIO -> lanes?.feedVoice(f.payload) ?: dropAv(f)
            Ocbm.CH_METADATA -> lanes?.feedMetadata(f.payload) ?: dropAv(f)
            // No cluster display is advertised (altVideoStreams: []), so the box never encodes this.
            // Counted rather than decoded, so a surprise stream is visible instead of a log flood.
            Ocbm.CH_ALT_VIDEO -> altVideoFrames++
            else -> dropAv(f)
        }
    }

    @Volatile var altVideoFrames = 0L
        private set

    /** Count an unroutable frame and say so at most once a second — 60 fps of video is a log flood. */
    private fun dropAv(f: OcbmFrame) {
        avDropped[f.channel] = (avDropped[f.channel] ?: 0L) + 1
        val now = System.nanoTime()
        if (now - lastAvDropLogNs < 1_000_000_000L) return
        lastAvDropLogNs = now
        val why = if (lanes == null) "no session armed" else "unhandled channel"
        log.w(
            "<< dropped ${Ocbm.channelName(f.channel)} (${f.payload.size}B) — $why " +
                "(total on this channel=${avDropped[f.channel]})",
        )
    }

    private fun handleCtrl(pl: ByteArray) {
        if (pl.isEmpty()) return
        when (pl[0]) {
            Ocbm.CT_HELLO_ACK -> {
                if (pl.size >= 6) {
                    caps = (pl[2].toInt() and 0xFF) or ((pl[3].toInt() and 0xFF) shl 8) or
                        ((pl[4].toInt() and 0xFF) shl 16) or ((pl[5].toInt() and 0xFF) shl 24)
                    if (pl.size >= 7) activeMode = pl[6]
                    helloAcked = true
                    ctrlQ.offer(pl)
                }
            }
            Ocbm.CT_SETTIME -> ctrlQ.offer(pl)
            Ocbm.CT_SESSION_EVENT -> {
                if (pl.size >= 2) {
                    val sev = pl[1]
                    lastSessionEvent = sev
                    log.i("<< SESSION_EVENT ${Ocbm.sevName(sev)}")
                    when (sev) {
                        Ocbm.SEV_HOST_GONE -> {
                            // The box has cleared its own subscribed flag and will ignore our
                            // heartbeats until a fresh SUBSCRIBE. Retire this A/V generation FIRST:
                            // a partial seam message left across the boundary would otherwise be
                            // interpreted against the next session's key, and a fresh generation also
                            // gives the renderer a fresh consume() that re-arms its keyframe gate.
                            retireLanes("box declared us gone")
                            subscribed = false
                            log.w("!! box dropped us — re-subscribing on the next heartbeat tick")
                            onHostPresence?.invoke(false)
                        }
                        Ocbm.SEV_HOST_PRESENT -> onHostPresence?.invoke(true)
                        Ocbm.SEV_PHONE_PRESENT -> {
                            phonePresent = true
                            onPhonePresence?.invoke(true)
                        }
                        Ocbm.SEV_PHONE_ABSENT -> {
                            phonePresent = false
                            onPhonePresence?.invoke(false)
                        }
                        else -> Unit
                    }
                    onSessionEvent?.invoke(sev)
                }
            }
            Ocbm.CT_UPLINK -> {
                if (pl.size >= 7) {
                    val on = (pl[1].toInt() and 0xFF) != 0
                    val rate =
                        (pl[2].toInt() and 0xFF) or ((pl[3].toInt() and 0xFF) shl 8) or
                            ((pl[4].toInt() and 0xFF) shl 16) or ((pl[5].toInt() and 0xFF) shl 24)
                    val ch = pl[6].toInt() and 0xFF
                    log.i("<< UPLINK ${if (on) "ON" else "OFF"} ${rate}Hz ${ch}ch (mic gate)")
                    uplinkOn = on
                    uplinkRate = if (on) rate else 0
                    uplinkChannels = if (on) ch else 0
                    onUplinkGate?.invoke(on, uplinkRate, uplinkChannels)
                }
            }
            Ocbm.CT_PHONE_IDENT -> {
                val json = if (pl.size > 1) String(pl, 1, pl.size - 1, Charsets.UTF_8).trim() else ""
                lastPhoneIdentity = json
                log.i(if (json.isEmpty()) "<< PHONE_IDENT cleared" else "<< PHONE_IDENT $json")
                onPhoneIdentity?.invoke(json)
            }
            Ocbm.CT_BT_PHASE -> {
                if (pl.size >= 2) {
                    val p = pl[1]
                    lastBtPhase = p
                    log.i("<< BT_PHASE ${Ocbm.btpName(p)}")
                    onBtPhase?.invoke(p)
                }
            }
            Ocbm.CT_PAIRING_CODE -> {
                val code = if (pl.size > 1) String(pl, 1, pl.size - 1, Charsets.US_ASCII).trim() else ""
                log.i(if (code.isEmpty()) "<< PAIRING_CODE cleared" else "<< PAIRING_CODE $code  <-- match this on the iPhone")
                onPairingCode?.invoke(code)
            }
            else -> log.i("<< CTRL unknown type 0x%02x (${pl.size}B)".format(pl[0]))
        }
    }

    // ---- uplink (CH_INPUT / CH_MIC) --------------------------------------------------------------

    private class Tx(
        val channel: Int,
        val payload: ByteArray,
    )

    /**
     * Bounded hand-off for the high-rate and UI-thread senders.
     *
     * `writeBulk` can park for its full 2 s timeout, so a touch dispatched straight from the UI thread
     * is an ANR waiting to happen. Everything here is drained by one `ocbm-tx` thread that calls the
     * same [sendSync] — so there is still exactly one lock and one wire order, and a caller never
     * blocks. A full queue drops and counts; dropping a touch beats freezing the UI.
     */
    private val txQ = java.util.concurrent.ArrayBlockingQueue<Tx>(256)
    private var txThread: Thread? = null

    @Volatile var txDropped = 0L
        private set

    private var lastInputDropLogNs = 0L
    private var inputDropCount = 0L

    /**
     * Write one logical payload, splitting it into `MIC_CHUNK` frames if needed — **holding [txLock]
     * across the whole split** so no other sender can interleave a frame into the middle of it.
     *
     * This is what makes a mic buffer atomic on the wire. Chunking at the *enqueue* side could not:
     * a check-then-act against the queue's remaining capacity races any other producer (a UI touch is
     * enough), so the tail chunks could be dropped after the head was already committed — a torn PCM
     * buffer, which is precisely what the all-or-nothing wording promised to prevent.
     */
    private fun sendOversize(
        channel: Int,
        payload: ByteArray,
    ): Boolean {
        if (payload.size <= Ocbm.MIC_CHUNK) return sendSync(channel, payload)
        synchronized(txLock) {
            var i = 0
            while (i < payload.size) {
                val end = minOf(i + Ocbm.MIC_CHUNK, payload.size)
                // sendSync re-enters the same reentrant monitor; the point is that no OTHER thread can
                // take it between chunks.
                if (!sendSync(channel, payload.copyOfRange(i, end))) return false
                i = end
            }
        }
        return true
    }

    private fun startTx() {
        if (txThread?.isAlive == true) return
        val t =
            Thread({
                while (running.get()) {
                    val item =
                        try {
                            txQ.poll(100, TimeUnit.MILLISECONDS)
                        } catch (_: InterruptedException) {
                            null
                        }
                    if (item != null) sendOversize(item.channel, item.payload)
                }
            }, "ocbm-tx")
        t.isDaemon = true
        txThread = t
        t.start()
    }

    /** Queue one frame. Returns false if it was dropped (not subscribed, or the queue is full). */
    private fun enqueue(
        channel: Int,
        payload: ByteArray,
        what: String,
    ): Boolean {
        if (!subscribed) {
            noteInputDrop(what)
            return false
        }
        if (!txQ.offer(Tx(channel, payload))) {
            txDropped++
            noteInputDrop("$what (tx queue full)")
            return false
        }
        return true
    }

    /** An input the client swallowed used to vanish silently. Count every one, log at most 1/s. */
    private fun noteInputDrop(what: String) {
        inputDropCount++
        val now = System.nanoTime()
        if (now - lastInputDropLogNs < 1_000_000_000L) return
        lastInputDropLogNs = now
        log.w("input dropped — no session: $what (total=$inputDropCount)")
    }

    /**
     * One touch point. `nx`/`ny` are normalized 0..1; the box scales them to the resolution it
     * advertised, so the host never needs to know the panel size.
     */
    fun sendTouch(
        phase: Byte,
        nx: Float,
        ny: Float,
        finger: Int = 0,
    ): Boolean {
        val xi = (nx.coerceIn(0f, 1f) * 65535f).toInt().coerceIn(0, 65535)
        val yi = (ny.coerceIn(0f, 1f) * 65535f).toInt().coerceIn(0, 65535)
        val p =
            byteArrayOf(
                Ocbm.INPUT_TOUCH,
                phase,
                (xi and 0xFF).toByte(),
                ((xi ushr 8) and 0xFF).toByte(),
                (yi and 0xFF).toByte(),
                ((yi ushr 8) and 0xFF).toByte(),
                (finger and 0xFF).toByte(),
            )
        return enqueue(Ocbm.CH_INPUT, p, "touch")
    }

    /** Tap a media-transport button on the advertised uid-2 HID; the box completes press+release. */
    fun sendMediaButton(index: Byte): Boolean = enqueue(Ocbm.CH_INPUT, byteArrayOf(Ocbm.INPUT_MEDIA_BTN, index), "media button $index")

    /** Send a mapped `/command`; the box owns the encrypted event channel and does the dispatch. */
    fun sendCommand(
        cmd: Byte,
        vararg args: Byte,
    ): Boolean = enqueue(Ocbm.CH_INPUT, byteArrayOf(Ocbm.INPUT_COMMAND, cmd) + args, "command 0x%02x".format(cmd))

    /** Global night mode — one input alongside iOS's own logic, not a per-display override. */
    fun sendNightMode(on: Boolean): Boolean = sendCommand(Ocbm.CMD_NIGHT_MODE, if (on) 1 else 0)

    /** Explicit per-display appearance. Silently ignored unless the pushed config advertised it. */
    fun sendAppearance(
        dark: Boolean,
        isMap: Boolean,
        stream: Byte = Ocbm.APPEARANCE_STREAM_MAIN,
    ): Boolean =
        sendCommand(
            if (isMap) Ocbm.CMD_MAP_APPEARANCE else Ocbm.CMD_UI_APPEARANCE,
            stream,
            if (dark) Ocbm.APPEARANCE_MODE_DARK else Ocbm.APPEARANCE_MODE_LIGHT,
        )

    fun sendNav(nav: Byte): Boolean = enqueue(Ocbm.CH_INPUT, byteArrayOf(Ocbm.INPUT_NAV, nav), "nav $nav")

    fun sendTelephony(index: Byte): Boolean = enqueue(Ocbm.CH_INPUT, byteArrayOf(Ocbm.INPUT_TELEPHONY, index), "telephony $index")

    private val lastKeyframeReqNs =
        java.util.concurrent.atomic
            .AtomicLong(0)
    private val lastAltKeyframeReqNs =
        java.util.concurrent.atomic
            .AtomicLong(0)

    /**
     * Ask the box to pull an IDR from iOS.
     *
     * Throttled, and that is load-bearing: `VideoSeam.onGap` fires per sequence gap AND per decrypt
     * failure, so a wrong key produces one call per frame — 60/s — while a single IDR repaints the
     * whole screen anyway.
     */
    fun requestKeyframe(): Boolean = throttledKeyframe(lastKeyframeReqNs, Ocbm.INPUT_KEYFRAME, "keyframe")

    /** The cluster lane has its own counter so a main gap and an alt gap can't suppress each other. */
    fun requestAltKeyframe(): Boolean = throttledKeyframe(lastAltKeyframeReqNs, Ocbm.INPUT_KEYFRAME_ALT, "keyframe-alt")

    private fun throttledKeyframe(
        stamp: java.util.concurrent.atomic.AtomicLong,
        op: Byte,
        what: String,
    ): Boolean {
        val now = System.nanoTime()
        val prev = stamp.get()
        if (now - prev < Ocbm.KEYFRAME_MIN_INTERVAL_MS * 1_000_000L) return false
        if (!stamp.compareAndSet(prev, now)) return false
        return enqueue(Ocbm.CH_INPUT, byteArrayOf(op), what)
    }

    /**
     * Ship captured mic PCM (S16LE at the gate's negotiated rate/channels).
     *
     * CH_MIC is a boundary-less byte stream, so an oversize buffer is split into `MIC_CHUNK` frames —
     * but the split happens on the **tx thread** ([sendOversize]), not here. One buffer therefore
     * occupies exactly ONE queue slot and is written under a single [txLock] hold, so it is atomic in
     * both senses: it cannot be partly-enqueued-then-dropped, and no other sender's frame can land in
     * the middle of it.
     *
     * Chunking at the enqueue side could not give either guarantee — a capacity check followed by a
     * loop of offers races any other producer (one UI touch is enough), which would commit the head of
     * a buffer and drop its tail.
     */
    fun sendMicPcm(
        pcm: ByteArray,
        off: Int = 0,
        len: Int = pcm.size,
    ): Boolean {
        if (len <= 0) return true
        return enqueue(Ocbm.CH_MIC, pcm.copyOfRange(off, off + len), "mic pcm")
    }

    /** Mid-session radio kill switch (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating). Blocking; call off the main thread. */
    fun setRadios(on: Boolean): Boolean = send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_RADIO, if (on) 1 else 0))

    // ---- bring-up -------------------------------------------------------------------------------

    /**
     * Send CT_HELLO and wait for CT_HELLO_ACK, retransmitting on the reference cadence (500 ms for
     * the first 15 s, then 2 s). Every non-ACK frame arriving meanwhile is discarded by [dispatch].
     */
    fun hello(timeoutMs: Long = 20_000): Boolean {
        ctrlQ.clear()
        // The trailing 4 bytes are the HOST INSTANCE NONCE (u32 LE). See Ocbm.CT_HELLO: it is how the
        // box tells one host PROCESS from another. Without it, a host killed without CT_STOP leaves the
        // box `present`, and a relaunch inside the heartbeat grace keeps the box's liveness stamp fresh
        // from the NEW process — so the box never re-arms projection and the session comes up with no
        // A/V. 0 means "not supplied", which the box treats as its old behaviour.
        val inst = instanceNonce
        val hi =
            byteArrayOf(
                Ocbm.CT_HELLO,
                Ocbm.VERSION,
                (inst and 0xFF).toByte(),
                ((inst ushr 8) and 0xFF).toByte(),
                ((inst ushr 16) and 0xFF).toByte(),
                ((inst ushr 24) and 0xFF).toByte(),
            )
        val deadline = System.currentTimeMillis() + timeoutMs
        val fastUntil = System.currentTimeMillis() + 15_000
        var attempt = 0
        while (System.currentTimeMillis() < deadline) {
            attempt++
            // A single failed write must NOT abort bring-up: right after a box reboot the gadget is
            // re-enumerating and the first write(s) can fail while a retry 500 ms later succeeds.
            if (!send(Ocbm.CH_CTRL, hi)) {
                log.w(">> CT_HELLO write failed (attempt $attempt) — retrying")
            } else if (attempt == 1) {
                log.i(">> CT_HELLO (v${Ocbm.VERSION})")
            }
            val waitMs = if (System.currentTimeMillis() < fastUntil) 500L else 2000L
            val end = System.currentTimeMillis() + waitMs
            while (System.currentTimeMillis() < end) {
                val pl = ctrlQ.poll(100, TimeUnit.MILLISECONDS) ?: continue
                if (pl.isNotEmpty() && pl[0] == Ocbm.CT_HELLO_ACK) {
                    log.i(
                        "<< CT_HELLO_ACK v${pl[1]} caps=0x%08x [%s] mode=%s"
                            .format(
                                caps,
                                Ocbm.capsString(caps),
                                if (activeMode == Ocbm.MODE_CONSOLE) "CONSOLE" else "PROJECTION",
                            ),
                    )
                    if (!hasMfi) log.w("!! CAP_MFI is NOT set — the box has no MFi chip access (/dev/i2c-1 failed)")
                    return true
                }
            }
        }
        log.e("!! no CT_HELLO_ACK within ${timeoutMs}ms after $attempt attempts")
        return false
    }

    /** Push the wall clock. The box has no RTC battery; do this on every connect. */
    fun setTime(timeoutMs: Long = 2000): Boolean {
        val secs = System.currentTimeMillis() / 1000
        val pl = ByteArray(9)
        pl[0] = Ocbm.CT_SETTIME
        for (i in 0 until 8) pl[1 + i] = ((secs shr (8 * i)) and 0xFF).toByte()
        ctrlQ.clear()
        if (!send(Ocbm.CH_CTRL, pl)) return false
        log.i(">> CT_SETTIME $secs (${java.util.Date(secs * 1000)})")
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            val r = ctrlQ.poll(100, TimeUnit.MILLISECONDS) ?: continue
            if (r.isNotEmpty() && r[0] == Ocbm.CT_SETTIME && r.size >= 10) {
                val ok = r[9] == 0.toByte()
                log.i("<< CT_SETTIME ack status=${r[9]} (${if (ok) "applied" else "settimeofday FAILED"})")
                return ok
            }
        }
        log.w("!! no CT_SETTIME ack — continuing anyway")
        return false
    }

    /**
     * The presence latch. The payload is `[0x10]` followed by the config blob VERBATIM — no length
     * prefix, no terminator; the frame length is the only delimiter. An empty blob is legal and
     * means "subscribe, box defaults".
     */
    fun subscribe(config: ByteArray): Boolean {
        // Record the blob BEFORE the gates. If this lands after them, a subscribe refused pre-ACK
        // leaves configBlob empty, and a later heartbeat-driven re-subscribe then pushes an EMPTY
        // config — a live session on box defaults (1920x720, no HEVC) with no error anywhere.
        configBlob = config
        if (!running.get()) {
            log.w("subscribe after stop — refusing (would re-latch the box)")
            return false
        }
        // Gate on the handshake. Subscribing before the box has ACKed leaves it uncommanded while we
        // believe otherwise, and — now that A/V is routed — would arm the lanes for a session whose
        // key exchange has not happened.
        if (!helloAcked) {
            log.w("subscribe before CT_HELLO_ACK — refusing")
            return false
        }
        val pl = ByteArray(1 + config.size)
        pl[0] = Ocbm.CT_SUBSCRIBE
        System.arraycopy(config, 0, pl, 1, config.size)
        if (!send(Ocbm.CH_CTRL, pl)) {
            log.e(">> CT_SUBSCRIBE write FAILED")
            return false
        }
        subscribed = true
        armLanes()
        log.i(">> CT_SUBSCRIBE (${config.size}B config) — this is the radio-wake edge")
        if (config.isNotEmpty()) {
            config
                .toString(Charsets.UTF_8)
                .trim()
                .lines()
                .forEach { log.i("     | $it") }
        }
        return true
    }

    /** Start the 1 Hz beat. If we are un-subscribed, the tick re-SUBSCRIBEs instead (it also stamps liveness). */
    fun startHeartbeat() {
        // isAlive, not != null: the loop nulls the field on exit (below), so a heartbeat that ended on
        // a spurious interrupt can be restarted instead of refusing forever.
        if (heartbeatThread?.isAlive == true) return
        val t =
            Thread({
                var writeFailures = 0
                var resubBackoff = 0L
                var nextResubAt = 0L
                // Consecutive healthy ticks while subscribed. The backoff clears only after the link
                // has held for this long — see the comment at the reset site.
                var stableTicks = 0
                val stableTicksToClear = 30
                try {
                    while (running.get()) {
                        try {
                            Thread.sleep(Ocbm.HEARTBEAT_MS)
                        } catch (_: InterruptedException) {
                            break
                        }
                        if (!running.get()) break
                        if (subscribed) {
                            // Check the write result — it is the liveness signal. On a box reboot / gadget
                            // stall every write fails; declare the link dead rather than looking healthy
                            // forever with a hung session.
                            if (send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_HEARTBEAT))) {
                                writeFailures = 0
                                // Do NOT clear the backoff just because a write succeeded. A successful
                                // USB write says nothing about whether the box ACCEPTED the subscription,
                                // and clearing it here made the escalation unreachable: re-subscribe ->
                                // one good heartbeat -> backoff reset -> HOST_GONE -> immediate re-latch,
                                // steady state ~2 s forever. That is precisely the rapid down/up edge
                                // pattern that trips the supervisor's flap detector and escalates to an
                                // ocbmd restart and then a full box reboot. Only a SUSTAINED subscription
                                // is evidence the link is healthy.
                                if (resubBackoff != 0L && ++stableTicks >= stableTicksToClear) {
                                    log.i("link stable for ${stableTicksToClear}s — clearing re-subscribe backoff")
                                    resubBackoff = 0L
                                    nextResubAt = 0L
                                    stableTicks = 0
                                }
                            } else if (run {
                                    stableTicks = 0
                                    ++writeFailures
                                } >= 5
                            ) {
                                // stableTicks resets here so "consecutive healthy ticks" is actually
                                // consecutive. Without it, 29 good ticks then a burst of failures then
                                // one good tick still reaches 30 and clears the backoff, on a link that
                                // demonstrably glitched seconds earlier.
                                log.e("!! heartbeat write failed ${writeFailures}x — link presumed dead")
                                // FATAL, and deliberately NOT signalled as SEV_HOST_GONE: that event means
                                // a recoverable box-side drop the backoff above handles by itself, and a
                                // session layer that cannot tell the two apart either tears down on a
                                // recoverable blip or waits forever on a dead pipe. Retire the lanes first
                                // so the renderers release their codecs, then say so exactly once.
                                retireLanes("heartbeat writes failing")
                                subscribed = false
                                announceLinkDead("heartbeat write failed ${writeFailures}x")
                                break
                            }
                        } else {
                            // HOST_GONE recovery WITH backoff: re-latching at 1 Hz produces rapid down/up
                            // edges that trip the box supervisor's flap detector (-> ocbmd restart -> full
                            // box reboot). Space attempts out: 2s, 4s, 8s, capped at 15s.
                            stableTicks = 0
                            val now = System.currentTimeMillis()
                            if (now >= nextResubAt) {
                                log.i(">> re-CT_SUBSCRIBE (recovering from HOST_GONE, backoff=${resubBackoff}ms)")
                                // Count re-subscribe write failures too. Without this the link-dead
                                // detector has a hole: a dead or halted OUT endpoint while IN merely
                                // idles produces no writeFailures (they are only counted in the
                                // subscribed branch) and no read-loop death (an idle IN returns -1 only
                                // after its full timeout, which is classified as idle, not gone) — so
                                // the session spins in silent re-SUBSCRIBE retries forever and
                                // onLinkDead never fires.
                                if (subscribe(configBlob)) {
                                    writeFailures = 0
                                } else if (++writeFailures >= 5) {
                                    log.e("!! re-SUBSCRIBE write failed ${writeFailures}x — link presumed dead")
                                    retireLanes("re-subscribe writes failing")
                                    announceLinkDead("re-SUBSCRIBE write failed ${writeFailures}x")
                                    break
                                }
                                resubBackoff = if (resubBackoff == 0L) 2000L else (resubBackoff * 2).coerceAtMost(15_000L)
                                nextResubAt = now + resubBackoff
                            }
                        }
                    }
                } finally {
                    heartbeatThread = null
                }
            }, "ocbm-heartbeat")
        t.isDaemon = true
        heartbeatThread = t
        t.start()
        log.i(">> heartbeat started at ${Ocbm.HEARTBEAT_MS}ms (box watchdog is ${Ocbm.HEARTBEAT_GRACE_MS}ms)")
    }

    // ---- CH_MFI relay ---------------------------------------------------------------------------

    /**
     * Serializes ALL CH_MFI traffic. The channel has TWO concurrent consumers — the native receiver
     * core's MFi relay (on control-connection threads) and any probe/bring-up cert/sign — and the
     * wire response carries no opcode echo or request id, so without a lock a certificate can be
     * returned as the answer to a signature request (auth-setup then fails and the session drops).
     */
    private val mfiLock = Any()

    private fun mfiRequest(
        req: ByteArray,
        timeoutMs: Long,
        expectedOk: (ByteArray) -> Boolean,
    ): Mfi.Response? {
        synchronized(mfiLock) {
            // Drain stale/late replies from a previous request UNDER the lock, so this request's poll
            // cannot pick up an earlier answer that arrived after that caller's timeout.
            while (mfiQ.poll() != null) { /* discard */ }
            if (!send(Ocbm.CH_MFI, req)) return null
            val deadline = System.currentTimeMillis() + timeoutMs
            while (true) {
                val remaining = deadline - System.currentTimeMillis()
                if (remaining <= 0) return null
                val pl = mfiQ.poll(remaining, TimeUnit.MILLISECONDS) ?: return null
                val resp = Mfi.parse(pl, pl.size) ?: continue
                // Weak length correlation until the protocol grows a request id: an OK reply whose
                // payload length doesn't match this op is a stale/mismatched answer — keep waiting.
                // Error statuses carry no payload and are accepted as-is (a misattributed transient
                // FAILED just triggers the caller's retry, which is benign).
                if (resp.ok && !expectedOk(resp.payload)) {
                    log.w("!! CH_MFI stale/mismatched OK reply (${resp.payload.size}B) discarded")
                    continue
                }
                return resp
            }
        }
    }

    /**
     * Fetch the MFi certificate. Generous timeout: the box takes a bounded flock on
     * /tmp/carplay_mfi.lock with a 10 s deadline, and MFi blocks its single-threaded dispatch.
     */
    fun mfiCertificate(timeoutMs: Long = 12_000): Mfi.Response? {
        log.i(">> CH_MFI copy_certificate")
        // A certificate is ~945 B; a 128-B OK reply is the signature answer to another request.
        return mfiRequest(Mfi.certRequest(), timeoutMs) { it.size > Mfi.SIG_LEN }
    }

    /**
     * Sign a 20-byte SHA-1 digest. The chip sequence alone runs up to ~2.1 s, and with lock
     * contention the worst case approaches 12 s.
     */
    fun mfiSign(
        digest: ByteArray,
        timeoutMs: Long = 15_000,
    ): Mfi.Response? {
        log.i(">> CH_MFI create_signature (${digest.size}B digest)")
        return mfiRequest(Mfi.signRequest(digest), timeoutMs) { it.size == Mfi.SIG_LEN }
    }

    // ---- CH_MGMT --------------------------------------------------------------------------------

    /** MGMT_GET_INFO → MGMT_INFO, whose payload is UTF-8 JSON running to the end of the frame. */
    fun mgmtGetInfo(timeoutMs: Long = 5000): String? {
        mgmtQ.clear()
        if (!send(Ocbm.CH_MGMT, byteArrayOf(Ocbm.MGMT_GET_INFO))) return null
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            val pl = mgmtQ.poll(200, TimeUnit.MILLISECONDS) ?: continue
            if (pl.isNotEmpty() && pl[0] == Ocbm.MGMT_INFO) {
                return String(pl, 1, pl.size - 1, Charsets.UTF_8)
            }
        }
        return null
    }

    /** Any of the four action verbs; returns the status byte from MGMT_ACK (0 ok, 1 error). */
    fun mgmtAction(
        verb: Byte,
        arg: ByteArray = ByteArray(0),
        timeoutMs: Long = 5000,
    ): Int? {
        mgmtQ.clear()
        val pl = ByteArray(1 + arg.size)
        pl[0] = verb
        System.arraycopy(arg, 0, pl, 1, arg.size)
        if (!send(Ocbm.CH_MGMT, pl)) return null
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            val r = mgmtQ.poll(200, TimeUnit.MILLISECONDS) ?: continue
            if (r.size >= 3 && r[0] == Ocbm.MGMT_ACK && r[1] == verb) return r[2].toInt()
        }
        return null
    }

    // ---- teardown -------------------------------------------------------------------------------

    /**
     * Clean shutdown: CT_STOP gives the box its 5 s grace so a quick relaunch reuses the session.
     *
     * Order is deliberate. The lanes close BEFORE CT_STOP because a read thread parked in a seam write
     * has to be released before anything can join it, and the heartbeat is joined first so a straggling
     * tick cannot re-SUBSCRIBE after we have said STOP.
     */
    fun stop() {
        if (!running.compareAndSet(true, false)) return
        // Latch the single-use flag HERE, not at the end of teardown. Set late, a start() racing
        // an in-progress stop() passes its `stopped` check and re-CASes running false->true
        // mid-teardown, yielding a "running" client whose transport is then closed underneath it.
        // CarlinkManager serialises lifecycle today so this is unreachable from the app, but the
        // single-use contract should hold on its own terms.
        stopped = true
        // JOIN, do not just interrupt: bulkTransfer is not interruptible, so a tick already inside
        // send() keeps writing for up to the write timeout. Closing the transport under it is the
        // gadget-stall case.
        heartbeatThread?.let {
            it.interrupt()
            try {
                it.join(3000)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        heartbeatThread = null
        txThread?.let {
            it.interrupt()
            try {
                it.join(1000)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        txThread = null
        // Close inline here (unlike the SEV_HOST_GONE path): stop() is called from the session layer,
        // never from the read thread, so joining the consumers is safe and we want them actually gone
        // before the transport disappears under their codecs.
        synchronized(lanesLock) { lanes.also { lanes = null } }?.let {
            runCatching { onLanesRetired?.invoke() }
            runCatching { it.close() }
        }
        if (subscribed) {
            log.i(">> CT_STOP")
            send(Ocbm.CH_CTRL, byteArrayOf(Ocbm.CT_STOP))
            subscribed = false
        }
        transport.stop()
        stopped = true
    }

    fun statsLine(): String =
        "frames=$framesIn bytes=$bytesIn resyncBytes=${reasm.resyncBytes} helloAck=$helloAcked " +
            "subscribed=$subscribed caps=[${Ocbm.capsString(caps)}] " +
            "btPhase=${Ocbm.btpName(lastBtPhase)} phone=$phonePresent txDrop=$txDropped " +
            "altVid=$altVideoFrames" +
            (lanes?.let { " ${it.statsLine()}" } ?: "")
}
