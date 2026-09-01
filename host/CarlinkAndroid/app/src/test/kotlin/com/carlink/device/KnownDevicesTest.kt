package com.carlink.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The persisted document format and the merge rules.
 *
 * The document is a compatibility surface — one app version writes it, the next reads it — so it is
 * pinned two ways: a golden string for what the writer emits, and a hand-authored fixture the writer
 * never produced for what the reader must accept. `read(write(x)) == x` alone would pass for any
 * self-consistent format, including one a future version cannot parse.
 */
class KnownDevicesTest {
    private fun dev(
        mac: String,
        name: String? = null,
        first: Long = 1_000L,
        last: Long? = null,
    ) = KnownDevice(mac = mac, name = name, firstSeenMs = first, lastConnectedMs = last)

    // ---- codec -----------------------------------------------------------------------------------

    @Test
    fun `encodes to the expected document`() {
        val snap =
            KnownDeviceSnapshot(
                devices =
                    mapOf(
                        "aa:bb:cc:dd:ee:ff" to
                            KnownDevice(
                                mac = "aa:bb:cc:dd:ee:ff",
                                name = "Test iPhone",
                                model = "iPhone18,4",
                                osName = "iPhone OS",
                                osVersion = "27.0",
                                firstSeenMs = 1_700_000_000_000L,
                                lastConnectedMs = 1_700_000_009_999L,
                            ),
                    ),
                preferredMac = "aa:bb:cc:dd:ee:ff",
            )
        assertEquals(
            """{"v":1,"preferredMac":"aa:bb:cc:dd:ee:ff","devices":[{"mac":"aa:bb:cc:dd:ee:ff",""" +
                """"name":"Test iPhone","model":"iPhone18,4","osName":"iPhone OS","osVersion":"27.0",""" +
                """"firstSeenMs":1700000000000,"lastConnectedMs":1700000009999}]}""",
            KnownDeviceCodec.encode(snap),
        )
    }

    /** A document this test wrote by hand, NOT one produced by [KnownDeviceCodec.encode]. */
    @Test
    fun `reads a hand-authored v1 document`() {
        val snap =
            KnownDeviceCodec.decode(
                """
                {"v":1,"preferredMac":"11:22:33:44:55:66","devices":[
                  {"mac":"11:22:33:44:55:66","name":"Spare","firstSeenMs":5,"lastConnectedMs":9},
                  {"mac":"AA:BB:CC:DD:EE:FF","firstSeenMs":1}
                ]}
                """.trimIndent(),
            )
        assertEquals(2, snap.devices.size)
        assertEquals("Spare", snap.devices["11:22:33:44:55:66"]?.name)
        assertEquals(9L, snap.devices["11:22:33:44:55:66"]?.lastConnectedMs)
        // MACs normalise to lower case, so the join against MGMT_INFO cannot miss on case alone.
        assertEquals("aa:bb:cc:dd:ee:ff", snap.devices["aa:bb:cc:dd:ee:ff"]?.mac)
        // Absent optional fields decode as null, not "".
        assertNull(snap.devices["aa:bb:cc:dd:ee:ff"]?.name)
        assertNull(snap.devices["aa:bb:cc:dd:ee:ff"]?.lastConnectedMs)
        assertEquals("11:22:33:44:55:66", snap.preferredMac)
    }

    /** Device names are typed by the user on the phone, so they are hostile input. */
    @Test
    fun `round-trips a name with quotes backslashes newlines and emoji`() {
        val nasty = "Owner\"s \\ iPhone 🚗\nline2"
        val snap = KnownDeviceSnapshot(devices = mapOf("aa" to dev("aa", name = nasty)))
        val back = KnownDeviceCodec.decode(KnownDeviceCodec.encode(snap))
        assertEquals(nasty, back.devices["aa"]?.name)
    }

