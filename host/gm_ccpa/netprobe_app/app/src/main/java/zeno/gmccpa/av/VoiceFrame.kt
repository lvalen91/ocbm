package zeno.gmccpa.av

/**
 * The `:9003` voice seam's wire, v1 and v2 (plan v5.8 V1a), as pure functions: no Android, no I/O.
 *
 * Every frame starts with the 11-byte v1 head `[rate u32 BE][ch u16 BE][atype u8][len u32 BE]`, and
 * `len` bytes follow. `atype` alone says what the frame is, and that is decided BEFORE any rate
 * check ([classify]):
 *  - `0xFF` — a v2 CONTROL frame (BEGIN / END), payload `[kind u8][write_us u64]…`;
 *  - `atype & 0x80` — a v2 AU: `[scid u64][rx_us u64][seq u16][rtp_ts u32][stream_type u8][AU]`
 *    (23 bytes, then the AU), purpose = `atype & 0x7F`;
 *  - otherwise a v1 AU: the body is the AU.
 * All multi-byte fields are big-endian. The receiver side is `crates/vendor/receiver/src/forward.rs`
 * (`tag_voice_v2`, `voice_begin_v2`, `voice_end_v2`); `VoiceFrameTest` carries the same golden
 * vectors as its `voice_v2_golden_vectors` test.
 */
object VoiceFrame {
    const val HEAD_LEN = 11
    const val V2_EXT_LEN = 23
    const val AU_FLAG = 0x80
    const val CONTROL = 0xFF
    const val KIND_BEGIN = 1
    const val KIND_END = 2
    const val BEGIN_LEN = 35
    const val END_LEN = 34
    /** The largest frame body a reader accepts (the v1 consumer's cap). */
    const val MAX_LEN = 1 shl 20

    /** END reasons (forward.rs `END_*`). */
    const val END_UNKNOWN = 0
    const val END_TEARDOWN = 1
    const val END_SESSION_RESET = 2
    const val END_SUPERSEDED = 3
    const val END_SOCKET_ERROR = 4

    /** BEGIN `codec` (the SEAM_FORMAT codes). */
    const val CODEC_PCM = 0
    const val CODEC_AAC_LC = 1
    const val CODEC_AAC_ELD = 2
    const val CODEC_OPUS = 3

    fun reasonName(r: Int): String = when (r) {
        END_TEARDOWN -> "teardown"
        END_SESSION_RESET -> "session reset"
        END_SUPERSEDED -> "superseded"
        END_SOCKET_ERROR -> "socket error"
        else -> "unknown($r)"
    }

    fun codecName(c: Int): String = when (c) {
        CODEC_PCM -> "PCM"; CODEC_AAC_LC -> "AAC-LC"; CODEC_AAC_ELD -> "AAC-ELD"; CODEC_OPUS -> "Opus"
        else -> "codec$c"
    }

    data class Head(val rate: Int, val ch: Int, val atype: Int, val len: Int)

    fun parseHead(b: ByteArray): Head =
        Head(be32(b, 0), be16(b, 4), b[6].toInt() and 0xFF, be32(b, 7))

    enum class Kind { CONTROL, AU_V2, AU_V1, DESYNC }

    /**
     * What a head announces. Control frames are dispatched BEFORE the rate check (VR:514-518 used
     * to reject any rate outside 8-48 kHz first); the rate check applies to AUs only.
     */
    fun classify(h: Head): Kind = when {
        h.len < 0 || h.len > MAX_LEN -> Kind.DESYNC
        h.atype == CONTROL -> Kind.CONTROL
        h.rate !in 8000..48000 -> Kind.DESYNC
        h.atype and AU_FLAG != 0 -> if (h.len >= V2_EXT_LEN) Kind.AU_V2 else Kind.DESYNC
        else -> Kind.AU_V1
    }

    data class Begin(
        val writeUs: Long, val scid: Long, val streamType: Int, val audioType: Int, val codec: Int,
        val rate: Int, val ch: Int, val audioLatencyMs: Int, val input: Boolean, val beginSeq: Long,
    )

    data class End(
        val writeUs: Long, val scid: Long, val reason: Int, val frames: Long, val drops: Long,
        val seqGaps: Long, val maxInterArrivalMs: Long,
    )

    /** A v2 AU's extension: who it belongs to and when rust received it. */
    data class AuExt(val scid: Long, val rxUs: Long, val seq: Int, val rtpTs: Long, val streamType: Int)

    /** A control payload as BEGIN or END; null for an unknown kind or a short payload. */
    fun parseControl(p: ByteArray): Any? {
        if (p.isEmpty()) return null
        return when (p[0].toInt() and 0xFF) {
            KIND_BEGIN -> if (p.size < BEGIN_LEN) null else Begin(
                writeUs = be64(p, 1), scid = be64(p, 9), streamType = p[17].toInt() and 0xFF,
                audioType = p[18].toInt() and 0xFF, codec = p[19].toInt() and 0xFF,
                rate = be32(p, 20), ch = be16(p, 24), audioLatencyMs = be32(p, 26),
                input = p[30].toInt() != 0, beginSeq = be32(p, 31).toLong() and 0xFFFFFFFFL,
            )
            KIND_END -> if (p.size < END_LEN) null else End(
                writeUs = be64(p, 1), scid = be64(p, 9), reason = p[17].toInt() and 0xFF,
                frames = be32(p, 18).toLong() and 0xFFFFFFFFL, drops = be32(p, 22).toLong() and 0xFFFFFFFFL,
                seqGaps = be32(p, 26).toLong() and 0xFFFFFFFFL,
                maxInterArrivalMs = be32(p, 30).toLong() and 0xFFFFFFFFL,
            )
            else -> null
        }
    }

