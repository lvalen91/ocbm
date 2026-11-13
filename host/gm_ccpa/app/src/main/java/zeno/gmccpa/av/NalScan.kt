package zeno.gmccpa.av

/**
 * The Annex-B NAL walk `HevcRenderer.handleMessage` classifies a seam message with, as a pure
 * function over `(buf, len)` (F-C2, 2026-10-08). The renderer now reads every message into a
 * grow-only scratch buffer instead of allocating one `ByteArray` per access unit (30-60 per second,
 * ~26 KB each — the GC churn behind the HeapTaskDaemon load), so the scan MUST stop at [len]: bytes
 * past it are the previous message's tail, and a stale `00 00 01` there would read as a phantom
 * VPS/SPS/PPS and release the codec mid-stream ("parameter sets changed"). `NalScanTest` holds that.
 *
 * Semantics are the renderer's own, unchanged: 3- and 4-byte start codes, HEVC NAL type from the
 * byte after the start code, parameter sets copied out (`copyOfRange`, so the caller may reuse the
 * buffer), VCL types 0..31 with 16..21 (IRAP) as keyframes.
 */
internal object NalScan {
    class Result {
        var vps: ByteArray? = null
        var sps: ByteArray? = null
        var pps: ByteArray? = null
        var sawParamSet = false
        var sawVcl = false
        var keyframe = false
    }

    fun scan(buf: ByteArray, len: Int): Result {
        val r = Result()
        var i = 0
        while (i < len - 4) {
            val long4 = buf[i].toInt() == 0 && buf[i + 1].toInt() == 0 &&
                        buf[i + 2].toInt() == 0 && buf[i + 3].toInt() == 1
            val short3 = !long4 && buf[i].toInt() == 0 && buf[i + 1].toInt() == 0 && buf[i + 2].toInt() == 1
            if (!long4 && !short3) { i++; continue }
            val off = i + (if (long4) 4 else 3)
            if (off >= len) break
            val type = (buf[off].toInt() shr 1) and 0x3F
            val to = nextStart(buf, len, off)
            when (type) {
                32 -> { r.vps = buf.copyOfRange(i, to); r.sawParamSet = true }
                33 -> { r.sps = buf.copyOfRange(i, to); r.sawParamSet = true }
                34 -> { r.pps = buf.copyOfRange(i, to); r.sawParamSet = true }
                in 0..31 -> { r.sawVcl = true; if (type in 16..21) r.keyframe = true }   // 16..21 = IRAP
            }
            i = to   // nextStart already walked off..to; resuming at `off` rescanned every NAL body
        }
        return r
    }

    /** Index of the next start code at or after [from] within [len], else [len]. */
    fun nextStart(buf: ByteArray, len: Int, from: Int): Int {
        var i = from
        while (i < len - 4) {
            if (buf[i].toInt() == 0 && buf[i + 1].toInt() == 0) {
                if (buf[i + 2].toInt() == 1) return i
                if (buf[i + 2].toInt() == 0 && buf[i + 3].toInt() == 1) return i
            }
            i++
        }
        return len
    }
}
