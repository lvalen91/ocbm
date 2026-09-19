package com.carlink.telephony

/**
 * The HFP/eSCO transport around [MsbcDecoder]/[MsbcEncoder]: H2 headers, resync, packet-loss
 * concealment, and the two adapters the app actually calls. Port of
 * `host/MacHost/carlink_macOS/Audio/MSBCFramer.swift`.
 *
 * THE AIR FORMAT (HFP 1.6 §5.7.4, Erratum 2409). A wideband SCO link is opened in TRANSPARENT mode:
 * the controller does no decoding, so every read off the socket is raw air data. One packet is
 * ```
 *     [0x01][sn][ 57-byte mSBC frame ][0x00]      = 60 bytes = 7.5 ms
 *      \______/
 *       H2 header: 0x01, then a sequence byte cycling 0x08 -> 0x38 -> 0xC8 -> 0xF8
 * ```
 * The sequence nibble is duplicated inside the byte, which is why the four values are so far apart
 * in Hamming distance — it survives a bit error. The trailing byte is padding to reach the 60-byte
 * eSCO payload; it is not part of SBC.
 *
 * WHY A RESYNCHRONISING FRAMER AND NOT A LENGTH SPLIT. The box hands us each SCO read verbatim
 * (`SEAM_PKT_PLAIN` under `SEAM_CODEC_MSBC`), and a read is not promised to be one packet: a short
 * read, a coalesced pair, or a lost packet all reach us as "some bytes". Splitting the byte stream by
 * a fixed 60 would silently decode garbage forever after the first odd-sized read. So: scan for the
 * H2 header (confirmed by the mSBC syncword 0xAD immediately behind it), carry a partial across chunk
 * boundaries, and use the sequence number — the only loss signal the air format has — to count what
 * never arrived. This is the proto's load-bearing rule: resynchronise on the H2 header, never on the
 * message length.
 */
object MsbcH2 {
    const val SYNC_BYTE: Int = 0x01

    /** Sequence bytes in transmission order. */
    val SEQUENCE_BYTES: IntArray = intArrayOf(0x08, 0x38, 0xC8, 0xF8)

    /** Pad byte that completes the 60-byte eSCO payload. */
    const val PAD_BYTE: Int = 0x00

    fun sequenceIndex(b: Int): Int =
        when (b and 0xFF) {
            0x08 -> 0
            0x38 -> 1
            0xC8 -> 2
            0xF8 -> 3
            else -> -1
        }
}

/**
 * Turns an arbitrarily chunked transparent-eSCO byte stream into whole mSBC frames plus explicit
 * loss events. Single-owner: the caller serialises access.
 */
class MsbcFramer {
    sealed class Event {
        /** One complete 57-byte mSBC frame, H2 header stripped. */
        class Frame(
            val bytes: ByteArray,
        ) : Event()

        /** `n` packets the sequence number says never arrived (1..3; a longer dropout aliases). */
        class Lost(
            val n: Int,
        ) : Event()
    }

    companion object {
        /**
         * Ceiling on the reassembly buffer. 4 KiB is ~68 packets — far past any plausible coalesced
         * read — so hitting it means the lane is carrying something that is not mSBC and the oldest
         * bytes are worthless.
         */
        const val MAX_BUFFERED: Int = 4096
    }

    private var buf = ByteArray(MAX_BUFFERED * 2)
    private var end = 0
    private var lastSeq = -1

    /** Frames handed out. */
    var framesOut = 0
        private set

    /** Packets the sequence numbers say were dropped in flight. */
    var lostPackets = 0
        private set

    /** Times more than a pad byte of junk was skipped to relock on an H2 header. */
    var resyncs = 0
        private set

    fun reset() {
        end = 0
        lastSeq = -1
    }

    fun push(bytes: ByteArray): List<Event> = push(bytes, 0, bytes.size)

