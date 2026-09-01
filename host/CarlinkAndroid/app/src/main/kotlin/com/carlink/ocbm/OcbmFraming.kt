package com.carlink.ocbm

/**
 * OCBM framing — a byte-faithful port of the `Header` / `frame()` / `Reassembler` trio in
 * `ccpa_custom/crates/ocbm-proto/src/lib.rs`.
 *
 * Header layout (all multi-byte fields little-endian):
 *
 *     off 0  magic   u32 = 0x4F43424D    resync marker (wire bytes 4D 42 43 4F)
 *     off 4  length  u32                 PAYLOAD bytes only — the 16-byte header is not counted
 *     off 8  channel u16
 *     off 10 flags   u8                  bit0 SOM, bit1 EOM
 *     off 11 hcheck  u8                  XOR of header bytes 0..=10
 *     off 12 seq     u32                 per-endpoint counter; debug only, no receiver validates it
 *     off 16 payload [length]
 *
 * `seq` is deliberately outside the checksum, and there is no payload checksum at all.
 */
object Framing {
    fun hcheck(
        buf: ByteArray,
        off: Int,
    ): Byte {
        var a = 0
        for (i in 0 until 11) a = a xor (buf[off + i].toInt() and 0xFF)
        return a.toByte()
    }

    private fun putIntLe(
        buf: ByteArray,
        off: Int,
        v: Int,
    ) {
        buf[off] = (v and 0xFF).toByte()
        buf[off + 1] = ((v ushr 8) and 0xFF).toByte()
        buf[off + 2] = ((v ushr 16) and 0xFF).toByte()
        buf[off + 3] = ((v ushr 24) and 0xFF).toByte()
    }

    private fun getIntLe(
        buf: ByteArray,
        off: Int,
    ): Int =
        (buf[off].toInt() and 0xFF) or
            ((buf[off + 1].toInt() and 0xFF) shl 8) or
            ((buf[off + 2].toInt() and 0xFF) shl 16) or
            ((buf[off + 3].toInt() and 0xFF) shl 24)

    private fun getShortLe(
        buf: ByteArray,
        off: Int,
    ): Int = (buf[off].toInt() and 0xFF) or ((buf[off + 1].toInt() and 0xFF) shl 8)

    /**
     * Build one complete frame. An over-MAX_PAYLOAD length field makes the box's reassembler treat
     * the header as junk and byte-resync through the ENTIRE payload — a stream tear, not a dropped
     * message. Never emit one.
     */
    fun frame(
        channel: Int,
        flags: Byte,
        seq: Int,
        payload: ByteArray,
    ): ByteArray {
        require(payload.size <= Ocbm.MAX_PAYLOAD) {
            "payload ${payload.size} exceeds MAX_PAYLOAD ${Ocbm.MAX_PAYLOAD} — would tear the stream"
        }
        val out = ByteArray(Ocbm.HDR_LEN + payload.size)
        putIntLe(out, 0, Ocbm.MAGIC)
        putIntLe(out, 4, payload.size)
        out[8] = (channel and 0xFF).toByte()
        out[9] = ((channel ushr 8) and 0xFF).toByte()
        out[10] = flags
        out[11] = hcheck(out, 0)
        putIntLe(out, 12, seq)
        System.arraycopy(payload, 0, out, Ocbm.HDR_LEN, payload.size)
        return out
    }

    // No `seq`: it is debug-only on the wire and no receiver validates it, so parsing it into a
    // field nothing reads is pure noise on the hot path.
    class Header(
        val length: Int,
        val channel: Int,
        val flags: Byte,
    )

    /** Parse and validate (magic + hcheck) a 16-byte header at [off]. */
    fun parseHeader(
        buf: ByteArray,
        off: Int,
        avail: Int,
    ): Header? {
        if (avail < Ocbm.HDR_LEN) return null
        if (getIntLe(buf, off) != Ocbm.MAGIC) return null
        if (buf[off + 11] != hcheck(buf, off)) return null
        return Header(getIntLe(buf, off + 4), getShortLe(buf, off + 8), buf[off + 10])
    }
}

/** One decoded frame handed up out of the [Reassembler]. No `flags`: the dispatcher routes on
 *  channel alone, and only F_BOTH is ever sent. */
class OcbmFrame(
    val channel: Int,
    val payload: ByteArray,
)

/**
 * Byte-stream → frame reassembler. Bulk USB transfers and OCBM frames are completely decoupled: a
 * single read can contain a partial header, several whole frames, or junk. Push every raw chunk and
 * drain [next] in a loop.
 *
 * Resync is byte-wise, exactly as in the Rust original: a bad magic, a bad hcheck, or an implausible
 * length advances the cursor by ONE byte and retries. A valid header with an incomplete payload does
 * NOT advance, so headers survive across reads.
 */
class Reassembler {
    private var buf = ByteArray(1 shl 16)
    private var end = 0 // bytes valid in buf
    private var start = 0 // read cursor

    /** Counts byte-wise resync steps — a nonzero value means the stream was torn or desynced. */
    var resyncBytes: Long = 0
        private set

    fun push(
        data: ByteArray,
        len: Int,
    ) {
        ensure(len)
        System.arraycopy(data, 0, buf, end, len)
        end += len
    }

    private fun ensure(extra: Int) {
        if (end + extra <= buf.size) return
        // Reclaim the consumed prefix first; only grow if that isn't enough.
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

    /** Pop the next complete frame, or null if more bytes are needed. */
    fun next(): OcbmFrame? {
        while (true) {
            val avail = end - start
            if (avail < Ocbm.HDR_LEN) {
                compact()
                return null
            }
            val h = Framing.parseHeader(buf, start, avail)
            if (h == null) {
                start += 1
                resyncBytes += 1
                continue
            }
            // Compare UNSIGNED. Kotlin's Int is signed, so a corrupt length with bit 31 set parses
            // negative and would sail past a plain `> MAX_PAYLOAD` test — then `total` goes <= 15 and
            // copyOfRange throws, killing the read thread instead of resyncing. Rust is immune here
            // because it widens to usize. This is precisely the torn-stream case the loop exists for.
            val plen = h.length.toLong() and 0xFFFF_FFFFL
            if (plen > Ocbm.MAX_PAYLOAD) {
                start += 1
                resyncBytes += 1
                continue
            }
            val total = Ocbm.HDR_LEN + plen.toInt()
            if (avail < total) {
                compact()
                return null
            }
            val payload = buf.copyOfRange(start + Ocbm.HDR_LEN, start + total)
            start += total
            compact()
            return OcbmFrame(h.channel, payload)
        }
    }

    private fun compact() {
        if (start == end) {
            start = 0
            end = 0
            return
        }
        if (start > Ocbm.HDR_LEN + Ocbm.MAX_PAYLOAD) {
            System.arraycopy(buf, start, buf, 0, end - start)
            end -= start
            start = 0
        }
    }
}
