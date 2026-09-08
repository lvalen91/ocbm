package zeno.gmccpa.logging

import zeno.gmccpa.ProbeLog
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.util.Locale
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * How much disk one capture scope is allowed to consume.
 *
 * Two numbers matter and they are NOT the same number. [maxFileBytes] bounds one *file* — it exists
 * so an export over SAF/USB moves in digestible chunks and so a truncated tail costs one chunk, not
 * the session. [totalCeilingBytes] bounds the *directory* — it is the only thing standing between a
 * multi-day unattended capture and a full `/data` on the head unit. The reference logger
 * (`carlink_native/.../FileLogManager.kt`) has only the first and gets away with it because it
 * captures one tag; whole-OS logcat on this unit measures ~63 KB/s (923 421 lines / 147 MB over a
 * 39-minute leg in `evidence/drive_20260818-132220`), i.e. ~227 MB/h unfiltered. Without a directory
 * ceiling that fills any partition in a day.
 *
 * [freeFloorBytes] is the second half of the storage guarantee: a static ceiling is not safe on a
 * partition that some *other* subsystem has already nearly filled, so [LogFiles.sweep] additionally
 * refuses to occupy space that would take free space below this floor, and hard-stops the capture
 * if even the minimum working set will not fit. Better to lose the capture than to brick the unit's
 * `/data`.
 *
 * [retentionMs] is hygiene, not safety. It is enforced from mtime, which a head unit's post-boot
 * clock correction can move backwards; when that happens files look younger or older than they are
 * and retention over- or under-deletes. The byte ceiling is unaffected by any clock and is therefore
 * the property to rely on.
 */
internal data class Budget(
    val maxFileBytes: Long,
    val totalCeilingBytes: Long,
    val retentionMs: Long,
    val freeFloorBytes: Long,
) {
    init {
        // Invariant relied on by pruneLocked(): the current file is never deleted, so the ceiling
        // must leave room for at least one full file plus one being filled, or the sweep would spin
        // trying (and failing) to get under the ceiling.
        require(totalCeilingBytes >= 2 * maxFileBytes) { "ceiling must hold >=2 files" }
    }
}

/** Outcome of a [LogFiles.sweep]; [DISK_CRITICAL] means the caller must stop capturing. */
internal enum class SweepResult { OK, DISK_CRITICAL }

/**
 * The rotating, byte-budgeted file sink. Everything about *where bytes land* lives here; nothing
 * about logcat, subprocesses or threads does.
 *
 * **Threading.** Every public method takes [lock] and does its work under it. The lock protects the
 * whole of the mutable set — [out], [current], [currentBytes], [seq], [closed], [degradedSinceMs] —
 * and nothing outside this class touches those. Callers may be any thread; in practice there are
 * three (the 1 Hz flush executor, an arbitrary caller of `LogCapture.flush()`, and the shutdown
 * thread) and the lock is held only across the actual `write(2)`/`unlink(2)`, never across anything
 * that blocks on another thread. This class never calls back into [LogCapture], so it cannot
 * participate in a lock cycle.
 *
 * **Rotation is pre-write, not post-write.** The reference logger checks size *after* writing and
 * therefore overshoots by up to a full batch; here the batch is encoded first and rotation happens
 * if it would not fit. Files exceed [Budget.maxFileBytes] only when a single batch alone exceeds it,
 * which the producer-side backpressure cap already makes bounded. Costs nothing, so there was no
 * reason to inherit the overshoot.
 *
 * **Durability.** Batches are written with no fsync, then fsync'd at most every [SYNC_INTERVAL_MS].
 * A process kill loses nothing (the page cache survives it); an abrupt vehicle power cut loses at
 * most the last ~10 s. Per-batch fsync at 1 Hz would close that window but writes 86 400 barriers a
 * day to eMMC for a marginal gain — the fault being chased is minutes long, not seconds.
 *
 * **Write failures do not end the session.** ENOSPC or a revoked fd closes the writer and enters a
 * degraded state that retries the open every [REOPEN_BACKOFF_MS]. The reference degrades to a
 * permanent silent drop-all recoverable only by a disable/enable cycle; over a multi-day unattended
 * run that turns one transient full disk into a lost capture.
 */
