package zeno.gmccpa

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.file.FileAlreadyExistsException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import java.security.SecureRandom

/**
 * The per-install Ed25519 accessory seed (plan v5.8 V5, finding Q001; hardening T6.5).
 *
 * Before V5 every install signed pair-verify with the same seed, derived from a 32-byte pattern in
 * source — the accessory's long-term key was public. Now each install draws 32 random bytes ONCE
 * and keeps them in `noBackupFilesDir/`[SEED_FILE] (app-private, excluded from backup; `allowBackup`
 * is already false). `CarPlayRx` has no default seed any more: this is the only source.
 *
 * Pure: java.io / java.nio / java.security only, so it runs on the host JVM (tools/jvm_tests.txt).
 * Logging goes through [Log]; NO log call ever takes the seed, and nothing derived from it (not even a
 * fingerprint) is logged — the only lines are `created` / `loaded` / the corrupt-file E.
 *
 * Creation is race-safe: one lock covers the whole of [loadOrCreate], and underneath it the file is
 * claimed atomically (`O_EXCL`, see [install]) so a loser adopts the winner's bytes rather than
 * overwriting them. The interleaving this prevents: two starts race on a fresh install, each
 * generates a seed, the second write overwrites the first — but the first `CarPlayRx` already fixed
 * the native `OnceLock` to seed A and the phone paired under A; the next process loads B and the
 * pairing is silently broken. Every caller returns the FILE's bytes, never its own candidate.
 *
 * Migration (OD-8B, owner 2026-10-07): a first load that finds no seed file but an existing peer
 * store is an upgraded install whose pairings are now invalid. [Rekey.PENDING] is persisted next to
 * the seed; SessionCore runs Forget Pairing automatically ONCE on the next OCBM link
 * ([Rekey.FORGOTTEN]) and shows the "forget this car on the iPhone" hint until the next successful
 * pair-verify clears it ([clearRekey]). A corrupt, short, unreadable or legacy-pattern seed file is
 * moved aside (kept, `.corrupt-<time>`), a new seed is created and the same flag is set.
 */
object AccessoryKey {
    const val SEED_FILE = "carplay_accessory_ed25519.seed"
    /** Persisted rekey state, one word: `pending` or `forgotten`; absent = [Rekey.NONE]. */
    const val REKEY_FILE = "carplay_accessory_rekey.state"
    const val SEED_LEN = 32

    /**
     * SHA-256 of the pre-V5 compiled-in seed (the `(i*31+7)` pattern). Only the digest is kept so no
     * code path can produce the legacy bytes: a seed file holding them is treated as corrupt.
     */
    const val LEGACY_SEED_SHA256 = "8e889f10b21cdd1b3ad72f740317a827d76e1b5b3f721e33c566f06d1deff8ea"

    /** Injected so the file stays Android-free; SessionCore passes a ProbeLog logger. */
    interface Log { fun i(msg: String); fun e(msg: String) }

    enum class Rekey { NONE, PENDING, FORGOTTEN }

    class Result(
        /** Exactly [SEED_LEN] bytes — the FILE's contents, never a caller's candidate. */
        val seed: ByteArray,
        /** This call wrote a new seed (fresh install, or a corrupt file replaced). */
        val created: Boolean,
        /** The persisted rekey state after this call. */
        val rekey: Rekey
    )

    private val lock = Any()
    private val rng = SecureRandom()