    fun push(
        bytes: ByteArray,
        off: Int,
        len: Int,
    ): List<Event> {
        if (end + len > buf.size) buf = buf.copyOf(maxOf(buf.size * 2, end + len))
        System.arraycopy(bytes, off, buf, end, len)
        end += len
        if (end > MAX_BUFFERED) {
            val drop = end - MAX_BUFFERED
            System.arraycopy(buf, drop, buf, 0, MAX_BUFFERED)
            end = MAX_BUFFERED
            resyncs += 1
            lastSeq = -1
        }

        val events = ArrayList<Event>(4)
        var idx = 0
        while (true) {
            val h = headerIndex(idx)
            if (h < 0 || end - h < 2 + Msbc.FRAME_BYTES) {
                // Either nothing lockable — then only the last two bytes can still be the head of
                // a header (0x01, or 0x01 + a sequence byte) and everything before them is junk —
                // or a header whose frame is still arriving: keep it and wait.
                val keep = if (h < 0) maxOf(idx, end - 2) else h
                noteSkip(keep - idx)
                idx = keep
                break
            }
            noteSkip(h - idx)
            val sn = MsbcH2.sequenceIndex(buf[h + 1].toInt()) // headerIndex only matches valid ones
            if (lastSeq >= 0) {
                val gap = (sn - lastSeq - 1 + 4) % 4
                if (gap > 0) {
                    lostPackets += gap
                    events.add(Event.Lost(gap))
                }
            }
            lastSeq = sn
            events.add(Event.Frame(buf.copyOfRange(h + 2, h + 2 + Msbc.FRAME_BYTES)))
            framesOut += 1
            idx = h + 2 + Msbc.FRAME_BYTES
        }
        if (idx > 0) {
            System.arraycopy(buf, idx, buf, 0, end - idx)
            end -= idx
        }
        return events
    }

    /**
     * A header is `0x01`, a valid sequence byte, and the mSBC syncword behind it. Requiring the
     * syncword is what makes a relock trustworthy: 0x01 followed by 0x08 occurs inside compressed
     * audio often enough to matter, 0x01 0x08 0xAD does not.
     */
    private fun headerIndex(start: Int): Int {
        var i = start
        while (i + 2 < end) {
            if ((buf[i].toInt() and 0xFF) == MsbcH2.SYNC_BYTE &&
                MsbcH2.sequenceIndex(buf[i + 1].toInt()) >= 0 &&
                (buf[i + 2].toInt() and 0xFF) == Msbc.SYNCWORD
            ) {
                return i
            }
            i += 1
        }
        return -1
    }

    /** One skipped byte is the packet's own pad byte and is expected; more than that is a relock. */
    private fun noteSkip(n: Int) {
        if (n > 1) resyncs += 1
    }
}

/**
 * Downlink adapter (box -> speaker): framer + decoder + PLC. Raw `SEAM_PKT_PLAIN` payloads in,
 * 16 kHz mono S16LE out.
 */
class MsbcTelephonyDecoder {
    private val framer = MsbcFramer()
    private val decoder = MsbcDecoder()
    private var lastPcm: ShortArray? = null
    private var consecutiveConcealed = 0

    var framesDecoded = 0
        private set

    /** Frames synthesised by concealment (sequence gaps + decode failures). */
    var plcFrames = 0
        private set

    var decodeFailures = 0
        private set

    val resyncs: Int get() = framer.resyncs
    val lostPackets: Int get() = framer.lostPackets

    /** The last decode failure reason, for a diagnostic line. */
    val lastFailure: MsbcDecoder.Failure? get() = decoder.lastFailure

    fun reset() {
        framer.reset()
        decoder.reset()
        lastPcm = null
        consecutiveConcealed = 0
    }

