package zeno.gmccpa.logging

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.SystemClock
import zeno.gmccpa.ProbeLog
import java.io.BufferedReader
import java.io.File
import java.io.IOException
import java.io.InputStreamReader
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * Always-on, untethered log capture: drains this device's logcat into rotating, byte-budgeted files
 * under `filesDir/logs/` so a fault that happens with no Mac attached is still recoverable.
 *
 * ## Why logcat and not a [ProbeLog] listener
 * The reference implementation this is ported from (`carlink_native/.../logging/FileLogManager.kt`)
 * hooks its logger's listener interface. That is wrong here, because only *half* of this app's
 * output goes through [ProbeLog]. The Rust receiver core emits ~138 `println!`/`eprintln!` calls
 * which `native/carplay-jni/src/lib.rs` dup2()s onto fd 1/2 and pushes straight to
 * `__android_log_write` under the same `NETPROBE` tag with a `[rust ]` prefix — they never touch
 * Kotlin. Draining logcat is the only capture point that sees both sides in one ordered stream, and
 * it is also the only one that is a *time machine*: `logcat -d` returns what is ALREADY in the ring
 * buffer, so a capture started after a fault still recovers the fault.
 *
 * ## Scopes
 * - [Scope.OWN_PROCESS] — always available. An app may always read its own entries; `--pid=<us>`
 *   makes that explicit. Sees `NETPROBE` (Kotlin) and `[rust ]` (JNI core) and nothing else.
 * - [Scope.WHOLE_OS] — needs `android.permission.READ_LOGS`, whose protection level is
 *   `signature|privileged|development`. The `development` bit is why it is reachable at all:
 *   `adb shell pm grant <our-package> android.permission.READ_LOGS` — see [grantCmd], which fills
 *   in the package from the running app rather than a literal (it was a literal, and it went stale
 *   at the 2026-09-08 package revert; fixed 2026-09-09). Two operational
 *   traps, both of which this class detects and logs rather than failing silently:
 *   1. `pm grant` REJECTS a permission the manifest does not request. `AndroidManifest.xml` must
 *      carry `<uses-permission android:name="android.permission.READ_LOGS"/>` or the grant errors
 *      out. (Declaring it is harmless when ungranted — it stays denied.) Not done in this class;
 *      see the wiring task.
 *   2. READ_LOGS is mapped to the supplementary gid `log`, and supplementary gids are assigned at
 *      **fork time**. After a grant, the already-running process still lacks the gid:
 *      `checkSelfPermission` says GRANTED while logd still filters output to our own uid. So the
 *      permission check alone is not proof, and [Session.verifyScope] additionally checks whether
 *      any captured line carries a pid other than ours, warning with the exact `am force-stop`
 *      command when it does not.
 *
 * Whole-OS degrades to own-process automatically when the permission is not held, with the exact
 * `pm grant` line in the log. It never silently captures nothing.
 *
 * ## Volume, and why WHOLE_OS is filtered by default
 * Measured on this unit: `evidence/drive_20260818-132220/headunit.log` is 923 421 lines / 147 MB
 * over a 39-minute leg — ~227 MB/h unfiltered, of which 88 % is V/D and 60 % is two Codec2 tags. A
 * multi-day unattended capture at that rate is not storable, so [DEFAULT_SYSTEM_FILTER] keeps V for
 * the tags that matter to the two faults being chased (USB attach permission prompt, iPhone BT
 * auto-reconnect) plus `NETPROBE`, and puts a `*:I` floor under everything else. That is ~27 MB/h,
 * which the 768 MB ceiling turns into roughly a day of rolling history. Pass `filterSpec =
 * listOf("*:V")` to capture everything and accept a ~3 h window instead.
 *
 * ## Threading model
 * Four threads touch a running session; each owns a distinct stage of a one-way pipeline and no
 * stage ever blocks the one behind it.
 *
 * 1. **Caller threads** — [start]/[stop]/[flush]/[status], any thread. [start] and [stop] serialize
 *    on [stateLock] and are idempotent; [flush] and [status] read the session through a volatile
 *    and take no engine lock at all, so they cannot be blocked by a stop in progress.
 * 2. **Pump thread** (one per session, daemon) — probes buffers, spawns `logcat`, and does the only
 *    blocking read. It *never* touches the disk: it hands lines to an in-memory queue and returns
 *    to the read immediately. This is deliberate. If the pump stalled on I/O, the child's stdout
 *    pipe would fill (64 KB), logcat would block, and logd would drop us as a slow reader — the
 *    capture would lose exactly the burst it exists to record.
 * 3. **stderr drain thread** (one per spawned child, daemon, exits at EOF) — exists solely so a
 *    child that writes diagnostics to stderr cannot wedge on a full stderr pipe. Its first 2 KB is
 *    logged when the child dies.
 * 4. **Flush executor** (single-threaded scheduler) — the only routine writer. Drains the queue at
 *    1 Hz and sweeps retention/ceiling at 1/60 Hz.
 *
 * **Locks and their order.** `stateLock` (start/stop mutual exclusion) > `flushLock` (one drainer
 * at a time, so two concurrent drains cannot interleave batches on disk) > `LogFiles.lock` (all
 * file state). `procLock` (the live [Process] handle plus the running flag) is a leaf taken only by
 * the pump and by [Session.stop], and is never held together with `flushLock` or `LogFiles.lock`.
 * The order is total and no thread acquires them in any other sequence, so the set is
 * deadlock-free. Held-across-blocking is bounded everywhere: `stateLock` is held across a bounded
 * `join`/`awaitTermination` in [stop], which is the one place a caller can wait on another thread.
 *
 * **Accepted couplings and races**, in full:
 * - [LogFiles] calls [ProbeLog] while holding its own lock. `ProbeLog.write` is now a bare
 *   `Log.println` with no fan-out, so nothing user-supplied executes under the writer lock. (This
 *   used to describe a `ProbeLog.mirror` hook said to append to a UI text buffer; the hook was never
 *   assigned and the buffer never existed. It has been removed rather than left to imply otherwise.)
 * - Drop counters are `getAndSet(0)` by the flush executor while the pump increments them. A count
 *   or two can land in the next marker instead of the current one. The total is never lost.
 * - [stop] proceeds after a bounded `join`. A pump thread that has not exited may enqueue a few
 *   more lines; the writer is closed by then, so they are discarded. They are from a killed child.
 * - Seam de-duplication is timestamp-ordered and logcat is not perfectly ordered across buffers.
 *   See [SeamFilter] for the exact window in which that can drop a line, and why it is bounded.
 *
 * ## Backpressure
 * The queue is capped in **characters**, not entries, because characters are what bound the heap
 * (~2 bytes each in UTF-16 plus per-String overhead). Above the cap the pump drops the *newest*
 * line, O(1), and counts it; when the queue drains, a `##### ... DROPPED n lines` marker is written
 * into the file. A silent gap in a fault capture is the one failure worse than a gap.
 */
