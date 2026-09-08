package zeno.gmccpa

import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * The consumer side of the receiver's localhost A/V seam.
 *
 * `forward.rs` CONNECTS out to `127.0.0.1:9001` (Annex-B video), `:9002` (ADTS media audio) and
 * `:9003` (tagged voice AUs) — so this side must be LISTENING before the streams are set up, or the
 * forwarder logs `connect carlink :900x failed: Connection refused` once per access unit (which is
 * precisely what we saw before this existed).
 *
 * This is deliberately NOT a decoder. It is the smallest thing that proves real A/V is arriving:
 * it parses the framing the way MediaCodec would have to (NAL start codes and their types for video,
 * ADTS sync words and their headers for audio), reports the first frames, and keeps a running count.
 * Feeding MediaCodec is the next step and can reuse these sockets unchanged.
 *
 * Everything is reported through [ProbeLog] because `run-as` is blocked on this head unit — logcat is
 * the only channel out. A capped prefix of each stream is also written to the app's own files dir for
 * offline inspection if we ever get a way to pull it.
 */
class AvSink(private val filesDir: File) {

    private val log = ProbeLog.sub("avsink")
    private val running = AtomicBoolean(false)
    // CopyOnWriteArrayList: mutated by three listener threads and iterated by stop() with no external
    // lock; a plain ArrayList corrupts and can drop an entry, leaking a bound port forever.
    private val servers = java.util.concurrent.CopyOnWriteArrayList<ServerSocket>()
    /** Accepted sockets, same reasoning. Closing the listener never unblocks a pump parked in `read()`
     *  on one of these, so [stop] has to close them itself before it claims to have stopped. */
    private val conns = java.util.concurrent.CopyOnWriteArrayList<Socket>()

    /** How many bytes of each stream to keep on disk. Enough for several frames, small enough to be safe. */
    private val CAPTURE_CAP = 512 * 1024

    val videoBytes = AtomicLong(0)
    val audioBytes = AtomicLong(0)
    val voiceBytes = AtomicLong(0)

    fun start() {
        if (!running.compareAndSet(false, true)) { log.i("already running"); return }
        listen(9001, "video") { s -> pumpVideo(s) }
        listen(9002, "audio") { s -> pumpAudio(s, "media", audioBytes, "audio.adts") }
        listen(9003, "voice") { s -> pumpAudio(s, "voice", voiceBytes, "voice.bin") }
        log.i("A/V sink listening on 127.0.0.1:9001 (video) :9002 (media audio) :9003 (voice)")
    }

    fun stop() {
        running.set(false)
        val nl = servers.size
        val nc = conns.size
        servers.forEach { runCatching { it.close() } }   // COW: safe to iterate under concurrent removes
        conns.forEach { runCatching { it.close() } }
        log.i("A/V sink stopped — listeners=$nl conns=$nc closed, " +
              "video=${videoBytes.get()} audio=${audioBytes.get()} voice=${voiceBytes.get()} bytes")
    }

    fun stats(): String =
        "video=${videoBytes.get()}B audio=${audioBytes.get()}B voice=${voiceBytes.get()}B"

    private fun listen(port: Int, label: String, handler: (Socket) -> Unit) {
        val t = Thread({
            var srv: ServerSocket? = null
            try {
                // The forwarder dials 127.0.0.1 explicitly, so bind loopback only — no exposure on br0.
                srv = ServerSocket(port, 4, InetAddress.getByName("127.0.0.1"))
                servers.add(srv)
                // If stop() raced the bind, `running` is already false and the loop exits straight to
                // the finally, which closes the socket — so the port is never leaked.
                while (running.get()) {
                    val s = try { srv.accept() } catch (e: Exception) { if (running.get()) log.e("$label accept: ${e.message}"); break }
                    log.i("$label seam connected from ${s.remoteSocketAddress}")
                    conns.add(s)
                    try { handler(s) } catch (e: Exception) { log.e("$label pump: ${e.message}") }
                    finally { conns.remove(s); runCatching { s.close() } }
                }
            } catch (e: Exception) {
                log.e("$label listener on :$port failed: ${e.message}")
            } finally {
                runCatching { srv?.close() }
                srv?.let { servers.remove(it) }
            }
        }, "avsink-$label")
        t.isDaemon = true
        t.start()
    }