    /** The 23-byte extension at the start of a v2 AU body. */
    fun parseAuExt(b: ByteArray): AuExt = AuExt(
        scid = be64(b, 0), rxUs = be64(b, 8), seq = be16(b, 16),
        rtpTs = be32(b, 18).toLong() and 0xFFFFFFFFL, streamType = b[22].toInt() and 0xFF,
    )

    fun be16(b: ByteArray, o: Int): Int = ((b[o].toInt() and 0xFF) shl 8) or (b[o + 1].toInt() and 0xFF)
    fun be32(b: ByteArray, o: Int): Int =
        ((b[o].toInt() and 0xFF) shl 24) or ((b[o + 1].toInt() and 0xFF) shl 16) or
        ((b[o + 2].toInt() and 0xFF) shl 8) or (b[o + 3].toInt() and 0xFF)
    fun be64(b: ByteArray, o: Int): Long {
        var v = 0L
        for (i in 0 until 8) v = (v shl 8) or (b[o + i].toLong() and 0xFF)
        return v
    }
}

/**
 * A latency histogram: 1 ms buckets to 2 s, then 100 ms buckets to 60 s, plus the exact max.
 * Percentiles resolve to a bucket's LOWER edge (floor ms, like the max). Pure; the caller synchronizes.
 */
class LatencyHist {
    private val fine = IntArray(FINE_MS)
    private val coarse = IntArray(COARSE_N + 1)
    var count = 0L
        private set
    var maxUs = Long.MIN_VALUE
        private set

    fun add(us: Long) {
        count++
        if (us > maxUs) maxUs = us
        val ms = (us / 1000).coerceAtLeast(0)
        if (ms < FINE_MS) fine[ms.toInt()]++
        else coarse[((ms - FINE_MS) / COARSE_STEP_MS).coerceAtMost(COARSE_N.toLong()).toInt()]++
    }

    /** The [p]th percentile (0..100) in ms, or -1 with no samples. */
    fun percentileMs(p: Double): Long {
        if (count == 0L) return -1
        val rank = Math.ceil(count * p / 100.0).toLong().coerceIn(1, count)
        var seen = 0L
        for (i in fine.indices) { seen += fine[i]; if (seen >= rank) return i.toLong() }
        for (i in coarse.indices) {
            seen += coarse[i]; if (seen >= rank) return FINE_MS + i * COARSE_STEP_MS
        }
        return maxUs / 1000
    }

    fun clear() { fine.fill(0); coarse.fill(0); count = 0; maxUs = Long.MIN_VALUE }

    /** `p50/p95/p99/max ms`, or `-` with no samples. */
    fun summary(): String =
        if (count == 0L) "-" else
            "${percentileMs(50.0)}/${percentileMs(95.0)}/${percentileMs(99.0)}/${maxUs / 1000}ms"

    private companion object {
        const val FINE_MS = 2000
        const val COARSE_STEP_MS = 100L
        const val COARSE_N = 580
    }
}

/**
 * Per-stream (scid) AU statistics for the V1a measurement build and the V1b END line: ages on the
 * receiver's voice clock (rx→read and rx→write), app-side RTP seq gaps, frame counts. A window
 * (reset every report) and a stream total. Pure; all methods synchronized.
 */
class VoiceStreamStats(val scid: Long, val label: String) {
    private val readWin = LatencyHist()
    private val writeWin = LatencyHist()
    private val readAll = LatencyHist()
    private val writeAll = LatencyHist()
    var aus = 0L
        private set
    var seqGaps = 0L
        private set
    private var lastSeq = -1
    private var winAus = 0L

    /** One AU read: its rx stamp, the read stamp (both voice-clock µs; -1 = unknown), its RTP seq. */
    @Synchronized fun onRead(rxUs: Long, readUs: Long, seq: Int) {
        aus++; winAus++
        if (rxUs >= 0 && readUs >= 0) { readWin.add(readUs - rxUs); readAll.add(readUs - rxUs) }
        if (lastSeq >= 0) {
            val d = (seq - lastSeq) and 0xFFFF
            if (d in 1 until 0x8000) { seqGaps += d - 1; lastSeq = seq }
        } else lastSeq = seq
    }

    /** One AU written to the track (or done with): rx stamp and write stamp, voice-clock µs. */
    @Synchronized fun onWrite(rxUs: Long, writeUs: Long) {
        if (rxUs >= 0 && writeUs >= 0) { writeWin.add(writeUs - rxUs); writeAll.add(writeUs - rxUs) }
    }

    /** The window line, then the window is reset. Null when the window saw no AU. */
    @Synchronized fun window(): String? {
        if (winAus == 0L) return null
        val s = "$label scid=${java.lang.Long.toHexString(scid)}: $winAus AUs, age rx→read p50/p95/p99/max " +
            "${readWin.summary()}, rx→write ${writeWin.summary()}, seq gaps $seqGaps"
        readWin.clear(); writeWin.clear(); winAus = 0
        return s
    }

    @Synchronized fun total(): String =
        "$aus AUs read, age rx→read ${readAll.summary()}, rx→write ${writeAll.summary()}, app seq gaps $seqGaps"
}
