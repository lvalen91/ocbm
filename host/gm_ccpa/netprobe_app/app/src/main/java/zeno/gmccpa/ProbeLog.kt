package zeno.gmccpa

import android.content.Context
import android.os.Build
import android.os.Process
import android.util.Log

/**
 * Logging for the whole instrument.
 *
 * **One logcat tag — `NETPROBE`** — so a single `adb logcat -s NETPROBE` captures the app's own
 * narrative with nothing missing. The tag is written from Rust as well (`native/carplay-jni/src/lib.rs`)
 * and grepped by `tools/tri_capture.sh`, so it must not be renamed. Inside that stream the
 * **subsystem** is a fixed-width bracketed field at the start of the line (`[ocbm]`, `[cprx ]`,
 * `[trace]`, …): `grep '\[cprx \]'`.
 *
 * ## How problems are actually pulled out of a capture
 *
 * The severity axis alone is NOT how. This KDoc used to say `adb logcat -s NETPROBE:W` isolates the
 * problems, and on the 2026-09-09 reference capture that filter returned seven lines — three of them
 * `*** MILESTONE` success markers — from a session that contained a 15 s MFi sign timeout, 42 dropped
 * Siri access units and six media underruns. Everything that went wrong was narrated at `I`.
 * Severity is now held to one rule — a fault is `E`, a degraded-but-working condition is `W`,
 * narration is `I` — and the thing that makes a capture answer "what went wrong" is
 * [zeno.gmccpa.logging.SessionTrace]: an expected step that never arrives is printed as
 * `!! EXPECTED-MISSING <step> — <the precondition that was observed>` at `E`, and the standing state
 * of every long-lived component is printed as `## STATUS` blocks. So:
 *
 *  - `grep 'EXPECTED-MISSING\|EXPECTED-LATE'` — steps that did not happen, or nearly did not;
 *  - `grep '## STATUS'` — what was up at any moment, from the nearest block;
 *  - `grep ' E NETPROBE'` — faults, now that `E` means fault.
 *
 * ## Two filters, not one — the app is more than its own tag
 *
 * `grep NETPROBE` is the app's *narrative*, but it is not the app's *activity*. On the reference
 * capture the app's PID emitted ~250 further lines under framework tags — `CCodec`, `CCodecConfig`,
 * `CCodecBuffers`, `MediaCodec`, `BufferQueueProducer`, `ViewRootImpl[CarPlayActivity]`,
 * `SurfaceUtils` — and those are exactly the lines that decide whether a decode problem is ours or
 * the platform's. They are attributable only by PID, which is why [identity] prints one at startup.
 * The recipe, on a generic `adb logcat > logcat.log`:
 *
 * ```
 * grep NETPROBE logcat.log                          # the app's own narrative
 * PID=$(grep -m1 'IDENTITY pid=' logcat.log | grep -o 'pid=[0-9]*' | cut -d= -f2)
 * awk -v p="$PID" '$3==p' logcat.log               # everything this process caused, framework included
 * ```
 *
 * The second filter is the one that keeps the whole-OS capture honest: it shows the app's lines in
 * order with the platform's, so the capture "ALSO maintains logs of other system processes" and
 * can still be cut down to what the app did.
 *
 * Using several *tags* was the obvious alternative and is worse in practice — logcat's `-s` filterspec
 * has no tag wildcard, so every new subsystem would have to be added to every command line, and
 * forgetting one silently drops output.
 *
 * There is deliberately NO on-screen mirror of this stream. A `mirror` hook used to be declared here
 * and documented as feeding an on-screen report — but nothing ever assigned it and no such view exists,
 * so every failure the KDoc promised would reach the driver ("native core unavailable", "advert did not
 * start", "connect-out: 10 attempts with no usable response") in fact reached logcat only. Rather than
 * leave a hook that reads as implemented, the driver-facing channel is the one that actually renders:
 * `Ui.setState`/`setDetail`, driven by [SessionSupervisor] with one sentence per phase.
 */
