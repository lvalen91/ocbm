package com.carlink.ocbm.seam

import java.io.InputStream
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * A bounded, blocking byte pipe that presents itself as an [InputStream].
 *
 * This is the join between the OCBM seam layer and the three proven renderers (`HevcRenderer`,
 * `VoiceRouter`, `AacPlayer`). Those all have the same shape — `consume(ins: InputStream)`, blocking,
 * owning their codec for the life of the call on their own thread — because they were written against
 * the box's *legacy on-box-decrypt* TCP seams. Handing them one of these instead of a socket means the
 * decrypt path can feed them **without touching a line of renderer code**: they cannot tell the
 * difference, which also makes an A/B against the box's own legacy seam a one-line change.
 *
 * ## Why bounded, and why blocking
 *
 * The cap is the project's closed-loop backpressure, not a buffer-size tuning knob. When a lane backs
 * up, [write] blocks the OCBM read thread; the USB pipe fills; the box's per-stream read-gate stops
 * pulling; the iPhone throttles *that* encoder. That is the designed behaviour
 * (`ccpa_custom/docs/16`, and the 24 MB lane cap in the macOS host's `OCBMAVDecrypt.swift:160-183`) —
 * the alternative, an unbounded queue, converts a transient decode stall into unbounded host memory
 * growth and a latency spike that never recovers.
 *
 * Queueing whole messages rather than bytes keeps a producer write O(1) and never splits a message
 * across two [write] calls, so a slow consumer can never observe a torn header.
 *
 * ## Lifetime
 *
 * [close] makes pending and subsequent reads return EOF, which is what every renderer treats as "seam
 * ended" — they release their codec and return from `consume` on their own thread, which is the only
 * thread allowed to release it. Closing is therefore the correct and complete way to stop a lane.
 */
class SeamPipe(
    /**
     * Maximum bytes resident in the queue before [write] blocks. Sized per lane by the caller: a video
     * lane must hold at least one whole keyframe (a 2400x960 HEVC IDR is ~200-400 KB, and the wire
     * permits up to [VideoSeam.MAX_MESSAGE]), an audio lane needs far less.
     */
    private val capacityBytes: Int,
    /** Bounds the number of queued messages independently of their size. */
    queueDepth: Int = 512,
) : InputStream() {
    private val q = ArrayBlockingQueue<ByteArray>(queueDepth)
    private val resident = AtomicLong(0)
    private val closed = AtomicBoolean(false)

    /** Bytes that never entered the pipe because it was closed. Nonzero after teardown is normal. */
    val dropped = AtomicLong(0)

    private var cur: ByteArray? = null
    private var curOff = 0

    /**
     * Offer one complete message. Blocks while the pipe is over [capacityBytes] — that block IS the
     * backpressure. Returns false only if the pipe is closed, so a caller can stop a drain loop.
     */
    fun write(msg: ByteArray): Boolean {
        // A zero-length message would surface from read() as a 0-return with len > 0, which every
        // renderer's readFully treats as EOF — killing the consumer and, through runConsumer's finally,
        // the whole lane. No current producer can emit one, so this is a guard against a future one.
        if (msg.isEmpty()) return true
        if (closed.get()) {
            dropped.addAndGet(msg.size.toLong())
            return false
        }
        // Admission is checked before the put, then the put itself blocks on queueDepth. Waiting on a
        // condition here would need a lock the reader also takes on the hot path; a short poll loop is
        // simpler and only ever spins when the consumer is already the bottleneck.
        while (resident.get() > capacityBytes) {
            if (closed.get()) {
                dropped.addAndGet(msg.size.toLong())
                return false
            }
            Thread.sleep(1)
        }
        resident.addAndGet(msg.size.toLong())
        // Bounded offer rather than put(): a closed pipe with a consumer that has already returned
        // would otherwise park the read thread forever.
        while (!q.offer(msg, 50, TimeUnit.MILLISECONDS)) {
            if (closed.get()) {
                resident.addAndGet(-msg.size.toLong())
                dropped.addAndGet(msg.size.toLong())
                return false
            }
        }
        return true
    }

    /** Bytes currently queued — surfaced so a lane can report backpressure rather than hide it. */
    fun residentBytes(): Long = resident.get()

    override fun read(): Int {
        val b = ByteArray(1)
        return if (read(b, 0, 1) == 1) b[0].toInt() and 0xFF else -1
    }

    override fun read(
        dst: ByteArray,
        off: Int,
        len: Int,
    ): Int {
        if (len == 0) return 0
        val src = fill() ?: return -1
        val n = minOf(len, src.size - curOff)
        System.arraycopy(src, curOff, dst, off, n)
        curOff += n
        if (curOff >= src.size) {
            cur = null
            curOff = 0
            resident.addAndGet(-src.size.toLong())
        }
        return n
    }

    /** Block until a message is available, or return null at EOF. */
    private fun fill(): ByteArray? {
        cur?.let { return it }
        while (true) {
            // Poll rather than take() so a close() while parked still surfaces EOF promptly.
            val m = q.poll(100, TimeUnit.MILLISECONDS)
            if (m != null) {
                cur = m
                curOff = 0
                return m
            }
            if (closed.get() && q.isEmpty()) return null
        }
    }

    /**
     * `resident` still counts the whole in-hand message until it is fully consumed, so the unread
     * remainder is `resident - curOff` — NOT `(cur.size - curOff) + resident`, which double-counts
     * `cur`. Clamped at zero because a `close()` racing an in-flight read can leave `resident`
     * transiently negative, and `InputStream.available()` may never report a negative.
     */
    override fun available(): Int = (resident.get() - curOff).coerceAtLeast(0L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

    override fun close() {
        closed.set(true)
        q.clear()
        resident.set(0)
    }
}