    /**
     * Video: Annex-B. Classify each NAL under BOTH interpretations and let the values decide the codec
     * — HEVC takes the type from bits 6..1 of the first header byte, H.264 from bits 4..0, and the
     * parameter-set types (HEVC VPS/SPS/PPS = 32/33/34, H.264 SPS/PPS = 7/8) disambiguate immediately.
     * docs/13 §5: the codec is never declared in the SETUP dict, it rides in-band, so this is the only
     * way to know what iOS actually chose.
     */
    private fun pumpVideo(s: Socket) {
        val out = File(filesDir, "video.h26x")
        out.delete()
        val ins = s.getInputStream()
        val buf = ByteArray(64 * 1024)
        var reported = 0
        var captured = 0
        var nalCount = 0
        var codecCalled = false
        val fos = out.outputStream()
        try {
            while (running.get()) {
                val n = ins.read(buf)
                if (n <= 0) break
                videoBytes.addAndGet(n.toLong())
                if (captured < CAPTURE_CAP) {
                    val take = minOf(n, CAPTURE_CAP - captured)
                    fos.write(buf, 0, take); captured += take
                }
                // Scan for start codes across the whole buffer (ignoring the rare split across reads —
                // we only need the first frames, and there are thousands).
                var i = 0
                while (i < n - 4) {
                    val isLong = buf[i].toInt() == 0 && buf[i + 1].toInt() == 0 &&
                                 buf[i + 2].toInt() == 0 && buf[i + 3].toInt() == 1
                    val isShort = !isLong && buf[i].toInt() == 0 && buf[i + 1].toInt() == 0 && buf[i + 2].toInt() == 1
                    if (isLong || isShort) {
                        val off = i + (if (isLong) 4 else 3)
                        if (off < n) {
                            val b0 = buf[off].toInt() and 0xFF
                            val hevcType = (b0 shr 1) and 0x3F
                            val h264Type = b0 and 0x1F
                            nalCount++
                            if (!codecCalled && (hevcType == 32 || hevcType == 33 || hevcType == 34)) {
                                log.i("CODEC = HEVC (hvc1) — saw VPS/SPS/PPS (hevc nal type $hevcType)")
                                codecCalled = true
                            } else if (!codecCalled && (h264Type == 7 || h264Type == 8)) {
                                log.i("CODEC = H.264 (avc1) — saw SPS/PPS (h264 nal type $h264Type)")
                                codecCalled = true
                            }
                            if (reported < 16) {
                                log.i("  NAL #$nalCount hdr=0x%02x  hevc_type=%2d (%s)  h264_type=%2d (%s)"
                                    .format(b0, hevcType, hevcName(hevcType), h264Type, h264Name(h264Type)))
                                reported++
                            }
                        }
                        i = off
                    } else i++
                }
                if (nalCount > 0 && videoBytes.get() > 0 && reported == 16) {
                    log.i("VIDEO CONFIRMED: ${videoBytes.get()} bytes, $nalCount NALs so far")
                    reported++
                }
            }
        } finally {
            runCatching { fos.close() }
            log.i("video seam closed — ${videoBytes.get()} bytes, $nalCount NALs, ${captured}B captured to ${out.name}")
        }
    }

    /**
     * Audio: the media seam is ADTS (`forward.rs` wraps the AAC-LC AUs), so every frame starts with a
     * 12-bit sync word 0xFFF. Parsing the header back out confirms the format actually matches what the
     * SETUP dict negotiated (48 kHz / 2 ch / AAC-LC) rather than trusting the negotiation.
     */
    private fun pumpAudio(s: Socket, label: String, counter: AtomicLong, fileName: String) {
        val out = File(filesDir, fileName)
        out.delete()
        val ins = s.getInputStream()
        val buf = ByteArray(32 * 1024)
        var reported = 0
        var captured = 0
        var frames = 0
        val fos = out.outputStream()
        val rates = intArrayOf(96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350)
        try {
            while (running.get()) {
                val n = ins.read(buf)
                if (n <= 0) break
                counter.addAndGet(n.toLong())
                if (captured < CAPTURE_CAP) {
                    val take = minOf(n, CAPTURE_CAP - captured)
                    fos.write(buf, 0, take); captured += take
                }
                var i = 0
                while (i < n - 7) {
                    if ((buf[i].toInt() and 0xFF) == 0xFF && (buf[i + 1].toInt() and 0xF0) == 0xF0) {
                        frames++
                        if (reported < 6) {
                            val b2 = buf[i + 2].toInt() and 0xFF
                            val prof = (b2 shr 6) and 0x03
                            val rateIdx = (b2 shr 2) and 0x0F
                            val ch = ((b2 and 0x01) shl 2) or ((buf[i + 3].toInt() and 0xC0) ushr 6)
                            val len = ((buf[i + 3].toInt() and 0x03) shl 11) or
                                      ((buf[i + 4].toInt() and 0xFF) shl 3) or
                                      ((buf[i + 5].toInt() and 0xE0) ushr 5)
                            val rate = if (rateIdx < rates.size) rates[rateIdx] else -1
                            log.i("  $label ADTS #$frames: profile=${profName(prof)} ${rate}Hz ${ch}ch frameLen=$len")
                            reported++
                        }
                        i += 7
                    } else i++
                }
                if (frames > 0 && reported == 6) {
                    log.i("${label.uppercase()} AUDIO CONFIRMED: ${counter.get()} bytes, $frames ADTS frames so far")
                    reported++
                }
            }
        } finally {
            runCatching { fos.close() }
            log.i("$label seam closed — ${counter.get()} bytes, $frames frames, ${captured}B captured to ${out.name}")
        }
    }

    private fun profName(p: Int) = when (p) { 0 -> "AAC-Main"; 1 -> "AAC-LC"; 2 -> "AAC-SSR"; 3 -> "AAC-LTP"; else -> "?" }

    private fun hevcName(t: Int) = when (t) {
        0 -> "TRAIL_N"; 1 -> "TRAIL_R"; 19 -> "IDR_W_RADL"; 20 -> "IDR_N_LP"; 21 -> "CRA"
        32 -> "VPS"; 33 -> "SPS"; 34 -> "PPS"; 35 -> "AUD"; 39 -> "SEI_PREFIX"; 40 -> "SEI_SUFFIX"
        else -> "-"
    }

    private fun h264Name(t: Int) = when (t) {
        1 -> "non-IDR"; 5 -> "IDR"; 6 -> "SEI"; 7 -> "SPS"; 8 -> "PPS"; 9 -> "AUD"
        else -> "-"
    }
}
