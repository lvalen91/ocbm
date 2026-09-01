package com.carlink.ocbm.seam

import com.carlink.logging.ProbeLog
import java.util.concurrent.atomic.AtomicLong

/**
 * `CH_VIDEO` / `CH_ALT_VIDEO` → Annex-B, for the proven `HevcRenderer`.
 *
 * Feed raw OCBM payload bytes with [feed]; complete access units come out the other side as
 * `[u32 BE len][Annex-B]` messages on [pipe] — **exactly the framing `HevcRenderer.consume` already
 * expects** (it was written against the box's legacy on-box-decrypt seam, `forward_screen`). So this
 * class is a transcoder from the forward-encrypted v2 seam into that legacy shape, and the renderer
 * needs no change at all.
 *
 * ## Wire
 *
 * A byte stream of `[u32 BE len][SEAM_MAGIC "SEAV"][marker][payload]`:
 *  - `marker 0x00` — `[key 32][scid 8 LE]`, the per-stream ChaCha20 key, re-sent on every seam
 *    reconnect. Always accept a new key and reset the sequence state with it.
 *  - `marker 0x01` — `[seq u64 LE][hdr 128][body]`. `hdr[4]` is the opcode: 1 = **plaintext**
 *    avcC/hvcC codec config (no decrypt, no counter), 0 = an encrypted access unit.
 *
 * ## Three details that are each their own bug if got wrong
 *
 *  1. **The message cap is 16 MB, not `MAX_PAYLOAD`.** One video message is reassembled across many
 *     64 KiB OCBM frames, so a large IDR is legitimately far bigger than a frame. The macOS host
 *     originally capped at `2 * maxPayload` = 128 KB, which silently rejected every 4K keyframe and
 *     left the decoder permanently poisoned with P-frames (`OCBMAVDecrypt.swift:104-109`).
 *  2. **`seq` is the decrypt nonce, so it advances even on failure.** The box consumed that nonce.
 *     Freezing the counter on a bad frame makes the next good frame look like a second gap.
 *  3. **The config may be a full ISO sample entry, not a bare record.** iOS sends HEVC config as an
 *     `hvc1`/`hev1` VisualSampleEntry wrapping `hvcC` (observed live 2026-07-12). The macOS bridge
 *     unwraps only those two, which would black-screen a wireless session that fell back to H.264 —
 *     [unwrapSampleEntry] handles `avc1`/`avcC` as well.
 */
