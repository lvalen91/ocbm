package com.carlink.ocbm

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [BinaryPlist] against fixtures produced by APPLE'S OWN ENCODER.
 *
 * Every fixture here was written by `plutil -convert binary1` on macOS and pasted in as hex, so the
 * parser is checked against bytes Apple generated rather than against a re-implementation of the
 * format. A shared misreading of the spec therefore cannot cancel itself out, which is the whole
 * risk with a hand-rolled decoder for someone else's wire format.
 */
class BinaryPlistTest {
    private fun hex(s: String): ByteArray =
        ByteArray(s.length / 2) {
            ((Character.digit(s[it * 2], 16) shl 4) or Character.digit(s[it * 2 + 1], 16)).toByte()
        }

    /** Apple-encoded: {type: requestUI} */
    private val simple =
        hex(
            "62706c6973743030d10102547479706559726571756573745549080b1000000000000001010000000000000003000000" +
                "0000000000000000000000001a",
        )

    /** Apple-encoded: {type: requestUI, params:{url: ...}} */
    private val withurl =
        hex(
            "62706c6973743030d201020304547479706556706172616d7359726571756573745549d105065375726c5f101f6d6170" +
                "733a2f6361722f696e737472756d656e74636c75737465722f6d6170080d121923262a00000000000001010000000000" +
                "0000070000000000000000000000000000004c",
        )

    /** Apple-encoded: mixed scalar types */
    private val mixed =
        hex(
            "62706c6973743030d60102030405060708090c0d0e53756e69516e536172725474797065526f6e536f66666600630061" +
                "006600e900202705111092a20a0b100110025c7365744c696d69746564554909080815191b1f24272b383b3e40424f50" +
                "0000000000000101000000000000000f00000000000000000000000000000051",
        )

    @Test
    fun `reads the command verb from an Apple-encoded plist`() {
        val d = BinaryPlist.parseDict(simple)
        assertEquals("requestUI", d?.get("type"))
    }

    @Test
    fun `reads a nested params dict`() {
        val d = BinaryPlist.parseDict(withurl)
        assertEquals("requestUI", d?.get("type"))

        @Suppress("UNCHECKED_CAST")
        val params = d?.get("params") as? Map<String, Any?>
        assertEquals("maps:/car/instrumentcluster/map", params?.get("url"))
    }

    @Test
    fun `decodes booleans integers unicode and arrays`() {
        val d = BinaryPlist.parseDict(mixed) ?: error("should parse")
        assertEquals("setLimitedUI", d["type"])
        assertEquals(true, d["on"])
        assertEquals(false, d["off"])
        assertEquals(4242L, d["n"])
        // Non-ASCII forces the UTF-16BE branch, encoded differently from the ASCII one.
        assertEquals("caf\u00e9 \u2705", d["uni"])
        assertEquals(listOf(1L, 2L), d["arr"])
    }

    // ---- hostile / malformed input: null, never an exception --------------------------------------

    @Test
    fun `rejects non-plist input`() {
        assertNull(BinaryPlist.parseDict(ByteArray(0)))
        assertNull(BinaryPlist.parseDict("not a plist at all".toByteArray()))
        assertNull(BinaryPlist.parseDict(ByteArray(64) { 0x41 }))
    }

    /**
     * The payload crosses the wire from the phone and is parsed ON THE OCBM READ THREAD, so an
     * exception here would take the whole dispatch loop with it. Truncation at every length must be
     * survivable.
     */
    @Test
    fun `every truncation of a valid plist returns null rather than throwing`() {
        for (n in 0 until withurl.size) {
            assertNull("truncated to $n bytes must not parse", BinaryPlist.parseDict(withurl.copyOfRange(0, n)))
        }
    }

    @Test
    fun `single-byte corruption anywhere does not throw`() {
        for (i in 8 until withurl.size) {
            val bad = withurl.copyOf()
            bad[i] = 0xFF.toByte()
            // May return null or a partial map; the contract is only that it does not throw.
            BinaryPlist.parseDict(bad)
        }
        assertTrue("survived every single-byte corruption", true)
    }
}
