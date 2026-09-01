package com.carlink.device

import android.content.Context
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * Durable home for the known-device history and the user's preferred phone.
 *
 * **Why SharedPreferences and not DataStore or Room.** DataStore was deliberately removed from this
 * project — `app/build.gradle.kts` records it as "write-only dead I/O" — and its Preferences flavour
 * has no list type, so it would end up storing this same JSON string through more machinery. Room
 * brings a schema, DAOs and ksp for at most a handful of records that are never queried. The data is
 * a few hundred bytes written a few times per session.
 *
 * **Why `commit()` and not `apply()`.** A head-unit power cut IS the normal shutdown here. `apply()`
 * queues an asynchronous write that flushes on clean lifecycle transitions, and a hard cut can drop
 * it; `commit()` writes synchronously, and SharedPreferences' temp-file-plus-rename with a `.bak`
 * means a kill mid-write yields the previous good file rather than a corrupt one. It is called on
 * this store's own thread, never a caller's, so the synchronous cost is invisible.
 *
 * **Why a private executor rather than the manager's scope.** `CarlinkManager.release()` cancels its
 * scope *before* teardown, so a save launched there in the final moments would die silently — the
 * same swallowed-post hazard the wake-lock teardown comment documents. Writing through on an
 * independent thread also means there is no "flush on shutdown" step that can be missed.
 *
 * Reads never touch disk: [snapshot] is an immutable value swapped under a lock and read through a
 * `@Volatile`, so the UI's 1 Hz poll and the connect path both see it for free.
 */
class KnownDeviceStore(
    context: Context,
    private val prefsName: String = PREFS_NAME,
) {
    private val prefs = context.applicationContext.getSharedPreferences(prefsName, Context.MODE_PRIVATE)
    private val io = Executors.newSingleThreadExecutor { r -> Thread(r, "known-devices").apply { isDaemon = true } }
    private val lock = Any()

    /** Current state. Immutable; replaced wholesale on every mutation. */
    @Volatile
    var snapshot: KnownDeviceSnapshot = KnownDeviceSnapshot()
        private set

    /** True once [load] has run, so callers can tell "empty" from "not read yet". */
    @Volatile
    var loaded: Boolean = false
        private set

    /**
     * Read the store. Blocking — call from a background thread.
     *
     * Idempotent and safe to race a concurrent [update]: the update wins, because it holds the lock
     * and load only installs its value if nothing has been written yet.
     */
    fun load(): KnownDeviceSnapshot {
        val decoded = KnownDeviceCodec.decode(prefs.getString(KEY, null))
        synchronized(lock) {
            if (!loaded) {
                snapshot = decoded
                loaded = true
            }
            return snapshot
        }
    }

    /**
     * Apply [mutation] to the snapshot and persist the result.
     *
     * The in-memory swap is synchronous so a caller can read its own write immediately; only the
     * disk write is deferred. Returns the new snapshot. A mutation that changes nothing does not
     * write — the encoded form is deterministic, which is what makes that comparison meaningful.
     */
    fun update(mutation: (KnownDeviceSnapshot) -> KnownDeviceSnapshot): KnownDeviceSnapshot {
        val next: KnownDeviceSnapshot
        synchronized(lock) {
            val prev = snapshot
            next = mutation(prev)
            if (next == prev) return prev
            snapshot = next
            loaded = true
        }
        val json = KnownDeviceCodec.encode(next)
        runCatching { io.execute { prefs.edit().putString(KEY, json).commit() } }
        return next
    }

    /** Drop everything, including the file. For a "clear device history" action. */
    fun clear() {
        synchronized(lock) {
            snapshot = KnownDeviceSnapshot()
            loaded = true
        }
        runCatching { io.execute { prefs.edit().remove(KEY).commit() } }
    }

    /**
     * Stop accepting writes and give queued ones a bounded chance to land.
     *
     * Best-effort by design: every mutation is already written through, so at worst the last delta
     * is lost — never the whole history.
     */
    fun close() {
        io.shutdown()
        runCatching { io.awaitTermination(1, TimeUnit.SECONDS) }
    }

    private companion object {
        const val PREFS_NAME = "known_devices"

        /**
         * One key holding the whole document.
         *
         * Not a key per field: the snapshot has to be read and written atomically, and
         * SharedPreferences only guarantees that per `commit()`, not across keys.
         */
        const val KEY = "snapshot_v1"
    }
}