object LogCapture {

    /** Which entries the capture can see. [WHOLE_OS] is a request; the effective scope may differ. */
    enum class Scope { OWN_PROCESS, WHOLE_OS }

    /**
     * @param scope requested scope; [Scope.WHOLE_OS] degrades to [Scope.OWN_PROCESS] without READ_LOGS.
     * @param filterSpec logcat filterspec, applied to [Scope.WHOLE_OS] only — own-process volume is
     *   ours to control and is captured at `*:V`. MUST end with a `*:LEVEL` term or logcat's default
     *   for unlisted tags is left to the build's `ANDROID_LOG_TAGS`.
     * @param includeKernel request `-b kernel`. Off by default: it is probably unreadable from an
     *   app even with READ_LOGS (`tools/drive_capture.sh` gets `-b all` only because it runs as the
     *   adb shell user), and it is voluminous when it is readable. The buffer is probed and its
     *   readability recorded in every file header either way.
     * @param appVersion recorded in the header; keep in sync with `AndroidManifest.xml` versionName.
     */
    data class Config(
        val scope: Scope = Scope.WHOLE_OS,
        val filterSpec: List<String> = DEFAULT_SYSTEM_FILTER,
        val includeKernel: Boolean = false,
        val appVersion: String = "4.0",
    )

    /** A snapshot; every field is read from a volatile or an atomic, so it is never torn but may be stale. */
    data class Status(
        val running: Boolean,
        val requestedScope: Scope,
        /**
         * False until the pump thread has run its scope probe. [effectiveScope] and
         * [readLogsGranted] hold their pre-probe defaults ([Config.scope] and `false`) while it is
         * false, so a caller that samples right after [start] must not print them as fact — the
         * authoritative line is the pump's own `capturing requested=… effective=… read_logs=…`.
         */
        val resolved: Boolean,
        val effectiveScope: Scope,
        val readLogsGranted: Boolean,
        val buffers: List<String>,
        val sessionId: String,
        val currentFile: String?,
        val fileCount: Int,
        val bytesOnDisk: Long,
        val linesWritten: Long,
        val linesDropped: Long,
        val restarts: Int,
    )

    const val LOGS_DIR = "logs"

    /**
     * The `pm grant` that unlocks [Scope.WHOLE_OS]. Logged verbatim whenever the permission is
     * missing, so the operator can copy-paste it.
     *
     * The package name is read from the running app ([Context.getPackageName]) and never written
     * down here. It used to be the `const val GRANT_CMD = "… android.car.usb.handler …"` literal,
     * which silently went stale when the fixed-handler package squat was reverted to `zeno.gmccpa`
     * on 2026-09-08: the app kept printing a grant recipe for a package that no longer exists on
     * the device, and `pm grant` fails with nothing but "Unknown package". Derived, it cannot drift
     * across the next rename (fixed 2026-09-09). BuildConfig is deliberately NOT used — the
     * canonical build (`tools/build_apk.sh`) is a raw kotlinc compile that generates no
     * BuildConfig class, and this module does not enable `buildFeatures { buildConfig true }`.
     */
    fun grantCmd(ctx: Context): String = grantCmd(ctx.packageName)

    /** @see grantCmd */
    fun grantCmd(pkg: String): String = "adb shell pm grant $pkg android.permission.READ_LOGS"

    /** …and the half everyone forgets: the `log` gid is only picked up by a freshly forked process. */
    fun forceStopCmd(ctx: Context): String = forceStopCmd(ctx.packageName)

    /** @see forceStopCmd */
    fun forceStopCmd(pkg: String): String = "adb shell am force-stop $pkg"

    /**
     * V for what the two open faults live in, `*:I` under everything else. Derived from a real
     * capture, not guessed: in `evidence/drive_20260818-132220` the USB and Bluetooth lines of
     * interest are almost all D/V, while the two Codec2 tags that produce 60 % of all lines are pure
     * V and vanish under the `*:I` floor.
     */
    val DEFAULT_SYSTEM_FILTER: List<String> = listOf(
        "NETPROBE:V",
        // USB attach / permission prompt path
        "car.usb.handle:V", "UsbAttachActivity:V", "UsbHostManagementActivity:V", "UsbDeviceManager:V", "UsbHostManager:V",
        "UsbPortManager:V", "UsbService:V", "UsbSettingsManager:V", "UsbProfileGroupSettingsManager:V",
        "UsbPermissionActivity:V", "UsbResolverActivity:V", "UsbConfirmActivity:V",
        "UsbDescriptorParser:V", "UsbUtils:V", "RDMS.UsbReceiver:V", "USBMountReceiver:V",
        // Bluetooth auto-reconnect path
        "BluetoothManagerService:V", "BluetoothAdapterService:V", "BluetoothAdapter:V",
        "BluetoothDevice:V", "bluetooth:V", "bt_stack:V", "bt_btm:V", "bt_btif:V", "bt_bta:V",
        "bt_vendor:V", "BT_CAR_LOG:V", "CAR.BluetoothDeviceConnectionPolicy:V",
        "CarBluetoothService:V", "CarBluetoothUserService:V", "A2dpService:V", "A2dpSinkService:V",
        "HeadsetService:V", "HeadsetClientService:V", "BluetoothPan:V",
        // enough system context to place the above in a lifecycle
        "ActivityManager:V", "ActivityTaskManager:V",
        "*:I",
    )

