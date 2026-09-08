package zeno.gmccpa.av

import zeno.gmccpa.ProbeLog
import java.io.InputStream
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * The `:9004` metadata seam framer.
 *
 * Wire format, from the producer (`ccpa_custom/crates/vendor/iap2-core/src/metadata.rs:41-43,63-69`):
 *
 *     [u32 BE 0x4D455441 "META"][u32 BE len][marker u8][payload]     len = 1 + payload.size
 *
 * Plaintext — there is no per-message key here, unlike the A/V lanes. The core already owns the
 * decrypted control connection, and in this app it runs IN THIS PROCESS, so the "seam" is a loopback
 * hop between two halves of the same APK.
 *
 * ## Why a magic, and why we resync on it instead of dropping the connection
 * The A/V seams are length-prefixed only, so a desync there is unrecoverable and [VoiceRouter] treats
 * one as fatal to the connection. This lane carries a 4-byte magic precisely so a torn stream can find
 * the next boundary, so a bad length here steps ONE byte and keeps looking. A "META" that appears
 * inside a JPEG is not a framing error, it is a coincidence, and the length sanity check below is what
 * tells the two apart.
 *
 * ## Why the consumer must never block
 * The producer writes under its `SINK` mutex with a 2 s timeout (`metadata.rs:115-127`), and that mutex
 * is shared with the iAP2 reader. Because the core is in-process here, that reader is OUR thread too —
 * so a slow consumer on this seam does not merely delay a metadata card, it stalls iAP2 ingest for the
 * whole session. Parse inline (cheap), hand off anything expensive (JPEG decode, MediaSession writes).
 * The same reasoning is why the listener must always accept AND drain: an accepted-but-unread socket
 * costs the producer its full 2 s per record.
 */
class MetadataSeam(
    private val log: ProbeLog.Logger,
    private val onMessage: (marker: Int, payload: ByteArray) -> Unit,
) {
    companion object {
        private val MAGIC = byteArrayOf(0x4D, 0x45, 0x54, 0x41) // "META"

        /** Inbound iPhone `/command` plist. Not consumed here — see [NowPlayingState.dispatch]. */
        const val META_CMD = 0x01
        /** One UTF-8 JSON object with a `"kind"` discriminator. */
        const val META_JSON = 0x02
        /** `[artwork id u8][JPEG]`. */
        const val META_ARTWORK = 0x03
        /** `[u32 BE display width][PNG]`. Not consumed. */
        const val META_CORNERMASK = 0x04

        /**
         * Artwork is the large case — the producer caps a single transfer at 8 MiB
         * (`metadata.rs:127`). 16 MiB is far above any real JPEG or plist and is deliberately NOT the
         * video lane's 8 MiB bound: the two lanes carry different things and size independently.
         */
        const val MAX_MESSAGE = 16 * 1024 * 1024
        private const val COMPACT_THRESHOLD = 1 shl 16
        private const val READ_CHUNK = 1 shl 16
    }

    val messagesOut = AtomicLong(0)
    val resyncBytes = AtomicLong(0)
    val bytesIn = AtomicLong(0)

    private var buf = ByteArray(1 shl 14)
    private var start = 0
    private var end = 0

    /**
     * Pump one accepted connection until it closes or [gen] flips. Same blocking-read shape as the
     * other seams; the framer is what turns arbitrary chunks into whole messages.
     */
    fun consume(gen: AtomicBoolean, ins: InputStream) {
        val chunk = ByteArray(READ_CHUNK)
        try {
            while (gen.get()) {
                val n = ins.read(chunk)
                if (n <= 0) break
                bytesIn.addAndGet(n.toLong())
                feed(chunk, n)
            }
        } catch (t: Throwable) {
            if (gen.get()) log.e("consume: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            // The producer re-dials per whole message (`metadata.rs:44-46`), so any partial tail held
            // across a disconnect belongs to a message that will be re-sent in full. Scanning it would
            // only manufacture a false resync.
            start = 0; end = 0
        }
    }

    fun feed(payload: ByteArray, n: Int = payload.size) {
        compact(); ensure(n)
        System.arraycopy(payload, 0, buf, end, n); end += n
        drain()
    }

    private fun ensure(extra: Int) {
        if (end + extra <= buf.size) return
        if (start > 0) { System.arraycopy(buf, start, buf, 0, end - start); end -= start; start = 0 }
        if (end + extra > buf.size) {
            var cap = buf.size
            while (cap < end + extra) cap = cap shl 1
            buf = buf.copyOf(cap)
        }
    }

    /** Lazy compaction. Draining from the front per message would be O(n^2) across a 100 KB artwork. */
    private fun compact() {
        if (start >= end) { start = 0; end = 0; return }
        if (start > COMPACT_THRESHOLD) {
            System.arraycopy(buf, start, buf, 0, end - start); end -= start; start = 0
        }
    }

    private fun be32(off: Int): Int =
        ((buf[off].toInt() and 0xFF) shl 24) or ((buf[off + 1].toInt() and 0xFF) shl 16) or
            ((buf[off + 2].toInt() and 0xFF) shl 8) or (buf[off + 3].toInt() and 0xFF)

    private fun magicAt(off: Int): Boolean =
        off + 4 <= end && buf[off] == MAGIC[0] && buf[off + 1] == MAGIC[1] &&
            buf[off + 2] == MAGIC[2] && buf[off + 3] == MAGIC[3]

    private fun drain() {
        while (true) {
            if (end - start < 8) return                       // need [magic 4][len 4]
            if (!magicAt(start)) { start++; resyncBytes.incrementAndGet(); continue }
            val mlen = be32(start + 4)
            // Zero or implausible length: this "META" was payload bytes that happened to spell the
            // magic. Step one byte rather than trusting it — the alternative is consuming megabytes
            // of a real message as if it were a header.
            if (mlen <= 0 || mlen > MAX_MESSAGE) { start++; resyncBytes.incrementAndGet(); continue }
            if (end - start < 8 + mlen) return                // wait for the rest
            // `len` COUNTS the marker byte, so a 1-byte message carries no payload: consume it and
            // deliver nothing rather than handing a consumer an empty array to misread.
            if (mlen > 1) {
                val marker = buf[start + 8].toInt() and 0xFF
                val payload = buf.copyOfRange(start + 9, start + 8 + mlen)
                messagesOut.incrementAndGet()
                // A consumer fault must not kill this connection. Losing the buffered tail would cost
                // the NEXT record too, and this lane is the only one whose producer never retries.
                try { onMessage(marker, payload) }
                catch (t: Throwable) { log.e("consumer threw ${t.javaClass.simpleName}: ${t.message}") }
            }
            start += 8 + mlen
            compact()
        }
    }
}