    @Test
    fun `decode never throws and yields empty on anything unreadable`() {
        for (bad in listOf(null, "", "   ", "not json", "[]", "{", """{"v":99,"devices":[]}""")) {
            assertEquals(KnownDeviceSnapshot(), KnownDeviceCodec.decode(bad))
        }
        // A record with no MAC cannot be joined against anything, so it is dropped rather than kept.
        assertTrue(KnownDeviceCodec.decode("""{"v":1,"devices":[{"name":"x"}]}""").devices.isEmpty())
    }

    @Test
    fun `encoding is deterministic regardless of map order`() {
        val a = KnownDeviceSnapshot(devices = linkedMapOf("bb" to dev("bb"), "aa" to dev("aa")))
        val b = KnownDeviceSnapshot(devices = linkedMapOf("aa" to dev("aa"), "bb" to dev("bb")))
        assertEquals(KnownDeviceCodec.encode(a), KnownDeviceCodec.encode(b))
    }

    // ---- merge -----------------------------------------------------------------------------------

    private fun merge(
        known: Map<String, KnownDevice>,
        bonded: Set<String>,
        suppressed: Set<String> = emptySet(),
        preferred: String? = null,
    ) = mergeDeviceList(known, bonded, suppressed, preferred) { "t$it" }

    @Test
    fun `a bonded device the app has never seen appears under its MAC`() {
        val out = merge(emptyMap(), setOf("aa:bb:cc:dd:ee:ff"))
        assertEquals(1, out.size)
        assertEquals("aa:bb:cc:dd:ee:ff", out[0].name)
        assertTrue(out[0].bonded)
        assertNull(out[0].lastConnected)
    }

    @Test
    fun `a remembered device carries its name and last-seen`() {
        val out = merge(mapOf("aa" to dev("aa", name = "Test iPhone", last = 42L)), setOf("aa"))
        assertEquals("Test iPhone", out[0].name)
        assertEquals("t42", out[0].lastConnected)
    }

    /**
     * History outlives the bond, but must never render as connectable — the box cannot page a phone
     * it has no link key for, and offering Connect is a lie the user discovers by tapping it.
     */
    @Test
    fun `history without a bond is listed but marked not bonded`() {
        val out = merge(mapOf("aa" to dev("aa", name = "Old")), bonded = emptySet())
        assertEquals(1, out.size)
        assertEquals("Old", out[0].name)
        assertEquals(false, out[0].bonded)
    }

    @Test
    fun `a suppressed MAC is excluded from both sides of the merge`() {
        val out = merge(mapOf("aa" to dev("aa", name = "Gone")), bonded = setOf("aa"), suppressed = setOf("aa"))
        assertTrue(out.isEmpty())
    }

    @Test
    fun `ordering is preferred then most-recently-connected then first-seen`() {
        val known =
            mapOf(
                "aa" to dev("aa", first = 1, last = 100),
                "bb" to dev("bb", first = 2, last = 300),
                "cc" to dev("cc", first = 3, last = null),
            )
        // No preference: recency wins, never-connected last.
        assertEquals(listOf("bb", "aa", "cc"), merge(known, setOf("aa", "bb", "cc")).map { it.btMac })
        // Explicit selection beats recency, and is sticky.
        assertEquals(
            listOf("cc", "bb", "aa"),
            merge(known, setOf("aa", "bb", "cc"), preferred = "cc").map { it.btMac },
        )
    }

    @Test
    fun `a preferred MAC is matched case-insensitively`() {
        val out = merge(mapOf("aa" to dev("aa"), "bb" to dev("bb", last = 9)), setOf("aa", "bb"), preferred = "AA")
        assertEquals("aa", out[0].btMac)
    }

    @Test
    fun `a device in both history and the bond list appears once`() {
        val out = merge(mapOf("aa" to dev("aa", name = "One")), setOf("aa", "AA:"))
        assertEquals(1, out.count { it.btMac == "aa" })
    }
}