    private val stateLock = ReentrantLock()

    @Volatile
    private var session: Session? = null

    /**
     * Starts capture. Idempotent: returns false if a session is already running, and does not
     * disturb it. Non-blocking — buffer probing spawns short-lived subprocesses and happens on the
     * pump thread, so this is safe to call from the main thread. Whether capture actually works is
     * reported by [status] and written into the file header, not by the return value.
     */
    fun start(context: Context, config: Config = Config()): Boolean = stateLock.withLock {
        val existing = session
        if (existing != null) {
            // A live session owns the directory; leave it alone.
            if (existing.status().running) return@withLock false
            // Otherwise it stopped ITSELF — disk-critical, an unreadable buffer, or a failed open —
            // and nothing clears `session` on those paths (sweepQuietly deliberately cannot call
            // stop() from under its own lock). Without this, one transient low-disk event would make
            // every later start() a silent no-op for the life of the process, which on the USB-attach
            // path means capture never comes back for the rest of the drive. Reap it and continue.
            ProbeLog.sub("cap").w("previous capture session had stopped itself — replacing it")
            existing.stop(2_000L)
            session = null
        }
        // A fresh Session per start: all queue/counter/seam/file state is per-session, so a
        // stop/start cycle cannot inherit a half-torn-down predecessor's state.
        val s = Session(context.applicationContext, config)
        session = s
        s.start()
        true
    }

    /**
     * Stops capture and flushes. Idempotent. Blocks the caller for at most roughly [timeoutMs]
     * while the pump thread and flush executor wind down, then finishes the drain on the caller's
     * thread so the last lines are on disk when this returns.
     */
    fun stop(timeoutMs: Long = 5_000L) = stateLock.withLock {
        val s = session ?: return@withLock
        session = null
        s.stop(timeoutMs)
    }

    /**
     * Drains the queue to disk synchronously. Safe from any thread; blocks the caller for the
     * duration of one write, so do not call it from the UI thread inside a burst. No-op when
     * stopped, or when the session has not opened its first file yet.
     */
    fun flush() {
        session?.flush()
    }

    fun status(): Status = session?.status() ?: Status(
        running = false, requestedScope = Scope.OWN_PROCESS, resolved = false,
        effectiveScope = Scope.OWN_PROCESS,
        readLogsGranted = false, buffers = emptyList(), sessionId = "-", currentFile = null,
        fileCount = 0, bytesOnDisk = 0L, linesWritten = 0L, linesDropped = 0L, restarts = 0,
    )

    /** Where the files are, whether or not a session is running. */
    fun logsDir(context: Context): File = File(context.applicationContext.filesDir, LOGS_DIR)

    /**
     * Every capture file on disk, oldest first, across sessions and scopes. Names sort
     * chronologically. For the later SAF/USB export task — `run-as` is blocked on this head unit
     * (see the comment at `AvSink.kt:23`), so this list is the only handle an exporter gets.
     */
    fun logFiles(context: Context): List<File> =
        logsDir(context)
            .listFiles { f -> f.isFile && f.name.startsWith(LogFiles.FILE_PREFIX) && f.name.endsWith(".log") }
            ?.sortedBy { it.name } ?: emptyList()
}

// =================================================================================================

/**
 * One capture run. Created by [LogCapture.start], discarded by [LogCapture.stop]; never reused.
 *
 * The pump thread's life cycle is a straight line, which is what makes it reviewable:
 * `probe format -> probe buffers -> open files -> write header -> dump backlog -> tail -> (die) ->
 * marker + backoff -> tail -> ...` and it exits only when [running] goes false.
 */
private class Session(private val ctx: Context, private val cfg: LogCapture.Config) {

    private val log = ProbeLog.sub("logcap")

    // --- immutable-after-init, published to other threads via the LogFiles lock (volatile anyway) --
    @Volatile private var effectiveScope = cfg.scope
    @Volatile private var readLogsGranted = false
    /** Set once [resolveScope] has run; the two fields above are pre-probe defaults until then. */
    @Volatile private var scopeResolved = false
    @Volatile private var buffersOk: List<String> = emptyList()
    @Volatile private var buffersFailed: List<String> = emptyList()
    @Volatile private var formatArgs: List<String> = FORMAT_YEAR_UTC
    @Volatile private var hasYear = true

    private val sessionId: String = utcStamp(System.currentTimeMillis())
    private val startedAtMs = System.currentTimeMillis()

    private val queue = ConcurrentLinkedQueue<String>()
    private val queuedChars = AtomicLong(0)
    private val droppedLines = AtomicLong(0)
    private val droppedChars = AtomicLong(0)
    private val linesWritten = AtomicLong(0)
    private val restarts = AtomicInteger(0)

    /** "the pump should keep reading" — cleared by [stop] and by the pump's own abort paths. */
    private val running = AtomicBoolean(false)

    /** "teardown has run" — the latch for [stop]'s idempotence. Deliberately NOT [running]: the
     *  pump clears that by itself when it aborts, and reusing it would make the subsequent stop()
     *  a no-op that leaks the executor and the open file. */
    private val torndown = AtomicBoolean(false)
    private val procLock = ReentrantLock()
    private var child: Process? = null