    /**
     * Decode one seam payload. Returns S16LE PCM at 16 kHz mono — 240 bytes per recovered frame,
     * possibly zero bytes (a fragment that did not complete a frame) or several frames' worth.
     */
    fun decode(
        payload: ByteArray,
        off: Int = 0,
        len: Int = payload.size - off,
    ): ByteArray {
        val events = framer.push(payload, off, len)
        if (events.isEmpty()) return EMPTY
        var frames = 0
        for (e in events) frames += if (e is MsbcFramer.Event.Lost) e.n else 1
        val out = ByteArray(frames * Msbc.PCM_BYTES_PER_FRAME)
        var pos = 0
        for (e in events) {
            when (e) {
                is MsbcFramer.Event.Lost -> repeat(e.n) { pos = append(conceal(), out, pos) }
                is MsbcFramer.Event.Frame -> {
                    val pcm = decoder.decode(e.bytes)
                    if (pcm != null) {
                        framesDecoded += 1
                        consecutiveConcealed = 0
                        lastPcm = pcm
                        pos = append(pcm, out, pos)
                    } else {
                        decodeFailures += 1
                        pos = append(conceal(), out, pos)
                    }
                }
            }
        }
        return out
    }

    /**
     * Simple PLC: repeat the last good frame faded linearly to zero, then silence. Repeating it at
     * full level would buzz on a long dropout; substituting silence immediately clicks.
     */
    private fun conceal(): ShortArray {
        plcFrames += 1
        val last = lastPcm
        val first = consecutiveConcealed == 0
        consecutiveConcealed += 1
        if (last == null || !first) return ShortArray(Msbc.SAMPLES_PER_FRAME)
        val n = Msbc.SAMPLES_PER_FRAME
        return ShortArray(n) { i ->
            val gain = 1.0 - i.toDouble() / (n - 1).toDouble()
            MsbcTables.clampS16(MsbcTables.roundHalfAway(last[i].toDouble() * gain))
        }
    }

    private fun append(
        pcm: ShortArray,
        out: ByteArray,
        pos: Int,
    ): Int {
        var p = pos
        for (s in pcm) {
            val u = s.toInt()
            out[p] = (u and 0xFF).toByte()
            out[p + 1] = ((u shr 8) and 0xFF).toByte()
            p += 2
        }
        return p
    }

    private companion object {
        val EMPTY = ByteArray(0)
    }
}

/**
 * Uplink adapter (mic -> box): encoder + H2 packetiser. 7.5 ms of 16 kHz mono S16LE in, one
 * 60-byte eSCO packet out. The box writes what this returns to the SCO socket verbatim, so the pad
 * byte and the cycling sequence number are this side's responsibility — and each packet must go up
 * as its own `CH_MIC` message, never concatenated.
 */
class MsbcUplinkEncoder {
    private val encoder = MsbcEncoder()
    private val samples = ShortArray(Msbc.SAMPLES_PER_FRAME)
    private var seq = 0

    var packetsOut = 0
        private set

    fun reset() {
        encoder.reset()
        seq = 0
    }

    /** Encode [Msbc.PCM_BYTES_PER_FRAME] bytes of S16LE at `pcm[off]`. Null only for a wrong-sized input. */
    fun packet(
        pcm: ByteArray,
        off: Int = 0,
        len: Int = pcm.size - off,
    ): ByteArray? {
        if (len != Msbc.PCM_BYTES_PER_FRAME) return null
        for (i in 0 until Msbc.SAMPLES_PER_FRAME) {
            samples[i] = ((pcm[off + 2 * i].toInt() and 0xFF) or (pcm[off + 2 * i + 1].toInt() shl 8)).toShort()
        }
        val frame = encoder.encode(samples) ?: return null
        val out = ByteArray(Msbc.PACKET_BYTES)
        out[0] = MsbcH2.SYNC_BYTE.toByte()
        out[1] = MsbcH2.SEQUENCE_BYTES[seq].toByte()
        System.arraycopy(frame, 0, out, 2, Msbc.FRAME_BYTES)
        out[Msbc.PACKET_BYTES - 1] = MsbcH2.PAD_BYTE.toByte()
        seq = (seq + 1) % MsbcH2.SEQUENCE_BYTES.size
        packetsOut += 1
        return out
    }
}
