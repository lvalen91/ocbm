package zeno.gmccpa.av

import java.lang.ref.WeakReference
import java.util.concurrent.atomic.AtomicReference

/**
 * The process-wide arm slot for the bench-only `mic_inject` verb (plan v5.9 MIC, rev b R24 / rev c
 * C7-C9). Pure and Android-free.
 *
 * **Who writes, who reads.** The ONLY caller of [arm] is the bench flavor's
 * `zeno.gmccpa.bench.BenchHooks` (src/bench). A release build compiles the src/nobench stub
 * instead, which never calls [arm], so in a release APK this slot is always empty and
 * [nextChunk] is one atomic read returning null. The only reader/advancer is MicUplink's capture
 * thread, at its existing send point: when a chunk is due, it overwrites the AudioRecord chunk in
 * place before `send()`. AudioRecord keeps running and draining, so the pacing is the recorder's own
 * clock — one 20 ms chunk out per 20 ms chunk read; no timer, no sleep. The real-mic samples read
 * while a clip plays are discarded by the caller, never sent and never logged.
 *
 * **Rules.**
 *  - ONE clip at a time ([arm] replaces it atomically; `file2` was withdrawn in rev c, C9).
 *  - Start rule (C7): an UNSTARTED clip starts at the next chunk of ANY capture whose format is
 *    16000 Hz / 1 ch — including one already open when the clip was armed — and binds to it. For
 *    any other format it is not consumed (logged once per capture).
 *  - Binding (C8) is by the capture OBJECT's identity (a [WeakReference] compared with `===`), never
 *    by MicUplink's per-instance capture counter, which restarts at 1 in every CarPlayActivity
 *    generation. Once started, a clip continues only on its own capture; a call from any other
 *    capture means that capture ended (gate off, read error, format change), and the remainder is
 *    discarded — never continued on a later gate.
 *  - An unstarted clip that outlived its expiry is dropped at its next read. A started clip is bound
 *    to its capture and cannot reach a later gate, so it is not cut by the expiry.
 *  - When the clip is exhausted it leaves the slot and the next chunk is the real mic again.
 *
 * Clip layout: `lead` zero bytes + the WAV's PCM + `tail` zero bytes, cut into 640-byte chunks
 * (20 ms of 16000/1 S16LE); the last chunk is zero-padded.
 */
object MicInject {

    const val RATE = 16000
    const val CHANNELS = 1
    /** Must equal MicUplink.CHUNK_MS: one injected chunk replaces exactly one recorded chunk. */
    const val CHUNK_MS = 20
    const val BYTES_PER_MS = RATE / 1000 * 2 * CHANNELS
    const val CHUNK_BYTES = BYTES_PER_MS * CHUNK_MS

    internal class Owner(obj: Any, val tag: Int) { val ref = WeakReference(obj) }

    class Clip internal constructor(
        internal val pcm: ByteArray, val leadMs: Int, val tailMs: Int, val expiresAt: Long,
    ) {
        internal val leadBytes = leadMs * BYTES_PER_MS
        val totalBytes = leadBytes + pcm.size + tailMs * BYTES_PER_MS
        val chunks = (totalBytes + CHUNK_BYTES - 1) / CHUNK_BYTES
        val clipMs: Int get() = pcm.size / BYTES_PER_MS
        internal val owner = AtomicReference<Owner?>(null)
        /** Chunks handed out. Written only by the owning capture's thread. */
        @Volatile internal var cursor = 0
        @Volatile internal var mismatchLoggedFor: WeakReference<Any>? = null
    }

    private val q = AtomicReference<List<Clip>>(emptyList())

    /** Log sink; set by the bench hook. No audio content is ever passed to it. */
    @Volatile var log: (String) -> Unit = {}
    /** Monotonic ms. Injectable for tests. */
    @Volatile var clock: () -> Long = { System.nanoTime() / 1_000_000L }

    /** Replace whatever is armed (pending or running) with ONE new clip. Bench flavor only. */
    fun arm(pcm: ByteArray, leadMs: Int, tailMs: Int, expireS: Int, now: Long = clock()): Clip {
        val c = Clip(pcm, leadMs, tailMs, now + expireS * 1000L)
        q.set(listOf(c))
        return c
    }

    /** Drop anything armed. True if something was. */
    fun clear(): Boolean = q.getAndSet(emptyList()).isNotEmpty()

    fun isArmed(): Boolean = q.get().isNotEmpty()

    /**
     * The capture-thread entry. Idle cost: one atomic read; the clock is read only when a clip is
     * armed. [owner] is the Capture object (identity), [tag] its number for the log only; [rate] and
     * [ch] are the capture's own AudioRecord format and [len] its chunk size — a clip is consumed only
     * when all three match (16000, 1, 640), so the returned array always fits the caller's buffer.
     */
    fun nextChunk(owner: Any, tag: Int, rate: Int, ch: Int, len: Int): ByteArray? =
        if (q.get().isEmpty()) null else nextChunk(owner, tag, rate, ch, len, clock())

    fun nextChunk(owner: Any, tag: Int, rate: Int, ch: Int, len: Int, now: Long): ByteArray? {
        val list = q.get()
        val c = list.firstOrNull() ?: return null
        val bound = c.owner.get()
        if (bound == null) {
            if (now >= c.expiresAt) {
                if (q.compareAndSet(list, emptyList())) log("mic_inject: expired")
                return null
            }
            if (rate != RATE || ch != CHANNELS || len != CHUNK_BYTES) {
                if (c.mismatchLoggedFor?.get() !== owner) {
                    c.mismatchLoggedFor = WeakReference(owner)
                    log("mic_inject: gate is $rate $ch, clip is $RATE $CHANNELS — not consumed (capture #$tag)")
                }
                return null
            }
            // Lost the bind to another capture in the same instant: real mic for this chunk.
            if (!c.owner.compareAndSet(null, Owner(owner, tag))) return null
            log("mic_inject: clip 1 started on capture #$tag")
        } else if (bound.ref.get() !== owner || rate != RATE || ch != CHANNELS || len != CHUNK_BYTES) {
            // Bound to a capture that is not this one: that capture ended. Never continued here.
            if (q.compareAndSet(list, emptyList())) {
                log("mic_inject: clip 1 discarded (capture #${bound.tag} ended at chunk ${c.cursor})")
            }
            return null
        }
        val k = c.cursor
        val out = ByteArray(CHUNK_BYTES)
        val from = k.toLong() * CHUNK_BYTES
        val start = maxOf(from, c.leadBytes.toLong())
        val stop = minOf(from + CHUNK_BYTES, c.leadBytes.toLong() + c.pcm.size)
        if (start < stop) {
            System.arraycopy(c.pcm, (start - c.leadBytes).toInt(), out, (start - from).toInt(), (stop - start).toInt())
        }
        c.cursor = k + 1
        if (k + 1 >= c.chunks && q.compareAndSet(list, emptyList())) {
            log("mic_inject: clip 1 done (${c.chunks} chunks) — real mic from the next chunk")
        }
        return out
    }
}