    private val flushLock = ReentrantLock()
    private lateinit var pump: Thread
    private val exec: ScheduledExecutorService =
        Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "logcap-flush").apply { isDaemon = true } }

    private val wholeOs = cfg.scope == LogCapture.Scope.WHOLE_OS
    private val budget = if (wholeOs) BUDGET_WHOLE_OS else BUDGET_OWN
    private val queueCharCap = if (wholeOs) QUEUE_CHARS_WHOLE else QUEUE_CHARS_OWN
    private val files = LogFiles(
        dir = File(ctx.filesDir, LogCapture.LOGS_DIR),
        budget = budget,
        namePrefix = if (wholeOs) "os" else "own",
        otherNamePrefix = if (wholeOs) "own" else "os",
        otherRetentionMs = (if (wholeOs) BUDGET_OWN else BUDGET_WHOLE_OS).retentionMs,
        sessionId = sessionId,
        header = ::header,
    )

    private val seam = SeamFilter({ hasYear }) { note -> enqueue(marker(note), critical = true) }
    @Volatile private var verified = false
    private var verifyDeadlineMs = 0L
    @Volatile private var diskStopped = false

    /**
     * True only while the one-shot `logcat -d` backlog dump is being read. In that phase a full
     * queue must NOT drop: the backlog is the fault that predates app start, the entire reason this
     * engine exists, and a whole-OS ring dump arrives far faster than a 1 Hz writer retires it. In
     * the tail phase the opposite is true — stalling the reader stalls the pipe and logd evicts a
     * slow reader — so there it drops. Written by the pump, read by the pump; volatile only because
     * it is set before the thread's first loop iteration.
     */
    @Volatile private var drainBackpressure = false

    fun start() {
        running.set(true)
        pump = Thread({
            // Android's default uncaught-exception handler KILLS THE PROCESS, from any thread. A
            // stop() that interrupts this thread while it is in Process.waitFor()/Thread.join()
            // raises InterruptedException, which is unchecked in Kotlin — unguarded, shutting the
            // capture down would take the app with it. Nothing escapes this frame.
            try {
                pumpLoop()
            } catch (t: Throwable) {
                log.e("pump died: ${t.javaClass.simpleName}: ${t.message}")
            } finally {
                running.set(false)
                procLock.withLock { child?.destroy(); child = null }
            }
        }, "logcap-pump").apply { isDaemon = true; start() }
    }

    /**
     * Ordering is the whole content of this method. `running=false` first so the pump cannot spawn
     * a replacement child; `destroy()` second because that is what unblocks a pump parked in
     * `readLine()` (an interrupt does not — `InputStream.read` is not interruptible); `interrupt()`
     * third for a pump parked in the backoff sleep; then a bounded join, then the executor, then a
     * final drain on this thread so the caller's "it is on disk" expectation holds.
     */
    fun stop(timeoutMs: Long) {
        if (torndown.getAndSet(true)) return
        running.set(false)
        procLock.withLock { child?.destroy() }
        // Self-join would hang forever. Nothing internal calls stop() today; this costs one branch
        // and makes it safe if the wiring task ever calls it from a capture callback.
        if (::pump.isInitialized && pump !== Thread.currentThread()) {
            pump.interrupt()
            try { pump.join(timeoutMs) } catch (e: InterruptedException) { Thread.currentThread().interrupt() }
            if (pump.isAlive) log.w("pump did not exit in ${timeoutMs}ms — proceeding (daemon)")
        }
        exec.shutdown()
        try {
            if (!exec.awaitTermination(EXEC_SHUTDOWN_MS, TimeUnit.MILLISECONDS)) exec.shutdownNow()
        } catch (e: InterruptedException) {
            exec.shutdownNow(); Thread.currentThread().interrupt()
        }
        enqueue(marker("capture stopped — lines=${linesWritten.get()} dropped=${droppedLines.get()} restarts=${restarts.get()}"), critical = true)
        drain()
        files.close()
        log.i("stopped — ${linesWritten.get()} lines, ${droppedLines.get()} dropped, ${restarts.get()} restarts")
    }

    fun flush() = drain()

    fun status() = LogCapture.Status(
        running = running.get(),
        requestedScope = cfg.scope,
        resolved = scopeResolved,
        effectiveScope = effectiveScope,
        readLogsGranted = readLogsGranted,
        buffers = buffersOk,
        sessionId = sessionId,
        currentFile = files.currentFileName(),
        fileCount = files.fileCount(),
        bytesOnDisk = files.totalBytes(),
        linesWritten = linesWritten.get(),
        linesDropped = droppedLines.get(),
        restarts = restarts.get(),
    )

    // --- pump thread -----------------------------------------------------------------------------

    private fun pumpLoop() {
        resolveScope()
        resolveFormat()
        resolveBuffers()
        if (buffersOk.isEmpty()) {
            log.e("no readable logcat buffer — capture aborted")
            running.set(false)
            return
        }
        if (!files.open()) {
            running.set(false)
            return
        }
        exec.scheduleWithFixedDelay(::drainQuietly, FLUSH_MS, FLUSH_MS, TimeUnit.MILLISECONDS)
        exec.scheduleWithFixedDelay(::sweepQuietly, SWEEP_MS, SWEEP_MS, TimeUnit.MILLISECONDS)
        log.i("capturing requested=${cfg.scope} effective=$effectiveScope read_logs=$readLogsGranted buffers=${buffersOk.joinToString(",")} -> ${files.currentFileName()}")
        verifyDeadlineMs = SystemClock.elapsedRealtime() + VERIFY_MS

        var backoffMs = BACKOFF_MIN_MS
        var phase = Phase.BACKLOG
        drainBackpressure = true
        while (running.get()) {
            // Arm before every spawn after the first: a plain `logcat` re-dumps the WHOLE ring
            // buffer before it starts following, so the overlap to suppress is not a line or two,
            // it is everything we have already written. See SeamFilter.
            if (phase != Phase.BACKLOG) seam.arm()
            val args = argsFor(phase)
            val t0 = SystemClock.elapsedRealtime()
            val rc = runChild(args)
            val aliveMs = SystemClock.elapsedRealtime() - t0
            if (!running.get()) break

            if (phase == Phase.BACKLOG) {
                // `logcat -d` terminating is success, not a fault: no marker, no backoff.
                phase = Phase.TAIL
                drainBackpressure = false
                continue
            }
            restarts.incrementAndGet()
            // Reset the backoff only after a child that actually stayed up. Resetting on every exit
            // would let a child that dies after 3 s spin at the minimum interval forever. Computed
            // before the marker so the marker quotes the interval actually about to be slept.
            backoffMs = if (aliveMs >= STABLE_MS) BACKOFF_MIN_MS else (backoffMs * 2).coerceAtMost(BACKOFF_MAX_MS)
            enqueue(
                marker(
                    "DISCONTINUITY — logcat tail exited rc=$rc after ${aliveMs}ms; restarting in ${backoffMs}ms. " +
                        "The gap is recovered from the ring buffer on restart UNLESS the ring wrapped during it."
                ),
                critical = true,
            )
            if (!sleepInterruptibly(backoffMs)) break
        }
        procLock.withLock { child?.destroy(); child = null }
    }

    private enum class Phase { BACKLOG, TAIL }

    private fun argsFor(phase: Phase): List<String> = buildList {
        add(logcatBin())
        addAll(formatArgs)
        buffersOk.forEach { add("-b"); add(it) }
        if (phase == Phase.BACKLOG) add("-d")
        if (effectiveScope == LogCapture.Scope.OWN_PROCESS) {
            add("--pid=${android.os.Process.myPid()}")
            add("*:V")
        } else {
            addAll(cfg.filterSpec)
        }
    }

    /**
     * Spawns a child and reads its stdout to EOF on this thread. Returns the exit code, or -1 if the
     * spawn failed. Never lets the child wedge: stdin is closed immediately (so anything that reads
     * it sees EOF rather than blocking) and stderr is drained by its own daemon thread, because an
     * unread stderr pipe filling at 64 KB would freeze logcat with no symptom other than a capture
     * that quietly stops.
     */
    private fun runChild(args: List<String>): Int {
        val proc = try {
            ProcessBuilder(args).redirectErrorStream(false).start()
        } catch (e: IOException) {
            log.e("spawn failed: ${e.message} (${args.take(2).joinToString(" ")})")
            return -1
        }
        // Publish under procLock and re-check running: stop() may have run between the check at the
        // top of the loop and here, in which case it destroyed a child we had not published yet and
        // this one must be killed by us instead of outliving the session.
        val accepted = procLock.withLock {
            if (!running.get()) false else { child = proc; true }
        }
        if (!accepted) { proc.destroy(); return -1 }

        try { proc.outputStream.close() } catch (e: IOException) { /* nothing writes to it anyway */ }
        val errBuf = StringBuilder()
        val errThread = Thread({
            try {
                proc.errorStream.bufferedReader().forEachLine { l ->
                    synchronized(errBuf) { if (errBuf.length < STDERR_KEEP) errBuf.append(l).append('\n') }
                }
            } catch (e: IOException) { /* stream torn down with the child */ }
        }, "logcap-stderr").apply { isDaemon = true; start() }

        try {
            BufferedReader(InputStreamReader(proc.inputStream, Charsets.UTF_8), READ_BUF).use { br ->
                while (running.get()) consume(br.readLine() ?: break)
            }
        } catch (e: IOException) {
            // Expected on stop(): destroy() tears the pipe out from under the read.
            if (running.get()) log.w("read: ${e.message}")
        }

        // Reap BEFORE killing. The read loop ended at EOF in the normal case, so the child has
        // already exited and exitValue() is the real reason it died — which is what the
        // discontinuity marker is for. destroy()-then-waitFor() would report our own SIGKILL every
        // time and make every restart look identical.
        var rc = -1
        try {
            if (proc.waitFor(CHILD_REAP_MS, TimeUnit.MILLISECONDS)) {
                rc = proc.exitValue()
            } else {
                proc.destroy()
                if (proc.waitFor(CHILD_REAP_MS, TimeUnit.MILLISECONDS)) rc = proc.exitValue()
            }
        } catch (e: InterruptedException) {
            proc.destroy()
            Thread.currentThread().interrupt()
        }
        procLock.withLock { if (child === proc) child = null }
        try { errThread.join(STDERR_JOIN_MS) } catch (e: InterruptedException) { Thread.currentThread().interrupt() }
        val err = synchronized(errBuf) { errBuf.toString().trim() }
        if (err.isNotEmpty()) log.w("logcat stderr: ${err.take(400)}")
        return rc
    }

    private fun consume(line: String) {
        if (!seam.accept(line)) return
        if (!verified) verifyScope(line)
        enqueue(line, critical = false)
    }

    /**
     * READ_LOGS can be granted-but-ineffective (the `log` gid is only acquired at fork), and the
     * failure is invisible: logcat runs, produces output, and every line is ours. Detect it by the
     * only observable that distinguishes the two states — a line from a foreign pid — and say
     * exactly what to do about it. Costs one tokenisation per line for at most VERIFY_MS.
     *
     * This is driven by captured lines, so it needs traffic to fire. It always has some: the
     * engine's own `ProbeLog.sub("logcap")` status line is written at the top of [pumpLoop] and is
     * itself captured, so even a completely idle unit delivers a line inside the window.
     */
    private fun verifyScope(line: String) {
        if (effectiveScope != LogCapture.Scope.WHOLE_OS) { verified = true; return }
        val pid = pidOf(line)
        if (pid != -1 && pid != android.os.Process.myPid()) {
            verified = true
            return
        }
        if (SystemClock.elapsedRealtime() < verifyDeadlineMs) return
        verified = true
        effectiveScope = LogCapture.Scope.OWN_PROCESS
        val why = "READ_LOGS reports granted but only our own pid is visible — the `log` gid is " +
            "assigned at fork, so the grant does not reach a process that was already running. " +
            "Restart the app: ${LogCapture.forceStopCmd(ctx)}"
        log.w(why)
        enqueue(marker("SCOPE DEGRADED — $why"), critical = true)
    }

    /** pid is token 2 in `-v threadtime` with or without the year modifier; -1 when unparseable. */
    private fun pidOf(line: String): Int {
        var i = 0
        var field = 0
        val n = line.length
        while (i < n && field < 2) {
            while (i < n && line[i] != ' ') i++
            while (i < n && line[i] == ' ') i++
            field++
        }
        var v = 0
        var any = false
        while (i < n && line[i] in '0'..'9') { v = v * 10 + (line[i] - '0'); i++; any = true }
        return if (any) v else -1
    }

    private fun sleepInterruptibly(ms: Long): Boolean = try {
        Thread.sleep(ms); running.get()
    } catch (e: InterruptedException) {
        Thread.currentThread().interrupt(); false
    }

    // --- startup probing (pump thread only, before any file is opened) ----------------------------

    private fun resolveScope() {
        readLogsGranted =
            ctx.checkSelfPermission(android.Manifest.permission.READ_LOGS) == PackageManager.PERMISSION_GRANTED
        if (cfg.scope == LogCapture.Scope.WHOLE_OS && !readLogsGranted) {
            effectiveScope = LogCapture.Scope.OWN_PROCESS
            log.w("READ_LOGS not held — capturing own process only. To capture the whole OS:")
            log.w("  1. AndroidManifest.xml must declare <uses-permission android:name=\"android.permission.READ_LOGS\"/> (pm grant rejects an undeclared permission)")
            log.w("  2. ${LogCapture.grantCmd(ctx)}")
            log.w("  3. ${LogCapture.forceStopCmd(ctx)}   (the `log` gid is only picked up by a fresh process)")
        }
        scopeResolved = true
    }

    /**
     * `-v year -v UTC` is not cosmetic. Without `year`, timestamps are `MM-DD` and the seam filter's
     * ordering breaks across new year; without `UTC`, a DST fold makes an hour of timestamps
     * non-monotonic and a post-boot timezone change shifts the whole stream. Both modifiers exist on
     * Android 7+, so they should be present at API 32 — but an unsupported `-v` makes logcat exit
     * immediately, which would be a total capture failure, so it is probed and falls back.
     */
    private fun resolveFormat() {
        if (probe(listOf(logcatBin()) + FORMAT_YEAR_UTC + listOf("-b", "main", "-t", "1")).first == 0) {
            formatArgs = FORMAT_YEAR_UTC; hasYear = true
        } else {
            formatArgs = FORMAT_PLAIN; hasYear = false
            log.w("logcat rejected -v year/-v UTC — falling back to bare threadtime (MM-DD, local time)")
        }
    }

    /**
     * Probes each candidate buffer on its own and keeps the ones that answer. Requesting a buffer
     * that cannot be opened can make logcat exit outright, which would take the readable buffers
     * down with it — so this never assumes, least of all about `kernel`, which
     * `tools/drive_capture.sh` only gets because it runs as the adb shell user.
     *
     * NEEDS ON-DEVICE VERIFICATION: which of `events`, `radio` and `kernel` an untrusted_app can
     * actually open on this GM unit is unknown. Whatever the answer, it is recorded in every file
     * header, so a capture is never ambiguous about what it contains.
     */
    private fun resolveBuffers() {
        val candidates = when {
            effectiveScope == LogCapture.Scope.OWN_PROCESS -> listOf("main", "system", "crash")
            cfg.includeKernel -> listOf("main", "system", "crash", "events", "radio", "kernel")
            else -> listOf("main", "system", "crash", "events", "radio")
        }
        val ok = ArrayList<String>()
        val bad = ArrayList<String>()
        for (b in candidates) {
            val (rc, err) = probe(listOf(logcatBin()) + formatArgs + listOf("-b", b, "-t", "1"))
            if (rc == 0) ok += b else bad += "$b(rc=$rc${if (err.isEmpty()) "" else ": ${err.take(80)}"})"
        }
        buffersOk = ok
        buffersFailed = bad
        if (bad.isNotEmpty()) log.w("buffers unavailable: ${bad.joinToString(" ")}")
        if (!cfg.includeKernel && effectiveScope == LogCapture.Scope.WHOLE_OS) {
            val (rc, _) = probe(listOf(logcatBin()) + formatArgs + listOf("-b", "kernel", "-t", "1"))
            log.i("kernel buffer ${if (rc == 0) "IS readable — enable with Config(includeKernel=true)" else "not readable (rc=$rc), as expected"}")
        }
    }

    /**
     * Runs a short-lived logcat and returns (exit code, stderr). Waits for exit BEFORE reading the
     * pipes — the child has closed them by then and the buffered bytes are still readable, which
     * avoids the read-then-wait deadlock a child that never exits would otherwise cause. `-t 1`
     * output is a single line, far below the 64 KB pipe buffer, so it cannot block the child.
     */
    private fun probe(args: List<String>): Pair<Int, String> = try {
        val p = ProcessBuilder(args).start()
        p.outputStream.close()
        if (!p.waitFor(PROBE_TIMEOUT_MS, TimeUnit.MILLISECONDS)) { p.destroyForcibly(); p.waitFor() }
        val err = p.errorStream.readBytes().toString(Charsets.UTF_8).trim()
        p.inputStream.close()
        p.exitValue() to err
    } catch (e: IOException) {
        -1 to (e.message ?: "spawn failed")
    } catch (e: InterruptedException) {
        Thread.currentThread().interrupt(); -1 to "interrupted"
    }

    /** Absolute path first: an app process's PATH is not guaranteed to contain /system/bin. */
    private fun logcatBin(): String =
        if (File("/system/bin/logcat").canExecute()) "/system/bin/logcat" else "logcat"

    // --- queue and writer -------------------------------------------------------------------------

    /**
     * O(1) enqueue with a character-counted cap. `critical = true` bypasses the cap: markers are
     * rare (one per restart or scope change) and a marker lost to backpressure is precisely the
     * information a reader needs when backpressure is happening.
     */
    private fun enqueue(line: String, critical: Boolean) {
        if (!critical && queuedChars.get() >= queueCharCap) {
            if (drainBackpressure && waitForRoom()) {
                // room freed — fall through and enqueue
            } else {
                droppedLines.incrementAndGet()
                droppedChars.addAndGet(line.length.toLong() + 1)
                return
            }
        }
        queue.offer(line)
        queuedChars.addAndGet(line.length.toLong() + 1)
    }

    /**
     * Backlog-phase flow control: yields to the writer until the queue has room. Bounded by
     * [BACKLOG_WAIT_MS] so a wedged or degraded writer can never park the pump for ever — past the
     * bound it degrades to the normal drop, which the drop marker then records.
     */
    private fun waitForRoom(): Boolean {
        val deadline = SystemClock.elapsedRealtime() + BACKLOG_WAIT_MS
        while (queuedChars.get() >= queueCharCap) {
            if (!running.get() || SystemClock.elapsedRealtime() >= deadline) return false
            try { Thread.sleep(BACKLOG_POLL_MS) } catch (e: InterruptedException) {
                Thread.currentThread().interrupt(); return false
            }
        }
        return true
    }

    private fun drainQuietly() {
        try { drain() } catch (t: Throwable) {
            // An escaping throwable cancels all future runs of a scheduleWithFixedDelay task, which
            // would silently end the capture. Swallow, log, keep the schedule alive.
            log.e("flush task: ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    private fun sweepQuietly() {
        try {
            if (diskStopped) return
            if (files.sweep() == SweepResult.DISK_CRITICAL) {
                diskStopped = true
                enqueue(marker("DISK CRITICAL — stopping capture to protect /data"), critical = true)
                drain()
                // Only tears down the pump and the child; stop() is the caller's job. Calling
                // LogCapture.stop() from here would take stateLock from an executor thread while a
                // caller may hold it waiting on this very executor to terminate.
                running.set(false)
                procLock.withLock { child?.destroy() }
            }
        } catch (t: Throwable) {
            log.e("sweep task: ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /**
     * The single drain path. [flushLock] admits one drainer at a time so a caller's [flush] and the
     * 1 Hz task cannot split the stream into two interleaved batches on disk; [LogFiles] would keep
     * the file uncorrupted either way, but the lines would be out of order, and out-of-order is
     * exactly what makes a log capture useless.
     */
    private fun drain() = flushLock.withLock {
        if (!files.isOpen()) return@withLock
        val sb = StringBuilder(8 * 1024)
        var n = 0L
        var chars = 0L
        while (true) {
            val l = queue.poll() ?: break
            sb.append(l).append('\n')
            n++
            chars += l.length + 1
        }
        if (n > 0) {
            queuedChars.addAndGet(-chars)
            files.write(sb.toString())
            linesWritten.addAndGet(n)
        }
        val d = droppedLines.getAndSet(0)
        if (d > 0) {
            val dc = droppedChars.getAndSet(0)
            // Enqueued rather than written: it lands on the next cycle, immediately after the lines
            // that survived the burst, which is where a reader expects to find the gap noted.
            enqueue(marker("DROPPED $d lines (~$dc chars) — producer outran the writer (queue cap $queueCharCap chars)"), critical = true)
        }
    }

    private fun marker(text: String) = "##### [${isoLocal(System.currentTimeMillis())}] $text"

    // --- header -----------------------------------------------------------------------------------

    private fun header(file: File, seq: Int): String = buildString {
        val bar = "=".repeat(78)
        appendLine(bar)
        appendLine("NETPROBE log capture")
        appendLine("  session id   : $sessionId   (file #$seq)")
        appendLine("  file         : ${file.name}")
        appendLine("  session start: ${isoLocal(startedAtMs)}  /  ${isoUtc(startedAtMs)}")
        appendLine("  file opened  : ${isoLocal(System.currentTimeMillis())}")
        appendLine("  app          : ${cfg.appVersion} (applicationId ${ctx.packageName}, classes zeno.gmccpa.*)")
        appendLine("  device       : ${Build.MANUFACTURER} ${Build.MODEL} / ${Build.DEVICE} / ${Build.FINGERPRINT}")
        appendLine("  android      : ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})")
        appendLine("  scope        : $effectiveScope (requested ${cfg.scope}, READ_LOGS granted=$readLogsGranted)")
        if (effectiveScope != cfg.scope) appendLine("  degraded     : ${LogCapture.grantCmd(ctx)} then ${LogCapture.forceStopCmd(ctx)}")
        appendLine("  pid          : ${android.os.Process.myPid()}")
        appendLine("  buffers ok   : ${buffersOk.joinToString(",").ifEmpty { "(none)" }}")
        appendLine("  buffers fail : ${buffersFailed.joinToString(" ").ifEmpty { "(none)" }}")
        appendLine("  format       : ${formatArgs.joinToString(" ")}")
        appendLine("  filter       : ${if (effectiveScope == LogCapture.Scope.WHOLE_OS) cfg.filterSpec.joinToString(" ") else "--pid=${android.os.Process.myPid()} (*:V)"}")
        appendLine("  budget       : file<=${budget.maxFileBytes / (1024 * 1024)}MB total<=${budget.totalCeilingBytes / (1024 * 1024)}MB " +
            "retention=${budget.retentionMs / 86_400_000L}d free-floor=${budget.freeFloorBytes / (1024 * 1024)}MB")
        appendLine("  '#####' lines are inserted by the capture engine, not by logcat.")
        appendLine(bar)
        appendLine()
    }

    companion object {
        /**
         * Own-process: our own tag plus the Rust core's `[rust ]` lines. Low rate, so 7 days of
         * history costs little and 64 MB is generous for it.
         */
        val BUDGET_OWN = Budget(
            maxFileBytes = 4L * 1024 * 1024,
            totalCeilingBytes = 64L * 1024 * 1024,
            retentionMs = 7L * 86_400_000L,
            freeFloorBytes = 256L * 1024 * 1024,
        )

        /**
         * Whole-OS: ~27 MB/h with [LogCapture.DEFAULT_SYSTEM_FILTER] (~227 MB/h if the filter is
         * widened to `*:V`). 768 MB is therefore about a day of rolling history filtered, ~3 h
         * unfiltered — and it is a ROLLING window: the ceiling, not the retention, is what binds,
         * and the oldest files go first. Retention is set to 3 days only so a long-idle unit does
         * not keep a stale session forever.
         */
        val BUDGET_WHOLE_OS = Budget(
            maxFileBytes = 8L * 1024 * 1024,
            totalCeilingBytes = 768L * 1024 * 1024,
            retentionMs = 3L * 86_400_000L,
            freeFloorBytes = 512L * 1024 * 1024,
        )

        // Character caps, not entry caps: ~2 bytes of heap each plus String overhead, so 2 Mchar is
        // roughly 5-6 MB of heap at the very worst. At 1 Hz flush the queue only ever holds a burst.
        private const val QUEUE_CHARS_OWN = 512L * 1024
        private const val QUEUE_CHARS_WHOLE = 2L * 1024 * 1024

        private val FORMAT_YEAR_UTC = listOf("-v", "threadtime", "-v", "year", "-v", "UTC")
        private val FORMAT_PLAIN = listOf("-v", "threadtime")

        private const val FLUSH_MS = 1_000L
        private const val SWEEP_MS = 60_000L
        private const val EXEC_SHUTDOWN_MS = 2_000L
        private const val PROBE_TIMEOUT_MS = 3_000L
        private const val VERIFY_MS = 15_000L
        private const val BACKOFF_MIN_MS = 1_000L
        private const val BACKOFF_MAX_MS = 60_000L
        private const val STABLE_MS = 60_000L
        private const val READ_BUF = 64 * 1024
        private const val STDERR_KEEP = 2048
        private const val STDERR_JOIN_MS = 500L
        private const val BACKLOG_WAIT_MS = 10_000L
        private const val BACKLOG_POLL_MS = 20L
        private const val CHILD_REAP_MS = 1_000L

        private fun fmt(pattern: String, utc: Boolean) =
            SimpleDateFormat(pattern, Locale.US).apply { if (utc) timeZone = TimeZone.getTimeZone("UTC") }

        // SimpleDateFormat is not thread-safe; every use below allocates rather than sharing one.
        // These are header/marker paths (rotation and restart frequency), never per line.
        fun utcStamp(ms: Long): String = fmt("yyyyMMdd-HHmmss", true).format(Date(ms))
        fun isoUtc(ms: Long): String = fmt("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", true).format(Date(ms))
        fun isoLocal(ms: Long): String = fmt("yyyy-MM-dd'T'HH:mm:ss.SSSZ", false).format(Date(ms))
    }
}

// =================================================================================================

/**
 * Suppresses the overlap between one logcat child and the next.
 *
 * **The overlap is not small.** `logcat -d` dumps the ring and exits; a plain `logcat` dumps the
 * ENTIRE ring again and only then starts following. So every spawn after the first re-delivers
 * everything already written — potentially the whole buffer. That property is also a gift: it means
 * a crashed tail is restarted by re-reading the ring, and the gap during the outage is *recovered*
 * rather than lost, as long as the ring did not wrap. This filter is what makes that safe, and it
 * is why the tail is deliberately spawned WITHOUT `-T <timestamp>`: `-T` would shrink the re-dump,
 * but its argument is parsed in local time while the stream is emitted in UTC, and getting that
 * wrong silently skips or duplicates hours. A filter that needs no clock agreement is worth more
 * than the CPU saved.
 *
 * **Rule.** While armed, a line is dropped if its timestamp is older than the newest one already
 * emitted, or equal to it and byte-identical to a line already emitted at that timestamp. The first
 * line strictly newer than that disarms the filter permanently for that child; steady-state costs
 * one timestamp parse per line and nothing else.
 *
 * **Bounds and the races accepted.**
 * - logcat merges several buffers and is not perfectly timestamp-ordered across them. A line that
 *   arrives slightly out of order *inside the armed window* is dropped. Outside it — i.e. all of
 *   steady state — nothing is ever dropped. Confining the risk to the seam is the entire reason the
 *   filter disarms on first crossing rather than staying on.
 * - A backwards clock correction (a head unit sets its clock after boot) would leave the filter
 *   armed against a timestamp from the future and drop everything for ever. Hence the hard budget:
 *   after [MAX_LINES] or [MAX_MS] the filter gives up, emits a marker, and disarms. Worst case is
 *   ~30 s of duplicate-or-suppressed lines around a clock jump, which is visible in the file.
 * - [DUP_MAX] identical-timestamp lines are remembered. If more than that many entries share one
 *   millisecond, the excess may be written twice. Duplication is the benign direction.
 *
 * Called only from the pump thread; needs no synchronisation, and has none.
 */
private class SeamFilter(private val hasYear: () -> Boolean, private val notice: (String) -> Unit) {

    private var armed = false
    private var lastTs = NO_TS
    private val dup = HashSet<String>()
    private var armedLines = 0
    private var armedAtMs = 0L

    fun arm() {
        if (lastTs == NO_TS) return
        armed = true
        armedLines = 0
        armedAtMs = SystemClock.elapsedRealtime()
    }

    /** True if the line should be written. Updates the high-water timestamp for emitted lines. */
    fun accept(line: String): Boolean {
        val ts = parse(line)
        if (armed) {
            armedLines++
            if (armedLines > MAX_LINES || SystemClock.elapsedRealtime() - armedAtMs > MAX_MS) {
                armed = false
                notice("SEAM RESYNC GAVE UP after $armedLines lines — the stream never passed the last written timestamp (clock moved backwards?). Duplicate or missing lines may follow.")
            } else {
                // Untimestamped lines ("--------- beginning of main") are re-dump artefacts inside
                // the window; outside it they are useful buffer markers and are kept.
                if (ts == NO_TS) return false
                if (ts < lastTs) return false
                if (ts == lastTs) {
                    // Bounded exactly as the KDoc claims: past DUP_MAX entries in a single
                    // millisecond the set stops growing and the excess is emitted. Duplicating a
                    // handful of lines is the benign direction; an unbounded HashSet on a
                    // multi-day capture is not.
                    if (dup.size >= DUP_MAX) return true
                    return dup.add(line)
                }
                armed = false
            }
        }
        if (ts != NO_TS) {
            if (ts > lastTs) { lastTs = ts; dup.clear(); dup.add(line) } else if (ts == lastTs && dup.size < DUP_MAX) dup.add(line)
        }
        return true
    }

    /**
     * Packs `[YYYY-]MM-DD HH:MM:SS.mmm` into a monotone decimal (`yyyyMMddHHmmssSSS` or
     * `MMddHHmmssSSS`); only ordering matters, never the value. Hand-parsed because this runs on
     * every captured line and a `SimpleDateFormat` there would dominate the capture's CPU cost.
     * Any line whose prefix is not exactly digits and `- : .` returns [NO_TS].
     */
    private fun parse(line: String): Long {
        val end = if (hasYear()) 23 else 18
        if (line.length < end) return NO_TS
        var v = 0L
        for (i in 0 until end) {
            val c = line[i]
            when {
                c in '0'..'9' -> v = v * 10 + (c - '0')
                c == '-' || c == ' ' || c == ':' || c == '.' -> {}
                else -> return NO_TS
            }
        }
        return v
    }

    companion object {
        private const val NO_TS = -1L
        private const val DUP_MAX = 256
        private const val MAX_LINES = 500_000
        private const val MAX_MS = 30_000L
    }
}