internal class LogFiles(
    private val dir: File,
    private val budget: Budget,
    private val namePrefix: String,
    /** The *other* scope's name prefix and retention. [sweep] ages that scope's leftover files out
     *  under THEIR budget — an idle scope must still expire — but never applies this session's byte
     *  ceiling to them; the two ceilings differ 12x. */
    private val otherNamePrefix: String,
    private val otherRetentionMs: Long,
    private val sessionId: String,
    private val header: (file: File, seq: Int) -> String,
) {
    private val log = ProbeLog.sub("logcap")
    private val lock = ReentrantLock()

    private var out: FileOutputStream? = null
    private var current: File? = null
    private var currentBytes = 0L
    private var seq = 0
    private var closed = false
    private var degradedSinceMs = 0L
    private var lastSyncMs = 0L

    /** True once [open] has succeeded and before [close]; writes before that are silently dropped. */
    fun isOpen(): Boolean = lock.withLock { out != null }

    fun currentFileName(): String? = lock.withLock { current?.name }

    /**
     * Creates the log directory and the first file. Returns false if the directory or the file
     * cannot be created, in which case the caller should abandon the session rather than run a
     * capture whose output goes nowhere.
     */
    fun open(): Boolean = lock.withLock {
        if (!dir.exists() && !dir.mkdirs()) {
            log.e("cannot create ${dir.absolutePath} — no capture")
            return@withLock false
        }
        rotateLocked(initial = true)
        out != null
    }

    /**
     * Appends [text] (already newline-terminated by the caller), rotating first if it will not fit.
     * Never throws: a failed write degrades the sink instead of propagating into the flush executor,
     * because an exception escaping a `scheduleAtFixedRate` task cancels all future runs of it.
     */
    fun write(text: String) = lock.withLock {
        if (closed) return@withLock
        val bytes = text.toByteArray(Charsets.UTF_8)
        if (out == null) {
            // Degraded: retry the open on a slow timer, but throw this batch away — buffering it
            // would reintroduce the unbounded growth the queue cap exists to prevent.
            val now = System.currentTimeMillis()
            if (now - degradedSinceMs < REOPEN_BACKOFF_MS) return@withLock
            rotateLocked(initial = true)
            if (out == null) return@withLock
        }
        if (currentBytes > 0 && currentBytes + bytes.size > budget.maxFileBytes) rotateLocked(initial = false)
        val fos = out ?: return@withLock
        try {
            fos.write(bytes)
            currentBytes += bytes.size
            val now = android.os.SystemClock.elapsedRealtime()
            if (now - lastSyncMs >= SYNC_INTERVAL_MS) {
                fos.fd.sync()
                lastSyncMs = now
            }
        } catch (e: IOException) {
            log.e("write failed (${e.message}) — degrading, retry in ${REOPEN_BACKOFF_MS / 1000}s")
            closeStreamLocked()
            degradedSinceMs = System.currentTimeMillis()
        }
    }

    /**
     * Enforces retention, the directory ceiling and the free-space floor. Retention covers EVERY
     * `netprobe-*.log` in the directory including the other scope's — old sessions are exactly what
     * would otherwise accumulate — but each scope is aged under its own [Budget.retentionMs]. The
     * byte ceiling is per-scope: the name carries the scope (`netprobe-own-` / `netprobe-os-`) and
     * applying a 64 MB own-process ceiling to whole-OS files would prune evidence this session's
     * budget never accounted for. Called on a timer rather than only at start, which is the caveat
     * the reference logger documents and does not fix; a capture meant to run for days must re-apply
     * retention while it runs.
     */
    fun sweep(): SweepResult = lock.withLock {
        if (closed) return@withLock SweepResult.OK
        val cur = current
        val mine = "$FILE_PREFIX$namePrefix-"

        val now = System.currentTimeMillis()
        expireLocked(mine, now - budget.retentionMs, cur)
        expireLocked("$FILE_PREFIX$otherNamePrefix-", now - otherRetentionMs, cur)

        var total = listLocked(mine).sumOf { it.length() }
        // usableSpace is a property of the PARTITION, not of a scope, so it stays a whole-directory
        // read even though `total` above is this scope's alone. It reports 0 when the fs cannot
        // answer; treat that as "no information" and fall back to the static ceiling rather than
        // hard-stopping a healthy capture.
        val usable = dir.usableSpace
        var ceiling = budget.totalCeilingBytes
        if (usable > 0) {
            val headroom = total + usable - budget.freeFloorBytes
            if (headroom < ceiling) ceiling = headroom
        }
        if (ceiling < 2 * budget.maxFileBytes) {
            log.e("disk critical: usable=${mb(usable)}MB, cannot keep ${mb(2 * budget.maxFileBytes)}MB — stopping capture")
            return@withLock SweepResult.DISK_CRITICAL
        }
        if (total > ceiling) {
            // Oldest first by NAME: the name carries a fixed-width UTC session id and a zero-padded
            // sequence, so lexicographic order is chronological order and — unlike mtime — a
            // post-boot clock correction cannot reorder it.
            for (f in listLocked(mine).sortedBy { it.name }) {
                if (total <= ceiling) break
                if (f == cur) continue
                val n = f.length()
                if (f.delete()) {
                    total -= n
                    log.i("ceiling: dropped ${f.name} (${mb(n)}MB), total now ${mb(total)}MB")
                }
            }
        }
        SweepResult.OK
    }

    fun files(): List<File> = lock.withLock { listLocked().sortedBy { it.name } }

    fun totalBytes(): Long = lock.withLock { listLocked().sumOf { it.length() } }

    fun fileCount(): Int = lock.withLock { listLocked().size }

    /** Idempotent. After this, [write] is a no-op forever — a session is never reopened. */
    fun close() = lock.withLock {
        if (closed) return@withLock
        closed = true
        closeStreamLocked()
    }

    // --- lock held below -----------------------------------------------------------------------

    /** [prefix] defaults to every scope's files; [sweep] narrows it to one scope's. */
    private fun listLocked(prefix: String = FILE_PREFIX): List<File> =
        dir.listFiles { f -> f.isFile && f.name.startsWith(prefix) && f.name.endsWith(".log") }
            ?.toList() ?: emptyList()

    private fun expireLocked(prefix: String, cutoffMs: Long, cur: File?) {
        for (f in listLocked(prefix)) {
            if (f == cur) continue
            if (f.lastModified() < cutoffMs && f.delete()) log.i("retention: dropped ${f.name}")
        }
    }

    private fun rotateLocked(initial: Boolean) {
        closeStreamLocked()
        val n = seq++
        val f = File(dir, "$FILE_PREFIX$namePrefix-$sessionId-${String.format(Locale.US, "%03d", n)}.log")
        try {
            // append=true: a same-second restart that produced the same session id appends rather
            // than truncating someone else's evidence.
            val fos = FileOutputStream(f, true)
            out = fos
            current = f
            currentBytes = f.length()
            degradedSinceMs = 0L
            lastSyncMs = 0L
            val head = header(f, n).toByteArray(Charsets.UTF_8)
            fos.write(head)
            currentBytes += head.size
            if (!initial) log.i("rotated -> ${f.name}")
        } catch (e: IOException) {
            log.e("cannot open ${f.name}: ${e.message}")
            closeStreamLocked()
            current = null
            degradedSinceMs = System.currentTimeMillis()
        }
    }

    private fun closeStreamLocked() {
        try {
            out?.fd?.sync()
            out?.close()
        } catch (e: IOException) {
            log.w("close: ${e.message}")
        }
        out = null
        currentBytes = 0L
    }

    private fun mb(b: Long) = b / (1024 * 1024)

    companion object {
        /** Shared by both scopes so a sweep sees, and can expire, the other scope's old sessions;
         *  the scope itself is the next name segment (`own` / `os`). */
        const val FILE_PREFIX = "netprobe-"
        private const val SYNC_INTERVAL_MS = 10_000L
        private const val REOPEN_BACKOFF_MS = 30_000L
    }
}