object ProbeLog {
    const val TAG = "NETPROBE"

    /** Subsystem names are padded to this width so the messages line up in a dense log. */
    private const val SUB_WIDTH = 5

    /**
     * Optional asset the build may drop in carrying `git rev-parse --short HEAD`. `build_apk.sh`
     * stamps the SHA into the APK *filename* only, and `pm install` discards the filename, so today
     * this is absent and [identity] says so rather than inventing one. Adding the asset is a
     * one-line change in the build script; nothing here needs to change when it lands.
     */
    private const val BUILD_SHA_ASSET = "build_sha"

    class Logger internal constructor(private val sub: String?, private val enabled: Boolean) {
        fun i(msg: String) { if (enabled) write(Log.INFO, sub, msg) }
        fun w(msg: String) { if (enabled) write(Log.WARN, sub, msg) }
        fun e(msg: String) { if (enabled) write(Log.ERROR, sub, msg) }
    }

    /** A logger tagged with a subsystem, e.g. `ProbeLog.sub("ocbm")`. */
    fun sub(name: String): Logger = Logger(name, true)

    /** A logger that discards everything — used by the headless self-test so it isn't noisy. */
    fun silent(): Logger = Logger(null, false)

    /** Already-formatted probe output (section headers and the network probes' own lines). */
    fun raw(msg: String) = write(Log.INFO, null, msg)

    /** A run marker, so one session is findable in a long capture. */
    fun banner(msg: String) = write(Log.INFO, null, "===== $msg =====")

    /**
     * The PID anchor: two lines that let a generic whole-OS capture be cut down to this process.
     *
     * Before this, the PID reached logcat only from `UsbAttachActivity` (the attach path), so a
     * launcher or `am start` session had no anchor at all and the ~250 framework lines the app's PID
     * emits per session were un-attributable. Printed by `MainActivity.onCreate` on every Activity
     * generation — a repeat is a fresh anchor, not noise, and the PID changes across a process death
     * so a capture spanning two processes needs both.
     *
     * Line 1 is the greppable one (`IDENTITY pid=`); line 2 is the platform the framework lines came
     * from, so "is it ours or the platform's" can be answered without a second command. The apk's
     * size and install time stand in for a build SHA until [BUILD_SHA_ASSET] exists — two builds
     * with the same size and mtime are the same build for every practical purpose.
     */
    fun identity(ctx: Context) {
        val pkg = ctx.packageName
        val uid = Process.myUid()
        val ver = runCatching {
            @Suppress("DEPRECATION")
            ctx.packageManager.getPackageInfo(pkg, 0).versionName
        }.getOrNull() ?: "?"
        val src = ctx.applicationInfo.sourceDir ?: "?"
        val apk = java.io.File(src)
        val sha = runCatching {
            ctx.assets.open(BUILD_SHA_ASSET).use { it.readBytes().toString(Charsets.UTF_8).trim() }
        }.getOrNull()
        write(Log.INFO, null,
            "===== IDENTITY pid=${Process.myPid()} uid=$uid user=${uid / 100_000} pkg=$pkg v=$ver " +
            "build=${sha ?: "n/a (no assets/$BUILD_SHA_ASSET)"} " +
            "apk=${apk.length()}B@${apk.lastModified()} $src =====")
        write(Log.INFO, null,
            "===== PLATFORM ${Build.MANUFACTURER} ${Build.MODEL} android=${Build.VERSION.RELEASE} " +
            "sdk=${Build.VERSION.SDK_INT} build=${Build.DISPLAY} " +
            "(filter: grep $TAG for the narrative; awk '\$3==${Process.myPid()}' for everything this process logged) =====")
    }

    private fun write(level: Int, sub: String?, msg: String) {
        val line = if (sub == null) msg else "[${sub.padEnd(SUB_WIDTH)}] $msg"
        // Log.println rather than Log.i/w/e so the level is a value, not a call-site choice.
        Log.println(level, TAG, line)
    }
}
