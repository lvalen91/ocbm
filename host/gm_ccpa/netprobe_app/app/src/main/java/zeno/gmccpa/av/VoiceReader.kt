package zeno.gmccpa.av

import java.io.InputStream
import java.util.concurrent.atomic.AtomicLong

/**
 * The `:9003` reader (plan v5.8 V1b, rev e R1): it parses frames, stamps each read on the receiver's
 * voice clock, routes by scid, and only ENQUEUES — [VoiceStreams.onBegin] registers a BEGIN's stream
 * synchronously, [VoiceStreams.onAu] puts an AU into that stream's jitter buffer, and the purpose
 * worker does everything else. It never blocks on a worker.
 *
 * Structural rule (it_v58_V1.sh): this file references no MediaCodec, AudioTrack or AudioManager,
 * and calls nothing in FocusSlot or the pool.
 */
class VoiceReader(
    private val core: VoiceStreams,
    private val clock: VoiceClock,
    private val log: VoiceLog,
    private val running: () -> Boolean,
    private val bytesIn: AtomicLong,
) {
    /** Read connection [gen] until it ends; returns why. Consume thread. */
    fun run(ins: InputStream, gen: Int): String {
        val hdr = ByteArray(VoiceFrame.HEAD_LEN)
        while (running()) {
            if (!readFully(ins, hdr, VoiceFrame.HEAD_LEN)) return "voice seam connection ended"
            val h = VoiceFrame.parseHead(hdr)
            when (VoiceFrame.classify(h)) {
                VoiceFrame.Kind.DESYNC -> {
                    val why = if (h.len < 0 || h.len > VoiceFrame.MAX_LEN) "voice desync: len=${h.len}"
                    else "voice desync: implausible rate ${h.rate}Hz (atype ${h.atype}, len ${h.len}) — the seam is " +
                         "probably speaking the forward-encrypted v2 framing (check OCBM_FWD_ENC on the box)"
                    log.e(why)
                    return why
                }
                VoiceFrame.Kind.CONTROL -> {
                    val p = ByteArray(h.len)
                    if (!readFully(ins, p, h.len)) return "voice seam connection ended mid-frame"
                    val readUs = clock.nowUs()
                    bytesIn.addAndGet((VoiceFrame.HEAD_LEN + h.len).toLong())
                    when (val c = VoiceFrame.parseControl(p)) {
                        is VoiceFrame.Begin -> core.onBegin(c, gen, readUs)
                        is VoiceFrame.End -> core.onEnd(c, gen, readUs)
                        else -> log.w("voice: control frame of ${p.size} B, kind ${if (p.isEmpty()) -1 else p[0].toInt() and 0xFF} — ignored")
                    }
                }
                VoiceFrame.Kind.AU_V2 -> {
                    val body = ByteArray(h.len)
                    if (!readFully(ins, body, h.len)) return "voice seam connection ended mid-frame"
                    val readUs = clock.nowUs()
                    bytesIn.addAndGet((VoiceFrame.HEAD_LEN + h.len).toLong())
                    val ext = VoiceFrame.parseAuExt(body)
                    val au = body.copyOfRange(VoiceFrame.V2_EXT_LEN, h.len)
                    core.onAu(ext, h.atype and 0x7F, h.rate, h.ch, au, gen, readUs)
                }
                VoiceFrame.Kind.AU_V1 -> {
                    if (h.len > 0) {
                        val body = ByteArray(h.len)
                        if (!readFully(ins, body, h.len)) return "voice seam connection ended mid-frame"
                        bytesIn.addAndGet((VoiceFrame.HEAD_LEN + h.len).toLong())
                    }
                    core.onV1Au(h.atype, h.rate, h.ch)
                }
            }
        }
        return "router stopped"
    }

    private fun readFully(ins: InputStream, buf: ByteArray, n: Int): Boolean {
        var off = 0
        while (off < n) {
            val r = try { ins.read(buf, off, n - off) } catch (e: Exception) {
                if (running()) log.e("voice read: ${e.message}")
                return false
            }
            if (r <= 0) return false
            off += r
        }
        return true
    }
}
