package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import com.carlink.ocbm.Ocbm
import java.util.concurrent.atomic.AtomicLong

/**
 * `CH_METADATA` → now-playing JSON, album art, and inbound `/command` plists.
 *
 * Framing is `[u32 BE "META"][u32 BE len][marker][payload]`, `len = 1 + payload.size` — the box-side
 * producer is `iap2-core/src/metadata.rs:62-72`. Unlike the A/V seams these payloads are **plaintext**:
 * the box decrypts the control connection and the host is the trusted consumer over the app-owned USB
 * link, so there is no key and no decrypt here.
 *
 * Markers: `META_CMD` 0x01 (binary plist of an inbound iPhone `/command`), `META_JSON` 0x02 (one JSON
 * object with a `kind` field), `META_ARTWORK` 0x03 (`[id u8][JPEG]`), `META_CORNERMASK` 0x04.
 *
 * ## Two rules worth stating
 *
 * **Resync is byte-wise on the magic**, exactly as in the video seam and for the same reason: this rides
 * a channel with no retransmission, so a torn stream must re-find a boundary rather than give up. A
 * length that is zero or implausible is treated as a coincidental magic, not as a message.
 *
 * **[onMessage] fires on the OCBM read thread.** The contract is *parse the framing inline, hand off
 * immediately* — JSON parsing and JPEG decode belong on the app's own thread. Blocking here blocks
 * every other channel, including A/V.
 */
class MetadataSeam(
    private val log: ProbeLog.Logger,
    private val onMessage: (marker: Int, payload: ByteArray) -> Unit,
) {
    companion object {
        /** Album art is the large case; a 16 MB ceiling is far above any real artwork or plist. */
        const val MAX_MESSAGE: Int = 16 * 1024 * 1024
        private const val COMPACT_THRESHOLD = 1 shl 16
    }

    val messagesOut = AtomicLong(0)
    val resyncBytes = AtomicLong(0)

    private var buf = ByteArray(1 shl 14)
    private var end = 0
    private var start = 0

    fun feed(payload: ByteArray) {
        compact()
        ensure(payload.size)
        System.arraycopy(payload, 0, buf, end, payload.size)
        end += payload.size
        drain()
    }

    private fun ensure(extra: Int) {
        if (end + extra <= buf.size) return
        if (start > 0) {
            System.arraycopy(buf, start, buf, 0, end - start)
            end -= start
            start = 0
        }
        if (end + extra > buf.size) {
            var cap = buf.size
            while (cap < end + extra) cap = cap shl 1
            buf = buf.copyOf(cap)
        }
    }

    /** Lazy compaction — never per message; a front-drain per message is O(n^2). */
    private fun compact() {
        if (start >= end) {
            start = 0
            end = 0
            return
        }
        if (start > COMPACT_THRESHOLD) {
            System.arraycopy(buf, start, buf, 0, end - start)
            end -= start
            start = 0
        }
    }

    private fun be32(off: Int): Int =
        ((buf[off].toInt() and 0xFF) shl 24) or ((buf[off + 1].toInt() and 0xFF) shl 16) or
            ((buf[off + 2].toInt() and 0xFF) shl 8) or (buf[off + 3].toInt() and 0xFF)

    private fun magicAt(off: Int): Boolean {
        val m = Ocbm.META_SEAM_MAGIC
        return off + 4 <= end &&
            buf[off] == m[0] &&
            buf[off + 1] == m[1] &&
            buf[off + 2] == m[2] &&
            buf[off + 3] == m[3]
    }

    private fun drain() {
        while (true) {
            if (end - start < 8) return // need [magic 4][len 4]
            if (!magicAt(start)) {
                start++
                resyncBytes.incrementAndGet()
                continue
            }
            val mlen = be32(start + 4)
            // Zero or implausible length means this "META" was payload bytes that happened to spell the
            // magic, not a header. Step one byte rather than trusting it.
            if (mlen <= 0 || mlen > MAX_MESSAGE) {
                start++
                resyncBytes.incrementAndGet()
                continue
            }
            if (end - start < 8 + mlen) return // wait for the rest
            // mlen counts the marker, so a 1-byte message carries no payload at all — consume it but
            // do not deliver an empty record.
            if (mlen > 1) {
                val marker = buf[start + 8].toInt() and 0xFF
                val payload = buf.copyOfRange(start + 9, start + 8 + mlen)
                messagesOut.incrementAndGet()
                try {
                    onMessage(marker, payload)
                } catch (t: Throwable) {
                    // A consumer fault must not tear down the read thread and with it every A/V lane.
                    log.e("metadata consumer threw ${t.javaClass.simpleName}: ${t.message}")
                }
            }
            start += 8 + mlen
            compact()
        }
    }
}
