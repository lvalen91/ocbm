package zeno.gmccpa.bench

import android.content.Context
import android.content.Intent
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.av.MicInject
import zeno.gmccpa.av.WavPcm
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.SocketTimeoutException
import java.util.concurrent.TimeUnit

/**
 * BENCH-ONLY operator hooks (plan v5.9 MIC, owner-approved 2026-10-07). Compiled only when
 * `tools/build_apk.sh` runs with `GMCCPA_BENCH=1`, which also suffixes the versionName with
 * `-bench`; a release build compiles the src/nobench stub instead. This file is the ONLY caller of
 * [MicInject.arm] in the app sources (src/test aside).
 *
 * `mic_inject` arms ONE 16 kHz mono S16LE WAV that MicUplink's capture thread sends in place of the
 * AudioRecord samples at the next chunk of a live 16000/1 capture (lead silence + clip + tail
 * silence, paced by AudioRecord), then the real mic again.
 *
 * **Exposure, stated (rev b, R25).** MainActivity is `exported="true"` with no permission and passes
 * any intent carrying `run` to its verb dispatcher, so in a bench build ANY installed app could send
 * this verb and arm a clip that Siri acts on — up to placing a call. The cheap guard is a shell-only
 * enable step: `adb shell setprop debug.gmccpa.mic_inject 1` before a test and `… 0` after (props
 * reset on reboot). Apps cannot set a `debug.*` property, but they CAN send the verb while it is
 * enabled: the guard narrows the window, it does not authenticate the caller. Whether a user-10 app
 * can read `debug.*` on this build is UNKNOWN (U59-16); if it cannot, this hook fails closed (always
 * "disabled"). The loopback form (`--es file -`) additionally accepts one connection on
 * 127.0.0.1:[MIC_INJECT_PORT] for ≤ 30 s, from any local process.
 *
 * Arguments: `--es file <path>` | `--es file -` (loopback) | `--es file none` (clear — honoured even
 * while disabled, since it can only remove a clip);
 * `--ei lead_ms` (700, 0..3000), `--ei tail_ms` (1500, 0..5000), `--ei expire_s` (60, 5..300).
 * Runs on its own short-lived thread: never main (the dispatcher's thread) and never the command
 * executor (it may be parked behind a claim).
 */
object BenchHooks {

    const val PROP = "debug.gmccpa.mic_inject"
    const val MIC_INJECT_PORT = 9119
    private const val LOOPBACK_MS = 30_000L
    /** Largest file read: 30 s of PCM plus generous room for header chunks (say's FLLR is 4 KB). */
    private const val MAX_FILE = WavPcm.MAX_BYTES + 64 * 1024

    private val log = ProbeLog.sub("mic")

    fun micInject(@Suppress("UNUSED_PARAMETER") ctx: Context, i: Intent) {
        val file = i.getStringExtra("file")
        val lead = intArg(i, "lead_ms", 700, 0, 3000)
        val tail = intArg(i, "tail_ms", 1500, 0, 5000)
        val expire = intArg(i, "expire_s", 60, 5, 300)
        if (i.hasExtra("file2")) log.w("mic_inject: --es file2 is withdrawn (one clip at a time) — ignored")
        Thread({ armFrom(file, lead, tail, expire) }, "mic-inject-arm").apply { isDaemon = true; start() }
    }

    /**
     * `finish_launcher` (plan v5.9 D6 rev c C5, bench only): destroy the launcher so a headless
     * auto-claim can be tested (B-D6-5). BACK cannot: MainActivity is a singleTask MAIN/LAUNCHER root,
     * and a root back press only moves the task to the back on API 32. The dispatcher has already
     * applied any `--es ssid/pass` before this runs. Harmless if a stray app sends it: the session
     * core is process-scoped and runs on headless.
     */
    fun finishLauncher(act: android.app.Activity) {
        log.i("finish_launcher: finishing the launcher activity (bench)")
        act.finish()
    }

    /** null = the extra is malformed (reported by [run]). */
    private fun intArg(i: Intent, key: String, def: Int, lo: Int, hi: Int): Int? {
        @Suppress("DEPRECATION") val v = i.extras?.get(key) ?: return def
        val n = when (v) { is Int -> v; is Long -> v.toInt(); is String -> v.trim().toIntOrNull(); else -> null } ?: return null
        return n.coerceIn(lo, hi)
    }

