package zeno.gmccpa

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.security.MessageDigest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** Plan v5.8 V5 (Q001): the per-install Ed25519 seed. Host JVM, no Android. */
class AccessoryKeyTest {
    private class RecLog : AccessoryKey.Log {
        val lines = java.util.Collections.synchronizedList(mutableListOf<String>())
        override fun i(msg: String) { lines.add("I $msg") }
        override fun e(msg: String) { lines.add("E $msg") }
    }

    private fun tmpDir(): File = Files.createTempDirectory("akey").toFile()
    /** The pre-V5 compiled-in seed, built here (test only) to prove it is never produced or accepted. */
    private fun legacy(): ByteArray = ByteArray(32) { i -> (i * 31 + 7).toByte() }
    private fun sha256(b: ByteArray) =
        MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }

    @Test fun legacyDigestConstant_matchesTheOldPattern() = assertEquals(AccessoryKey.LEGACY_SEED_SHA256, sha256(legacy()))

    @Test fun freshDir_creates32Bytes_stableOnReload_noRekey() {
        val d = tmpDir(); val log = RecLog()
        val a = AccessoryKey.loadOrCreate(d, peersPresent = false, log = log)
        assertEquals(32, a.seed.size)
        assertTrue(a.created)
        assertEquals(AccessoryKey.Rekey.NONE, a.rekey)
        assertArrayEquals(a.seed, File(d, AccessoryKey.SEED_FILE).readBytes())
        val b = AccessoryKey.loadOrCreate(d, peersPresent = false, log = log)
        assertFalse(b.created)
        assertArrayEquals(a.seed, b.seed)
        assertEquals(AccessoryKey.Rekey.NONE, b.rekey)
        assertFalse(File(d, AccessoryKey.REKEY_FILE).exists())
        assertEquals(listOf("I accessory key created", "I accessory key loaded"), log.lines)
        // nothing but the seed file is left behind (the temp was moved, not copied)
        assertEquals(listOf(AccessoryKey.SEED_FILE), d.list()!!.toList())
    }

    @Test fun generatedSeed_neverEqualsLegacyPattern_andIsNotAllZero() {
        repeat(20) {
            val d = tmpDir()
            val s = AccessoryKey.loadOrCreate(d, peersPresent = false, log = RecLog()).seed
            assertFalse(s.contentEquals(legacy()))
            assertFalse(s.all { it == 0.toByte() })
        }
    }

    @Test fun eightThreadsRaceOnFreshDir_allReturnTheFileBytes() {
        val d = tmpDir(); val log = RecLog()
        val n = 8
        val barrier = CyclicBarrier(n)
        val pool = Executors.newFixedThreadPool(n)
        val results = (0 until n).map { pool.submit<ByteArray> { barrier.await(); AccessoryKey.loadOrCreate(d, false, log).seed } }
        val seeds = results.map { it.get(10, TimeUnit.SECONDS) }
        pool.shutdownNow()
        val file = File(d, AccessoryKey.SEED_FILE).readBytes()
        assertEquals(32, file.size)
        seeds.forEach { assertArrayEquals(file, it) }
        assertEquals(1, log.lines.count { it == "I accessory key created" })
        assertEquals(n - 1, log.lines.count { it == "I accessory key loaded" })
    }

    /** The file-level guard on its own (no process lock): 8 distinct candidates, one winner, 7 adopters. */
    @Test fun install_withoutTheLock_firstWriterWins_losersAdoptTheFile() {
        val d = tmpDir()
        val target = File(d, AccessoryKey.SEED_FILE)
        val n = 8
        val candidates = (0 until n).map { k -> ByteArray(32) { (k + 1).toByte() } }
        val barrier = CyclicBarrier(n)
        val done = CountDownLatch(n)
        val out = arrayOfNulls<ByteArray>(n)
        val threads = (0 until n).map { k ->
            Thread {
                barrier.await()
                out[k] = AccessoryKey.install(target, candidates[k])
                done.countDown()
            }.apply { start() }
        }
        assertTrue(done.await(10, TimeUnit.SECONDS))
        threads.forEach { it.join() }
        val file = target.readBytes()
        assertTrue(candidates.any { it.contentEquals(file) })
        out.forEach { assertArrayEquals(file, it) }
        assertEquals(listOf(AccessoryKey.SEED_FILE), d.list()!!.toList())   // no temp files left
    }

    @Test fun corruptFile31Bytes_movedAside_newSeed_rekeyPending_eLogged() {
        val d = tmpDir(); val log = RecLog()
        File(d, AccessoryKey.SEED_FILE).writeBytes(ByteArray(31) { 0x5a })
        val r = AccessoryKey.loadOrCreate(d, peersPresent = false, log = log, now = { 1234L })
        assertTrue(r.created)
        assertEquals(32, r.seed.size)
        assertEquals(AccessoryKey.Rekey.PENDING, r.rekey)
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.rekeyState(d))
        val aside = File(d, "${AccessoryKey.SEED_FILE}.corrupt-1234")
        assertTrue("kept, not deleted", aside.exists())
        assertEquals(31, aside.length())
        assertArrayEquals(r.seed, File(d, AccessoryKey.SEED_FILE).readBytes())
        assertEquals(1, log.lines.count { it.startsWith("E accessory key file is 31 bytes, not 32") })
    }

    @Test fun legacyPatternFile_isCorrupt_rekeyPending() {
        val d = tmpDir(); val log = RecLog()
        File(d, AccessoryKey.SEED_FILE).writeBytes(legacy())
        val r = AccessoryKey.loadOrCreate(d, peersPresent = false, log = log, now = { 7L })
        assertTrue(r.created)
        assertFalse(r.seed.contentEquals(legacy()))
        assertEquals(AccessoryKey.Rekey.PENDING, r.rekey)
        assertTrue(File(d, "${AccessoryKey.SEED_FILE}.corrupt-7").exists())
        assertEquals(1, log.lines.count { it.startsWith("E accessory key file is the pre-V5 compiled-in pattern") })
    }

    @Test fun migration_peersPresentAndNoSeed_pending_freshInstallNot() {
        val up = tmpDir()
        val r = AccessoryKey.loadOrCreate(up, peersPresent = true, log = RecLog())
        assertTrue(r.created)
        assertEquals(AccessoryKey.Rekey.PENDING, r.rekey)
        assertEquals("pending", File(up, AccessoryKey.REKEY_FILE).readText().trim())
        // persisted: a later load (same key, peers still there or not) still reports it
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.loadOrCreate(up, peersPresent = false, log = RecLog()).rekey)

        val fresh = tmpDir()
        assertEquals(AccessoryKey.Rekey.NONE, AccessoryKey.loadOrCreate(fresh, peersPresent = false, log = RecLog()).rekey)
        assertFalse(File(fresh, AccessoryKey.REKEY_FILE).exists())
    }

    @Test fun rekeyStateMachine_pendingForgottenCleared() {
        val d = tmpDir()
        assertEquals(AccessoryKey.Rekey.NONE, AccessoryKey.rekeyState(d))
        assertFalse("nothing to mark", AccessoryKey.markForgotten(d))
        AccessoryKey.loadOrCreate(d, peersPresent = true, log = RecLog())
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.rekeyState(d))
        assertTrue(AccessoryKey.markForgotten(d))
        assertEquals(AccessoryKey.Rekey.FORGOTTEN, AccessoryKey.rekeyState(d))
        assertFalse("FORGOTTEN -> FORGOTTEN is not a transition", AccessoryKey.markForgotten(d))
        assertEquals(AccessoryKey.Rekey.FORGOTTEN, AccessoryKey.rekeyState(d))
        assertTrue(AccessoryKey.clearRekey(d))
        assertEquals(AccessoryKey.Rekey.NONE, AccessoryKey.rekeyState(d))
        assertFalse("idempotent", AccessoryKey.clearRekey(d))
        // force: the forget that began under PENDING re-asserts FORGOTTEN after a racing clear
        assertFalse("was NONE", AccessoryKey.markForgotten(d, force = true))
        assertEquals(AccessoryKey.Rekey.FORGOTTEN, AccessoryKey.rekeyState(d))
        assertTrue(AccessoryKey.clearRekey(d))
        assertFalse("no force, NONE stays NONE", AccessoryKey.markForgotten(d))
        assertEquals(AccessoryKey.Rekey.NONE, AccessoryKey.rekeyState(d))
        // the seed itself is untouched by the state machine
        assertEquals(32, File(d, AccessoryKey.SEED_FILE).length())
    }

    @Test fun unreadableRekeyFile_readsAsNone() {
        val d = tmpDir()
        File(d, AccessoryKey.REKEY_FILE).writeText("garbage\n")
        assertEquals(AccessoryKey.Rekey.NONE, AccessoryKey.rekeyState(d))
    }

    /** All three log paths (create, load, corrupt-replace): no line carries the seed in any encoding. */
    @Test fun logLines_neverCarryTheSeed() {
        val d = tmpDir(); val log = RecLog()
        val seeds = mutableListOf<ByteArray>()
        seeds += AccessoryKey.loadOrCreate(d, peersPresent = true, log = log).seed   // create
        seeds += AccessoryKey.loadOrCreate(d, peersPresent = true, log = log).seed   // load
        File(d, AccessoryKey.SEED_FILE).writeBytes(ByteArray(5))
        seeds += AccessoryKey.loadOrCreate(d, peersPresent = true, log = log).seed   // corrupt -> replace
        assertEquals(3, log.lines.size)   // created, loaded, the corrupt E (no second "created")
        for (s in seeds) {
            val hex = s.joinToString("") { "%02x".format(it) }
            val b64 = java.util.Base64.getEncoder().encodeToString(s)
            for (l in log.lines) {
                assertFalse(l, l.contains(hex)); assertFalse(l, l.contains(hex.substring(0, 8)))
                assertFalse(l, l.contains(b64)); assertFalse(l, l.contains(sha256(s).substring(0, 8)))
            }
        }
    }

    // ---- review fix1 cases --------------------------------------------------------------------------

    /**
     * fix1-1: the upgrade flag must be on disk BEFORE the seed is published. A dangling symlink at the
     * seed path makes `exists()` false but the O_EXCL claim fail (EEXIST), so install() cannot
     * publish and loadOrCreate throws — and the PENDING flag must already be there.
     */
    @Test fun upgrade_flagPersistedBeforeTheSeedIsPublished() {
        val d = tmpDir()
        Files.createSymbolicLink(File(d, AccessoryKey.SEED_FILE).toPath(), File(d, "does-not-exist").toPath())
        assertFalse(File(d, AccessoryKey.SEED_FILE).exists())
        var threw: Throwable? = null
        try { AccessoryKey.loadOrCreate(d, peersPresent = true, log = RecLog()) } catch (t: Throwable) { threw = t }
        assertTrue("install must fail here, got ${threw?.javaClass?.simpleName}", threw is IllegalStateException || threw is java.io.IOException)
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.rekeyState(d))
    }

    /** fix1-4: a 0-byte file (crash between claim and publish) with no peer store has nothing to rekey. */
    @Test fun corruptZeroByte_noPeers_noRekey_butNonEmptyOrPeers_pending() {
        val a = tmpDir(); File(a, AccessoryKey.SEED_FILE).writeBytes(ByteArray(0))
        val ra = AccessoryKey.loadOrCreate(a, peersPresent = false, log = RecLog(), now = { 1L })
        assertTrue(ra.created); assertEquals(AccessoryKey.Rekey.NONE, ra.rekey)
        assertFalse(File(a, AccessoryKey.REKEY_FILE).exists())
        assertTrue("kept even when empty", File(a, "${AccessoryKey.SEED_FILE}.corrupt-1").exists())

        val b = tmpDir(); File(b, AccessoryKey.SEED_FILE).writeBytes(ByteArray(0))
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.loadOrCreate(b, peersPresent = true, log = RecLog()).rekey)

        val c = tmpDir(); File(c, AccessoryKey.SEED_FILE).writeBytes(ByteArray(7) { 1 })
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.loadOrCreate(c, peersPresent = false, log = RecLog()).rekey)
    }

    /** fix1-2: when the move aside fails but the delete works, the E line says so. */
    @Test fun corruptFile_moveAsideBlocked_logSaysDeleted() {
        val d = tmpDir(); val log = RecLog()
        File(d, AccessoryKey.SEED_FILE).writeBytes(ByteArray(3))
        val aside = File(d, "${AccessoryKey.SEED_FILE}.corrupt-42")
        aside.mkdir(); File(aside, "occupant").writeText("x")      // a non-empty dir blocks the rename
        val r = AccessoryKey.loadOrCreate(d, peersPresent = false, log = log, now = { 42L })
        assertEquals(32, r.seed.size)
        assertTrue(aside.isDirectory)
        assertEquals(1, log.lines.count { it.startsWith("E accessory key file is 3 bytes, not 32 — move aside FAILED, deleted instead") })
        assertEquals(0, log.lines.count { it.contains("moved aside as") })
    }

    /** fix1-4: neither move nor delete possible -> fail loudly, never adopt the bytes that are there. */
    @Test fun corruptFile_cannotMoveOrDelete_throws_andLegacyBytesAreNeverAdopted() {
        val d = tmpDir(); val log = RecLog()
        val f = File(d, AccessoryKey.SEED_FILE); f.writeBytes(legacy())
        assertTrue(d.setWritable(false))
        try {
            var threw: Throwable? = null
            try { AccessoryKey.loadOrCreate(d, peersPresent = true, log = log) } catch (t: Throwable) { threw = t }
            assertTrue("got ${threw?.javaClass?.simpleName}", threw is IllegalStateException)
            assertTrue("still in place", f.exists())
            assertArrayEquals(legacy(), f.readBytes())
            assertEquals(1, log.lines.count { it.contains("move aside FAILED and delete FAILED") })
        } finally { d.setWritable(true) }
        assertEquals(AccessoryKey.Rekey.NONE, AccessoryKey.rekeyState(d))
    }

    /** A-X2 (2026-10-08): the rekey flag is published by an atomic replace of an fsynced temp; no temp survives. */
    @Test fun rekeyFlag_publishedAtomically_noTempLeftBehind() {
        val d = tmpDir()
        AccessoryKey.loadOrCreate(d, peersPresent = true, log = RecLog())
        assertEquals(AccessoryKey.Rekey.PENDING, AccessoryKey.rekeyState(d))
        assertTrue(AccessoryKey.markForgotten(d))
        val names = d.list()!!.toList()
        assertTrue(names.toString(), names.none { it.endsWith(".tmp") })
        assertEquals("forgotten\n", File(d, AccessoryKey.REKEY_FILE).readText())
    }
}