    /**
     * Load the install's seed, creating it on first use. [peersPresent] = `carplay_peers.bin` exists
     * (the caller knows the peer store's directory; it is not this one). [now] feeds the
     * `.corrupt-<time>` suffix only.
     */
    fun loadOrCreate(dir: File, peersPresent: Boolean, log: Log, now: () -> Long = System::currentTimeMillis): Result =
        synchronized(lock) {
            dir.mkdirs()
            val f = File(dir, SEED_FILE)
            if (f.exists()) {
                val bytes = try { f.readBytes() } catch (_: IOException) { null }
                val why = validate(bytes)
                if (why == null) {
                    log.i("accessory key loaded")
                    return Result(bytes!!, created = false, rekey = rekeyState(dir))
                }
                // Move aside (kept, not deleted); delete only if the move fails; and say which happened
                // (review fix1-2: this E line is the only evidence of what became of the file).
                val aside = File(dir, "$SEED_FILE.corrupt-${now()}")
                val moved = try { Files.move(f.toPath(), aside.toPath()); true } catch (_: IOException) { false }
                val deleted = !moved && f.delete()
                val fate = when {
                    moved -> "moved aside as ${aside.name}"
                    deleted -> "move aside FAILED, deleted instead"
                    else -> "move aside FAILED and delete FAILED"
                }
                // A pairing could only have been made under the bytes that were there: a 0-byte file
                // (a crash between claim and publish) with no peer store has nothing to rekey.
                val hadKeyMaterial = peersPresent || (bytes != null && bytes.isNotEmpty())
                log.e("accessory key file is $why — $fate; creating a new key" +
                      (if (hadKeyMaterial) ", existing pairings are invalid" else ""))
                // Never adopt what is still there: install() would read the existing (legacy) bytes
                // back as "the winner" (review fix1-4). No fallback seed exists; fail loudly.
                check(!f.exists()) { "accessory key file is $why and could not be moved or deleted" }
                if (hadKeyMaterial) setRekey(dir, Rekey.PENDING)
                val seed = install(f, fresh())
                return Result(seed, created = true, rekey = rekeyState(dir))
            }
            // Upgrade = a peer store exists before any seed: the flag goes down BEFORE the seed is
            // published, so a process death between the two cannot leave a valid seed with no flag
            // (review fix1-1: that install would never auto-forget or hint, and the old pairing would
            // fail silently). A concurrent creator saw the same peer store, so the flag stands.
            if (peersPresent) setRekey(dir, Rekey.PENDING)
            val candidate = fresh()
            val seed = install(f, candidate)
            val created = seed.contentEquals(candidate)
            log.i(if (created) "accessory key created" else "accessory key loaded (created concurrently)")
            Result(seed, created, rekeyState(dir))
        }

    /** The persisted migration state (see the class doc). */
    fun rekeyState(dir: File): Rekey {
        val f = File(dir, REKEY_FILE)
        if (!f.exists()) return Rekey.NONE
        return when (try { f.readText().trim() } catch (_: IOException) { "" }) {
            "pending" -> Rekey.PENDING
            "forgotten" -> Rekey.FORGOTTEN
            else -> Rekey.NONE
        }
    }

    /**
     * PENDING -> FORGOTTEN (the one automatic Forget Pairing ran, or the driver pressed it). Returns
     * whether it was PENDING. [force] writes FORGOTTEN from any state: the caller saw PENDING when its
     * forget began, and a pair-verify that cleared the flag in the meantime was on the receiver that
     * forget just stopped, so its pairing went with the peer store and the hint must stay up.
     */
    fun markForgotten(dir: File, force: Boolean = false): Boolean = synchronized(lock) {
        val was = rekeyState(dir)
        if (was != Rekey.PENDING && !force) return false
        setRekey(dir, Rekey.FORGOTTEN)
        was == Rekey.PENDING
    }

    /** A pair-verify succeeded under the current key: the migration is over. Idempotent. */
    fun clearRekey(dir: File): Boolean = synchronized(lock) {
        val f = File(dir, REKEY_FILE)
        f.exists() && f.delete()
    }

    /** null = usable; otherwise the reason it is not (never the bytes). */
    private fun validate(bytes: ByteArray?): String? = when {
        bytes == null -> "unreadable"
        bytes.size != SEED_LEN -> "${bytes.size} bytes, not $SEED_LEN"
        isLegacy(bytes) -> "the pre-V5 compiled-in pattern"
        else -> null
    }

    private fun isLegacy(bytes: ByteArray): Boolean {
        val d = MessageDigest.getInstance("SHA-256").digest(bytes)
        return d.joinToString("") { "%02x".format(it) } == LEGACY_SEED_SHA256
    }

