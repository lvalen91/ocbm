package com.carlink.ocbm

import com.carlink.logging.ProbeLog
import com.carlink.ocbm.seam.AudioSeam
import com.carlink.ocbm.seam.MetadataSeam
import com.carlink.ocbm.seam.SeamPipe
import com.carlink.ocbm.seam.VideoSeam
import java.io.InputStream
import java.util.concurrent.atomic.AtomicBoolean

/**
 * One A/V generation: the seams, the audio pipes, and the threads that consume them.
 *
 * Built when `CT_SUBSCRIBE` lands, closed on `STOP` or `SEV_HOST_GONE`, never reused. Its existence is
 * also the **arming guard** — `OcbmClient` only routes A/V while it holds a non-null instance, so a
 * frame arriving before HELLO_ACK or before SUBSCRIBE is counted and dropped rather than fed to a
 * decoder with a stale or absent key.
 *
 * ## The wedge this class exists to prevent
 *
 * A/V is fed inline on the OCBM read thread, on purpose: [SeamPipe]'s blocking write IS the project's
 * closed-loop backpressure (lane backs up → USB fills → the box's read-gate stops pulling → the iPhone
 * throttles that encoder). Interposing a queue would replace a closed loop with an unbounded buffer and
 * merely move the head-of-line problem.
 *
 * That design has exactly one hazard. `HevcRenderer.consume` `break`s out of its loop on an implausible
 * seam length — a dead consumer — while the pipe is still open, and `SeamPipe.write` only returns false
 * once the pipe is **closed**. A dead consumer plus an open pipe therefore blocks the USB read thread
 * forever, taking every other channel down with it.
 *
 * The fix is ownership, not another thread: **whoever owns a pipe also owns its consumer thread and
 * closes the pipe in that thread's `finally`.** [runConsumer] is the only way to start a consumer here,
 * and it enforces exactly that.
 *
 * ## Scoping
 *
 * Audio and metadata are **session**-scoped (as in the reference implementation, where the players are
 * activity-scoped and only the video renderer is surface-scoped). Video is different: `HevcRenderer`
 * binds its Surface at construction, so its pipe and thread are **surface**-scoped and are owned by the
 * caller's video epoch. The [videoSeam] stays here because it holds the session's decrypt key —
 * see [VideoSeam.attach].
 */
class OcbmAvLanes(
    private val log: ProbeLog.Logger,
    onKeyframeNeeded: () -> Unit,
    onMetadata: (marker: Int, payload: ByteArray) -> Unit,
    /** First video key of this generation — the session layer's "device connected" edge. */
    onVideoKeyed: () -> Unit = {},
    /**
     * Sized to absorb a burst without unbounded growth. Note `queueDepth` (512 messages) binds long
     * before these byte caps for AAC — an ADTS frame is a few hundred bytes — so the effective media
     * buffer is ~11 s of audio, not the ~50 s the byte figure alone suggests.
     */
    mediaCapacityBytes: Int = 2 * 1024 * 1024,
    voiceCapacityBytes: Int = 1 * 1024 * 1024,
) {
    /** Session-scoped: holds the per-stream key and the frame sequence counter. */
    val videoSeam = VideoSeam(null, log, onKeyframeNeeded).apply { onKey = onVideoKeyed }

    /** ONE instance for both audio channels — the scid key and format tables are shared. */
    val mediaPipe = SeamPipe(mediaCapacityBytes)
    val voicePipe = SeamPipe(voiceCapacityBytes)
    val audioSeam = AudioSeam(mediaPipe, voicePipe, log)

    val metadataSeam = MetadataSeam(log, onMetadata)

    private val threads = ArrayList<Thread>()
    private val closed = AtomicBoolean(false)

    /**
     * Run [body] against [pipe] on its own daemon thread, closing the pipe when it returns.
     *
     * The close is the point: every renderer's `consume` releases its codec in its own `finally` and
     * only its own thread may do so, and a consumer that returns early must not leave a producer parked
     * on a pipe nobody will drain. Both properties are satisfied here and nowhere else.
     */
    fun runConsumer(
        name: String,
        pipe: SeamPipe,
        body: (InputStream) -> Unit,
    ): Thread {
        check(!closed.get()) { "runConsumer after close() — the thread would never be joined" }
        val t =
            Thread({
                try {
                    body(pipe)
                } catch (t: Throwable) {
                    log.e("$name consumer died: ${t.javaClass.simpleName}: ${t.message}")
                } finally {
                    pipe.close()
                    log.i("$name consumer ended")
                }
            }, name)
        t.isDaemon = true
        synchronized(threads) { threads.add(t) }
        t.start()
        return t
    }

    // ---- fed from OcbmClient.dispatch(), on the OCBM read thread ONLY ---------------------------
    // Not thread-safe by design: the seams keep unsynchronized reassembly buffers, and confining every
    // feed to the single read thread is what makes that safe without a lock on the hot path.

    fun feedVideo(p: ByteArray) = videoSeam.feed(p)

    fun feedMedia(p: ByteArray) = audioSeam.feedMedia(p)

    fun feedVoice(p: ByteArray) = audioSeam.feedVoice(p)

    fun feedMetadata(p: ByteArray) = metadataSeam.feed(p)

    /**
     * Close every pipe, then join the consumers.
     *
     * Order matters: closing first unblocks a read thread parked in a seam write and gives each
     * consumer EOF, so the joins can actually complete instead of timing out.
     */
    fun close() {
        if (!closed.compareAndSet(false, true)) return
        videoSeam.attach(null)
        mediaPipe.close()
        voicePipe.close()
        val snapshot = synchronized(threads) { threads.toList() }
        for (t in snapshot) {
            runCatching { t.join(1500) }
            if (t.isAlive) log.w("consumer ${t.name} did not exit within 1500 ms")
        }
        log.i("A/V lanes closed — ${statsLine()}")
    }

    fun statsLine(): String =
        "vid=${videoSeam.framesOut.get()}/${videoSeam.decryptOk.get()}/${videoSeam.decryptFail.get()}" +
            "/${videoSeam.gaps.get()} nopipe=${videoSeam.framesDroppedNoPipe.get()} " +
            "keys=${videoSeam.keysReceived.get()} unkeyed=${videoSeam.unkeyed.get()} " +
            "aud=${audioSeam.decryptOk.get()}/${audioSeam.decryptFail.get()}/${audioSeam.unkeyed.get()} " +
            "meta=${metadataSeam.messagesOut.get()}/${metadataSeam.resyncBytes.get()} " +
            "pipe=${mediaPipe.residentBytes()}/${voicePipe.residentBytes()}"
}