class VideoSeam(
    initialPipe: SeamPipe?,
    private val log: ProbeLog.Logger,
    /**
     * Invoked when a frame gap or a decrypt failure is detected. The caller should ask the box for an
     * IDR (`CH_INPUT` `INPUT_KEYFRAME`, or `INPUT_KEYFRAME_ALT` for the cluster lane) — throttled, as
     * a burst of gaps must not become a burst of ForceKeyFrame.
     */
    private val onGap: () -> Unit,
) {
    /**
     * Invoked once per key message. The FIRST key is the earliest proof that a paired, keyed session
     * exists — it is what the session layer raises "device connected" on, and nothing else on the wire
     * says it as early. Fires on the OCBM read thread: hand off, do not block.
     *
     * A property rather than a constructor parameter on purpose: a defaulted LAST parameter would
     * silently capture the trailing lambda at every existing `VideoSeam(pipe, log) { ... }` call site,
     * rebinding what callers wrote as `onGap`.
     */
    @Volatile
    var onKey: (() -> Unit)? = null

    /**
     * Invoked once per codec-config message with `true` for HEVC and `false` for H.264.
     *
     * Exists so a consumer can NAME a codec mismatch instead of showing a black screen. The Pi port
     * runs an HEVC-only decoder; if the box was launched without the app-pushed config it negotiates
     * H.264, every frame decrypts perfectly, and nothing renders — with no error anywhere, because
     * at the seam layer nothing is wrong. Fires on the read thread: hand off, do not block.
     *
     * Defaulted to null and additive, so the OCBM host is unaffected.
     */
    @Volatile
    var onCodec: ((Boolean) -> Unit)? = null

    companion object {
        /** Comfortably holds a 4K IDR. See note 1 in the class doc — do NOT reduce to a frame size. */
        const val MAX_MESSAGE: Int = 16 * 1024 * 1024
        private const val MIN_FRAME_MSG = 1 + 8 + SeamCrypto.HDR_LEN
    }

    val framesOut = AtomicLong(0)
    val decryptOk = AtomicLong(0)
    val decryptFail = AtomicLong(0)
    val gaps = AtomicLong(0)

    /** Access units produced while no pipe was attached (no surface). Counted, not an error. */
    val framesDroppedNoPipe = AtomicLong(0)

    /** Per-stream keys received. The first one marks the session as keyed. */
    val keysReceived = AtomicLong(0)

    /** Frames dropped because no key had arrived yet — distinguishes key-plumbing from crypto faults. */
    val unkeyed = AtomicLong(0)

    /**
     * The current output pipe, or null when there is no surface to decode to.
     *
     * **Swappable on purpose, and this is load-bearing.** `HevcRenderer`'s surface is bound at
     * construction, so its pipe and consumer thread live and die with the Surface — but the ChaCha20
     * key and the frame sequence counter live in *this* object and are scoped to the OCBM **session**.
     * Rebuilding the seam on a surface change would throw the key away, and the box only re-sends it
     * when its own seam reconnects — which a host-side surface change does not cause. The result would
     * be permanent decrypt failure after the first surface swap. So: one seam per session, one pipe per
     * surface, and [attach] joins them.
     */
    @Volatile
    private var pipe: SeamPipe? = initialPipe

    /** Point the seam at a new output, or at nothing. The caller owns the pipe's lifetime. */
    fun attach(p: SeamPipe?) {
        pipe = p
    }

    private var buf = ByteArray(1 shl 18)
    private var end = 0
    private var start = 0

    private var key: ByteArray? = null
    private var lastSeq = 0L
    private var haveSeq = false

    /** From the codec config: how many bytes prefix each NAL in the AVCC access units. */
    private var nalLengthSize = 4

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

    /** Lazy compaction, never per message — a front-drain per frame is O(n^2) on the hot path. */
    private fun compact() {
        if (start >= end) {
            start = 0
            end = 0
            return
        }
        if (start > (1 shl 18)) {
            System.arraycopy(buf, start, buf, 0, end - start)
            end -= start
            start = 0
        }
    }

    private fun be32(
        b: ByteArray,
        off: Int,
    ): Int =
        ((b[off].toInt() and 0xFF) shl 24) or ((b[off + 1].toInt() and 0xFF) shl 16) or
            ((b[off + 2].toInt() and 0xFF) shl 8) or (b[off + 3].toInt() and 0xFF)

    private fun magicAt(off: Int): Boolean =
        off + 4 <= end &&
            buf[off] == SeamCrypto.SEAM_MAGIC[0] &&
            buf[off + 1] == SeamCrypto.SEAM_MAGIC[1] &&
            buf[off + 2] == SeamCrypto.SEAM_MAGIC[2] &&
            buf[off + 3] == SeamCrypto.SEAM_MAGIC[3]

    private fun drain() {
        while (true) {
            if (end - start < 8) return // need [len 4][magic 4]
            val mlen = be32(buf, start)
            if (mlen < 5 || mlen > MAX_MESSAGE || !magicAt(start + 4)) {
                if (!resyncToMagic()) return
                continue
            }
            if (end - start < 4 + mlen) return // magic verified, so mlen is trustworthy
            // Strip [len][magic]; what remains is [marker][payload].
            handle(buf, start + 8, 4 + mlen - 8)
            start += 4 + mlen
            compact()
        }
    }

    /**
     * Re-align on the next `SEAM_MAGIC`, leaving the cursor on its 4-byte length prefix. Returns false
     * when no magic is buffered yet, keeping a short tail in case one straddles the next feed.
     */
    private fun resyncToMagic(): Boolean {
        var i = start + 5
        while (i + 4 <= end) {
            if (magicAt(i)) {
                start = i - 4
                return true
            }
            i++
        }
        if (end - start > 7) start = end - 7
        return false
    }

    private fun handle(
        b: ByteArray,
        off: Int,
        len: Int,
    ) {
        if (len < 1) return
        when (b[off].toInt() and 0xFF) {
            SeamCrypto.MARK_KEY -> {
                if (len < 33) return
                key = b.copyOfRange(off + 1, off + 33)
                haveSeq = false // a re-key is a new stream; the old counter means nothing
                keysReceived.incrementAndGet()
                log.i("received video key")
                runCatching { onKey?.invoke() }
            }
            SeamCrypto.MARK_FRAME -> {
                if (len < MIN_FRAME_MSG) return
                var seq = 0L
                for (k in 0 until 8) seq = seq or ((b[off + 1 + k].toLong() and 0xFF) shl (8 * k))
                val hdrOff = off + 9
                val opcode = b[hdrOff + 4].toInt() and 0xFF
                val bodyOff = hdrOff + SeamCrypto.HDR_LEN
                val bodyLen = len - MIN_FRAME_MSG
                if (opcode == SeamCrypto.OPCODE_CONFIG) {
                    onConfig(b.copyOfRange(bodyOff, bodyOff + bodyLen))
                } else if (opcode == SeamCrypto.OPCODE_FRAME) {
                    onFrame(b, hdrOff, bodyOff, bodyLen, seq)
                }
            }
        }
    }

    /** Plaintext avcC/hvcC — no decrypt, no counter. Emit the parameter sets as one Annex-B message. */
    private fun onConfig(raw: ByteArray) {
        val rec = unwrapSampleEntry(raw)
        if (rec == null || rec.isEmpty()) {
            log.w("video config: unrecognised record (${raw.size} B)")
            return
        }
        val parsed = if (isHvcC(rec)) parseHvcC(rec) else parseAvcC(rec)
        if (parsed == null) {
            log.w("video config: parse failed (${rec.size} B)")
            return
        }
        nalLengthSize = parsed.nalLengthSize
        runCatching { onCodec?.invoke(isHvcC(rec)) }
        log.i(
            "video config: ${if (isHvcC(rec)) "hvcC" else "avcC"}, ${parsed.paramSets.size} B " +
                "parameter sets, NAL length size $nalLengthSize",
        )
        emit(parsed.paramSets)
    }

    private fun onFrame(
        b: ByteArray,
        hdrOff: Int,
        bodyOff: Int,
        bodyLen: Int,
        seq: Long,
    ) {
        val k = key
        if (k == null) {
            unkeyed.incrementAndGet()
            return
        }
        if (haveSeq && seq != lastSeq + 1) {
            val n = gaps.incrementAndGet()
            // Throttled like the decrypt-failure log below, and for the same reason: onGap fires
            // per gap AND per decrypt failure, so a bad key would make this 60 logd writes per
            // second on the USB read thread.
            if (n == 1L || n % 300 == 0L) {
                log.i("video gap: seq $lastSeq -> $seq — resync + keyframe (gaps=$n)")
            }
            onGap()
        }
        val hdr = b.copyOfRange(hdrOff, hdrOff + SeamCrypto.HDR_LEN)
        val body = b.copyOfRange(bodyOff, bodyOff + bodyLen)
        val plain = SeamCrypto.openVideo(k, seq, hdr, body)
        if (plain != null) {
            decryptOk.incrementAndGet()
            emit(avccToAnnexB(plain, nalLengthSize))
        } else {
            val n = decryptFail.incrementAndGet()
            // A failed decrypt is a lost frame; the gap detector cannot fire for it because seq still
            // advances, so recover the same way.
            onGap()
            if (n == 1L || n % 300 == 0L) {
                log.e("video decrypt FAILED (ok=${decryptOk.get()} fail=$n) — requesting keyframe")
            }
        }
        // Advance even on failure — see note 2 in the class doc.
        lastSeq = seq
        haveSeq = true
    }

    /** Wrap in the renderer's `[u32 BE len][payload]` framing and hand it over. */
    private fun emit(annexB: ByteArray) {
        if (annexB.isEmpty()) return
        val p = pipe
        if (p == null) {
            framesDroppedNoPipe.incrementAndGet()
            return
        }
        val out = ByteArray(4 + annexB.size)
        out[0] = ((annexB.size ushr 24) and 0xFF).toByte()
        out[1] = ((annexB.size ushr 16) and 0xFF).toByte()
        out[2] = ((annexB.size ushr 8) and 0xFF).toByte()
        out[3] = (annexB.size and 0xFF).toByte()
        System.arraycopy(annexB, 0, out, 4, annexB.size)
        if (p.write(out)) framesOut.incrementAndGet()
    }

    // ---- codec configuration records -------------------------------------------------------------

    class VideoConfig(
        val nalLengthSize: Int,
        val paramSets: ByteArray,
    )

    private fun isHvcC(rec: ByteArray): Boolean = looksHevc(rec)

    /**
     * Discriminate hvcC from avcC structurally: an hvcC is at least 23 bytes and its numOfArrays walk
     * must land EXACTLY on the end of the record.
     *
     * This is a heuristic, not a decision procedure, and deliberately not what the box does — the box
     * parses as avcC and then tests whether the first emitted NAL is an H.264 parameter set. A valid
     * avcC whose bytes from 22 on happen to walk exactly to the end would be misread as HEVC here.
     * The walk landing exactly on the boundary makes that vanishingly unlikely, and a config arrives
     * about once per session, so the residual risk is accepted rather than unnoticed.
     */
    private fun looksHevc(rec: ByteArray): Boolean {
        if (rec.size < 23) return false
        var i = 22
        val numArrays = rec[i].toInt() and 0xFF
        i += 1
        var arrays = 0
        while (arrays < numArrays && i + 3 <= rec.size) {
            val numNalus = ((rec[i + 1].toInt() and 0xFF) shl 8) or (rec[i + 2].toInt() and 0xFF)
            i += 3
            var n = 0
            while (n < numNalus && i + 2 <= rec.size) {
                val l = ((rec[i].toInt() and 0xFF) shl 8) or (rec[i + 1].toInt() and 0xFF)
                i += 2 + l
                n++
            }
            if (n != numNalus) return false
            arrays++
        }
        return arrays == numArrays && i == rec.size
    }

    /**
     * Unwrap a QuickTime sample description to the nested configuration record.
     *
     * A bare record begins with configurationVersion = 1. Otherwise scan for an `hvcC`/`avcC` atom —
     * **including the `avc1` case**, which the macOS bridge omits (`OCBMAVBridge.swift:80` handles only
     * `hvc1`/`hev1`); on a wireless session that fell back to H.264 that omission is a black screen.
     */
    private fun unwrapSampleEntry(data: ByteArray): ByteArray? {
        if (data.isEmpty()) return null
        if (data[0].toInt() == 1) return data
        // Byte-wise scan for the atom tag rather than an atom-tree walk: the config sits nested inside
        // the sample entry at a depth that varies by wrapper, and a mis-stepped walk returns nothing
        // where a scan still finds it. The 4-byte size sits immediately before the tag.
        var i = 4
        while (i + 4 <= data.size) {
            val isHvcc =
                data[i] == 'h'.code.toByte() &&
                    data[i + 1] == 'v'.code.toByte() &&
                    data[i + 2] == 'c'.code.toByte() &&
                    data[i + 3] == 'C'.code.toByte()
            val isAvcc =
                data[i] == 'a'.code.toByte() &&
                    data[i + 1] == 'v'.code.toByte() &&
                    data[i + 2] == 'c'.code.toByte() &&
                    data[i + 3] == 'C'.code.toByte()
            if (isHvcc || isAvcc) {
                val size = be32(data, i - 4)
                val from = i + 4
                val to = if (size >= 8 && size <= data.size - (i - 4)) (i - 4) + size else data.size
                return if (to > from) data.copyOfRange(from, to) else null
            }
            i++
        }
        return null
    }

    private fun parseHvcC(rec: ByteArray): VideoConfig? {
        if (rec.size < 23) return null
        val nls = (rec[21].toInt() and 0x03) + 1
        val out = java.io.ByteArrayOutputStream(rec.size + 64)
        var i = 22
        val numArrays = rec[i].toInt() and 0xFF
        i++
        repeat(numArrays) {
            if (i + 3 > rec.size) return null
            val numNalus = ((rec[i + 1].toInt() and 0xFF) shl 8) or (rec[i + 2].toInt() and 0xFF)
            i += 3
            repeat(numNalus) {
                if (i + 2 > rec.size) return null
                val l = ((rec[i].toInt() and 0xFF) shl 8) or (rec[i + 1].toInt() and 0xFF)
                i += 2
                if (i + l > rec.size) return null
                out.write(byteArrayOf(0, 0, 0, 1))
                out.write(rec, i, l)
                i += l
            }
        }
        return VideoConfig(nls, out.toByteArray())
    }

    private fun parseAvcC(rec: ByteArray): VideoConfig? {
        if (rec.size < 7) return null
        val nls = (rec[4].toInt() and 0x03) + 1
        val out = java.io.ByteArrayOutputStream(rec.size + 32)
        var i = 5
        val numSps = rec[i].toInt() and 0x1F
        i++
        repeat(numSps) {
            if (i + 2 > rec.size) return null
            val l = ((rec[i].toInt() and 0xFF) shl 8) or (rec[i + 1].toInt() and 0xFF)
            i += 2
            if (i + l > rec.size) return null
            out.write(byteArrayOf(0, 0, 0, 1))
            out.write(rec, i, l)
            i += l
        }
        if (i >= rec.size) return VideoConfig(nls, out.toByteArray())
        val numPps = rec[i].toInt() and 0xFF
        i++
        repeat(numPps) {
            if (i + 2 > rec.size) return null
            val l = ((rec[i].toInt() and 0xFF) shl 8) or (rec[i + 1].toInt() and 0xFF)
            i += 2
            if (i + l > rec.size) return null
            out.write(byteArrayOf(0, 0, 0, 1))
            out.write(rec, i, l)
            i += l
        }
        return VideoConfig(nls, out.toByteArray())
    }

    /**
     * Length-prefixed AVCC/HVCC access unit → Annex-B, in one pass.
     *
     * With the usual 4-byte prefix the start code occupies exactly the bytes the length did, so this
     * is a copy with four bytes overwritten per NAL rather than a re-layout. A malformed length ends
     * the walk instead of throwing — a torn frame should cost one frame, not the read thread.
     */
    private fun avccToAnnexB(
        au: ByteArray,
        nls: Int,
    ): ByteArray {
        if (nls == 4) {
            // Patch IN PLACE. `au` is the array `doFinal` just returned and this is its only
            // reference (single call site in onFrame), so the defensive copy bought nothing and
            // cost a full-size Large-Object-Space allocation per frame — ~1.5 MB/s of GC churn
            // at 12 Mbps, which measurement showed is the real hot-path cost here.
            val out = au
            var i = 0
            while (i + 4 <= out.size) {
                val l = be32(out, i)
                // Compare against REMAINING space, never `i + 4 + l`: that sum overflows Int for a
                // large length, wraps negative, passes the guard, and the next be32() indexes way out
                // of bounds — an AIOOBE thrown on the USB read thread, killing every channel. The
                // Swift original is immune only because its Int is 64-bit.
                if (l <= 0 || l > out.size - i - 4) break
                out[i] = 0
                out[i + 1] = 0
                out[i + 2] = 0
                out[i + 3] = 1
                i += 4 + l
            }
            return out
        }
        val out = java.io.ByteArrayOutputStream(au.size + 64)
        var i = 0
        while (i + nls <= au.size) {
            var l = 0
            for (k in 0 until nls) l = (l shl 8) or (au[i + k].toInt() and 0xFF)
            i += nls
            if (l <= 0 || l > au.size - i) break // remaining-space form; see the fast path
            out.write(byteArrayOf(0, 0, 0, 1))
            out.write(au, i, l)
            i += l
        }
        return out.toByteArray()
    }
}