    private fun armFrom(file: String?, lead: Int?, tail: Int?, expire: Int?) {
        try {
            // Clear BEFORE the enable check (rc2 review MINOR): clearing can only remove a clip, and a
            // `disable` then `clear` used to be refused here, leaving an unstarted clip armed for up to
            // expire_s (300 s max) to start on the next 16000/1 capture.
            if (file == "none") {
                log.i(if (MicInject.clear()) "mic_inject: cleared" else "mic_inject: cleared (nothing was armed)")
                return
            }
            val prop = readProp()
            if (prop != "1") {
                log.i("mic_inject: disabled ($PROP != 1${if (prop == null) "; getprop unreadable" else ""})")
                return
            }
            MicInject.log = { log.i(it) }
            if (file == null) { log.w("mic_inject: no --es file (<path> | - | none) — nothing armed"); return }
            if (lead == null || tail == null || expire == null) {
                reject("lead_ms/tail_ms/expire_s must be integers"); return
            }
            val bytes = (if (file == "-") readLoopback() else readFile(file)) ?: run { reject(null); return }
            when (val r = WavPcm.parse(bytes)) {
                is WavPcm.Result.Bad -> reject(r.reason)
                is WavPcm.Result.Ok -> {
                    val c = MicInject.arm(r.pcm, lead, tail, expire)
                    log.i("mic_inject: armed 1 clip(s) (${c.clipMs} ms + lead $lead ms + tail $tail ms, expires $expire s; " +
                        "WAV format ${r.formatTag}, ${WavPcm.RATE} Hz, ${WavPcm.CHANNELS} ch, ${WavPcm.BITS} bit, ${c.chunks} chunks)")
                }
            }
        } catch (t: Throwable) {
            reject("${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /** A rejected arm leaves NOTHING armed (any earlier clip is cleared too). [why] null = already logged. */
    private fun reject(why: String?) {
        val had = MicInject.clear()
        log.w("mic_inject: rejected${if (why != null) " — $why" else ""}; nothing armed${if (had) " (the earlier clip was cleared)" else ""}")
    }

    /** `getprop` via exec; null when it cannot be run or read (fails closed). */
    private fun readProp(): String? = try {
        val p = Runtime.getRuntime().exec(arrayOf("/system/bin/getprop", PROP))
        val v = p.inputStream.use { it.readBytes().toString(Charsets.US_ASCII).trim() }
        if (!p.waitFor(2, TimeUnit.SECONDS)) { p.destroy(); null } else v
    } catch (t: Throwable) {
        log.w("mic_inject: getprop $PROP failed: ${t.javaClass.simpleName}: ${t.message}"); null
    }

    private fun readFile(path: String): ByteArray? = try {
        val f = File(path)
        val len = f.length()
        if (len > MAX_FILE) { log.w("mic_inject: $path is $len bytes (> $MAX_FILE)"); null }
        else f.inputStream().use { readBounded(it, Long.MAX_VALUE) }
    } catch (t: Throwable) {
        // Android's message carries the errno, e.g. "open failed: EACCES (Permission denied)".
        log.w("mic_inject: read $path failed: ${t.javaClass.simpleName}: ${t.message} — try `--es file -` (loopback)"); null
    }

    /**
     * ONE connection on 127.0.0.1:[MIC_INJECT_PORT], ≤ 30 s from now in total. The read stops at the
     * RIFF length (so a sender that never half-closes still completes), at EOF, or at the deadline.
     */
    private fun readLoopback(): ByteArray? {
        val deadline = System.nanoTime() / 1_000_000L + LOOPBACK_MS
        return try {
            ServerSocket(MIC_INJECT_PORT, 1, InetAddress.getByName("127.0.0.1")).use { ss ->
                log.i("mic_inject: waiting ≤ ${LOOPBACK_MS / 1000} s for one connection on 127.0.0.1:$MIC_INJECT_PORT " +
                    "(adb shell \"toybox nc 127.0.0.1 $MIC_INJECT_PORT < /data/local/tmp/gmccpa_mic.wav\")")
                ss.soTimeout = LOOPBACK_MS.toInt()
                ss.accept().use { s ->
                    readBounded(s.getInputStream(), deadline) { left -> s.soTimeout = left.toInt().coerceAtLeast(1) }
                }
            }
        } catch (_: SocketTimeoutException) {
            log.w("mic_inject: loopback: no complete WAV within ${LOOPBACK_MS / 1000} s"); null
        } catch (t: Throwable) {
            log.w("mic_inject: loopback :$MIC_INJECT_PORT failed: ${t.javaClass.simpleName}: ${t.message}"); null
        }
    }

    /** Reads ≤ [MAX_FILE] bytes; stops early once the RIFF length is complete. Null when over the cap. */
    private fun readBounded(ins: InputStream, deadline: Long, setTimeout: (Long) -> Unit = {}): ByteArray? {
        val acc = ByteArrayOutputStream()
        val tmp = ByteArray(16 * 1024)
        var want = Long.MAX_VALUE
        while (true) {
            if (deadline != Long.MAX_VALUE) {
                val left = deadline - System.nanoTime() / 1_000_000L
                if (left <= 0) throw SocketTimeoutException("deadline")
                setTimeout(left)
            }
            val n = ins.read(tmp)
            if (n < 0) break
            acc.write(tmp, 0, n)
            if (acc.size() > MAX_FILE) { log.w("mic_inject: input exceeds $MAX_FILE bytes"); return null }
            if (want == Long.MAX_VALUE && acc.size() >= 8) {
                val h = acc.toByteArray()
                if (String(h, 0, 4, Charsets.US_ASCII) == "RIFF") {
                    want = 8L + ((h[4].toLong() and 0xFF) or ((h[5].toLong() and 0xFF) shl 8) or
                        ((h[6].toLong() and 0xFF) shl 16) or ((h[7].toLong() and 0xFF) shl 24))
                }
            }
            if (acc.size() >= want) break
        }
        return acc.toByteArray()
    }
}
