package zeno.gmccpa.av

/**
 * RIFF/WAVE validation for the bench-only `mic_inject` verb (plan v5.9 MIC). Pure and Android-free.
 *
 * Accepts exactly the uplink's Siri format: PCM (audioFormat 1), 1 channel, 16000 Hz, 16 bits,
 * blockAlign 2, with 0.2 s ≤ duration ≤ 30 s. Anything else is rejected with ONE reason line.
 *
 * The chunk walk skips every chunk it does not need. macOS `say -o x.wav --file-format=WAVE
 * --data-format=LEI16@16000 --channels=1` writes `fmt ` (16 bytes), then an `FLLR` padding chunk of
 * 4044 bytes (all zero), then `data` — measured 2026-10-07 and again 2026-10-08 (macOS 27.2). RIFF
 * word alignment: an odd-size chunk is followed by one pad byte, so the walk advances by
 * `size + (size and 1)` (rev b, R30).
 */
object WavPcm {

    const val RATE = 16000
    const val CHANNELS = 1
    const val BITS = 16
    /** 0.2 s and 30 s of 16000 Hz mono S16LE. */
    const val MIN_BYTES = 6_400
    const val MAX_BYTES = 960_000
    const val FORMAT_PCM = 1
    const val FORMAT_EXTENSIBLE = 0xFFFE

    sealed class Result {
        /** [pcm] is S16LE mono 16000 Hz, even length, MIN_BYTES..MAX_BYTES. */
        class Ok(val pcm: ByteArray, val formatTag: Int) : Result() {
            val durationMs: Int get() = pcm.size / (RATE / 1000 * 2)
        }
        class Bad(val reason: String) : Result()
    }

    private fun u16(b: ByteArray, o: Int): Int = (b[o].toInt() and 0xFF) or ((b[o + 1].toInt() and 0xFF) shl 8)
    private fun u32(b: ByteArray, o: Int): Long =
        (u16(b, o).toLong()) or (u16(b, o + 2).toLong() shl 16)
    private fun id(b: ByteArray, o: Int): String = String(b, o, 4, Charsets.US_ASCII)

    fun parse(b: ByteArray): Result {
        if (b.size < 12 || id(b, 0) != "RIFF" || id(b, 8) != "WAVE") return Result.Bad("not a RIFF/WAVE file")
        var fmtSeen = false
        var off = 12L
        val end = b.size.toLong()
        while (off + 8 <= end) {
            val o = off.toInt()
            val cid = id(b, o)
            val size = u32(b, o + 4)
            val body = off + 8
            when (cid) {
                "fmt " -> {
                    if (size < 16 || body + 16 > end) return Result.Bad("fmt chunk truncated (size $size)")
                    val p = body.toInt()
                    val format = u16(b, p)
                    val ch = u16(b, p + 2)
                    val rate = u32(b, p + 4)
                    val blockAlign = u16(b, p + 12)
                    val bits = u16(b, p + 14)
                    if (format == FORMAT_EXTENSIBLE) return Result.Bad("WAVE_FORMAT_EXTENSIBLE (0xFFFE) is not accepted — need PCM (format 1)")
                    if (format != FORMAT_PCM) return Result.Bad("audioFormat $format is not PCM (1)")
                    if (ch != CHANNELS) return Result.Bad("$ch channels — need 1 (mono)")
                    if (rate != RATE.toLong()) return Result.Bad("$rate Hz — need 16000 Hz")
                    if (bits != BITS) return Result.Bad("$bits bits per sample — need 16")
                    if (blockAlign != 2) return Result.Bad("blockAlign $blockAlign — need 2")
                    fmtSeen = true
                }
                "data" -> {
                    if (!fmtSeen) return Result.Bad("data chunk before any fmt chunk")
                    if (body + size > end) return Result.Bad("data length $size exceeds the file (${end - body} bytes left)")
                    if (size and 1L != 0L) return Result.Bad("data length $size is odd — not whole 16-bit samples")
                    if (size < MIN_BYTES) return Result.Bad("clip is ${size / 32} ms — shorter than 200 ms")
                    if (size > MAX_BYTES) return Result.Bad("clip is ${size / 32} ms — longer than 30 s")
                    return Result.Ok(b.copyOfRange(body.toInt(), (body + size).toInt()), FORMAT_PCM)
                }
            }
            off = body + size + (size and 1L)
        }
        return Result.Bad(if (fmtSeen) "no data chunk" else "no fmt chunk")
    }
}
