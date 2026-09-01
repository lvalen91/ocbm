package com.carlink.device

import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The store's durability contract, against real `SharedPreferences` under Robolectric.
 *
 * The property that matters is not "a setter followed by a getter agrees" — that would pass for a
 * purely in-memory cache. It is that state left this process's object graph and can be read back by
 * a *different* store instance, which is the closest honest proxy for process death available
 * without an instrumented test.
 */
@RunWith(RobolectricTestRunner::class)
class KnownDeviceStoreTest {
    private val ctx get() = ApplicationProvider.getApplicationContext<android.content.Context>()

    private fun store(name: String) = KnownDeviceStore(ctx, prefsName = name)

    private fun dev(
        mac: String,
        name: String? = null,
    ) = KnownDevice(mac = mac, name = name, firstSeenMs = 1_000L, lastConnectedMs = 2_000L)

    /** Writes are asynchronous by design; drain the store's thread before reading it back. */
    private fun KnownDeviceStore.settle() = close()

    @Test
    fun `a fresh store is empty and reports not loaded until read`() {
        val s = store("fresh")
        assertFalse(s.loaded)
        assertEquals(KnownDeviceSnapshot(), s.load())
        assertTrue(s.loaded)
        s.close()
    }

    @Test
    fun `state survives into a brand new store instance`() {
        val a = store("survive")
        a.load()
        a.update { it.copy(devices = mapOf("aa" to dev("aa", "Test iPhone")), preferredMac = "aa") }
        a.settle()

        // A different object over the same Context — the write must have left process memory.
        val b = store("survive")
        val snap = b.load()
        assertEquals("Test iPhone", snap.devices["aa"]?.name)
        assertEquals("aa", snap.preferredMac)
        b.close()
    }

    /**
     * Everything lives in one prefs file and nothing in a static, so the OS wiping app storage
     * genuinely empties the feature. (That "clear data" deletes the prefs directory is Android's
     * contract; what this pins is that we keep nothing anywhere else.)
     */
    @Test
    fun `clearing the prefs file empties the store`() {
        val a = store("cleared")
        a.load()
        a.update { it.copy(devices = mapOf("aa" to dev("aa", "Gone"))) }
        a.settle()

        ctx
            .getSharedPreferences("cleared", android.content.Context.MODE_PRIVATE)
            .edit()
            .clear()
            .commit()

        val b = store("cleared")
        assertEquals(KnownDeviceSnapshot(), b.load())
        b.close()
    }

    @Test
    fun `clear wipes both memory and disk`() {
        val a = store("wipe")
        a.load()
        a.update { it.copy(devices = mapOf("aa" to dev("aa")), preferredMac = "aa") }
        a.clear()
        assertEquals(KnownDeviceSnapshot(), a.snapshot)
        a.settle()

        val b = store("wipe")
        assertTrue(b.load().devices.isEmpty())
        assertNull(b.snapshot.preferredMac)
        b.close()
    }

    @Test
    fun `a caller reads its own write immediately`() {
        val s = store("readback")
        s.load()
        s.update { it.copy(preferredMac = "bb") }
        // Synchronous in memory even though the disk write is deferred.
        assertEquals("bb", s.snapshot.preferredMac)
        s.close()
    }

    @Test
    fun `a no-op mutation does not change the snapshot identity`() {
        val s = store("noop")
        s.load()
        val first = s.update { it.copy(devices = mapOf("aa" to dev("aa"))) }
        val second = s.update { it }
        assertTrue("an unchanged mutation should return the same instance", first === second)
        s.close()
    }

    @Test
    fun `an unreadable document degrades to empty rather than throwing`() {
        ctx
            .getSharedPreferences("corrupt", android.content.Context.MODE_PRIVATE)
            .edit()
            .putString("snapshot_v1", "{ this is not json")
            .commit()
        val s = store("corrupt")
        assertEquals(KnownDeviceSnapshot(), s.load())
        s.close()
    }
}