    private fun fresh(): ByteArray {
        while (true) {
            val b = ByteArray(SEED_LEN).also { rng.nextBytes(it) }
            if (!isLegacy(b)) return b    // 2^-256, but the invariant is cheap to state
        }
    }

    /**
     * Claim [target] with [candidate]. Returns the bytes now in [target] — the candidate if this call
     * won, the winner's otherwise. `internal` so the race test can drive it from several threads
     * WITHOUT the outer lock, which is what proved the plan's original mechanism wrong:
     * `Files.move` without `REPLACE_EXISTING` is exists-check-then-`rename(2)` in the JDK, so two
     * racing creators both saw no target, both renamed, the second overwrote the first, and the first
     * returned its own candidate — the exact interleaving V5 exists to exclude (AccessoryKeyTest
     * `install_withoutTheLock…`, first run). So the claim is `O_EXCL` (`Files.createFile`, atomic on
     * every POSIX filesystem), and the content is published by an atomic replace of that empty claim
     * with the fsynced temp, so a reader ever sees 0 or 32 bytes, never a partial write. A loser
     * waits (bounded) for the winner's 32 bytes. A crash between claim and publish leaves a 0-byte
     * file, which the next load treats as corrupt (moved aside, new seed, rekey flag).
     */
    internal fun install(target: File, candidate: ByteArray): ByteArray {
        require(candidate.size == SEED_LEN)
        val claimed = try { Files.createFile(target.toPath()); true } catch (_: FileAlreadyExistsException) { false }
        if (!claimed) return awaitWinner(target)
        val tmp = File.createTempFile("${SEED_FILE}.", ".tmp", target.parentFile)
        try {
            FileOutputStream(tmp).use { out ->
                out.write(candidate)
                out.flush()
                out.fd.sync()
            }
            Files.move(tmp.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
            fsyncDir(target.parentFile)
            return candidate
        } finally {
            tmp.delete()
        }
    }

    /**
     * Best-effort fsync of the directory entry after the publish (review fix1-5): the data was
     * synced in the temp, but a power loss inside the FS commit interval could still drop the entry
     * (a missing/0-byte seed = forced re-pair on next boot). Not every JVM/FS allows opening a
     * directory for sync; a failure here is ignored, not fatal.
     */
    private fun fsyncDir(dir: File?) {
        if (dir == null) return
        try {
            java.nio.channels.FileChannel.open(dir.toPath(), java.nio.file.StandardOpenOption.READ).use { it.force(true) }
        } catch (_: Exception) { }
    }

    /** Lost the claim: adopt the winner's bytes once they are published (0 bytes = not yet). */
    private fun awaitWinner(target: File): ByteArray {
        val deadline = System.nanoTime() + ADOPT_WAIT_MS * 1_000_000L
        while (true) {
            val b = try { target.readBytes() } catch (_: IOException) { ByteArray(0) }
            if (b.size == SEED_LEN) return b
            check(b.isEmpty()) { "seed file written by a concurrent creator is ${b.size} bytes" }
            check(System.nanoTime() < deadline) { "seed file claimed by a concurrent creator but never written" }
            Thread.sleep(5)
        }
    }

    private const val ADOPT_WAIT_MS = 2_000L

    private fun setRekey(dir: File, state: Rekey) {
        val f = File(dir, REKEY_FILE)
        if (state == Rekey.NONE) { f.delete(); return }
        val tmp = File(dir, "$REKEY_FILE.tmp")
        // Same discipline as the seed ([install], A-X2 2026-10-08): the bytes are fsynced in the temp
        // before the atomic replace, then the directory entry. A power loss between the write and
        // the FS commit could otherwise publish an empty flag file, which reads as NONE — and a
        // PENDING lost that way skips the one automatic Forget Pairing the flag exists for.
        FileOutputStream(tmp).use { out ->
            out.write((state.name.lowercase() + "\n").toByteArray(Charsets.UTF_8))
            out.flush()
            out.fd.sync()
        }
        Files.move(tmp.toPath(), f.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        fsyncDir(dir)
    }
}
