package zeno.gmccpa.ocbm

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Wave 3: the adapter's Bluetooth name out of MGMT_INFO. Synthetic values only. */
class MgmtInfoTest {
    /** The exact key order/shape ocbmd's mgmt_info format! emits (ccpa/ocbmd/src/main.rs). */
    private fun info(name: String, hostName: String = "gm_ccpa netprobe") =
        "{\"bt_mac\":\"00:11:22:33:44:55\",\"wifi_mac\":\"00:11:22:33:44:66\",\"serial\":\"SYNTH0001\"," +
        "\"name\":\"$name\",\"uptime_s\":12,\"rootfs_pct\":40,\"rootfs_free_kb\":1000,\"ssp\":true,\"hci_up\":true," +
        "\"wlan_ap\":false,\"transport\":\"wireless\",\"host_present\":true,\"phone_present\":false," +
        "\"daemons\":{\"ocbmd\":true,\"iap2d\":true,\"airplayd\":true,\"carplay_wireless\":false}," +
        "\"host_name\":\"$hostName\",\"devices\":[\"00:11:22:33:44:77\"]}"

    @Test fun realShape_name() = assertEquals("CarLink-4466", MgmtInfo.btName(info("CarLink-4466")))
    @Test fun neverReturnsBtMac() = assertEquals("CarLink", MgmtInfo.btName(info("CarLink")))
    @Test fun nullOrEmpty() { assertNull(MgmtInfo.btName(null)); assertNull(MgmtInfo.btName("")) }
    @Test fun noNameKey_hostNameIsNotName() =
        assertNull(MgmtInfo.btName("{\"bt_mac\":\"00:11:22:33:44:55\",\"host_name\":\"CarLink-evil\"}"))
    @Test fun nestedNameIgnored() =
        assertNull(MgmtInfo.btName("{\"daemons\":{\"name\":\"nested\"},\"devices\":[{\"name\":\"x\"}]}"))
    @Test fun nameInsideAnotherValueIgnored() =
        assertEquals("CarLink-0001", MgmtInfo.btName(info("CarLink-0001", hostName = "a\\\",\\\"name\\\":\\\"spoof")))
    @Test fun escapedNameInsideValue_beforeRealKey() =
        assertEquals("real", MgmtInfo.btName("{\"transport\":\"x\\\"name\\\":\\\"fake\",\"name\":\"real\"}"))
    @Test fun whitespaceAroundColon() = assertEquals("CarLink-ab12", MgmtInfo.btName("{ \"name\" : \"CarLink-ab12\" }"))
    @Test fun unicodeEscape() = assertEquals("CarLink-é", MgmtInfo.btName("{\"name\":\"CarLink-\\u00e9\"}"))
    @Test fun controlCharsStripped_andTrimmed() = assertEquals("CarLinkX", MgmtInfo.btName("{\"name\":\" CarLink\\nX\\t \"}"))
    @Test fun capped() = assertEquals(MgmtInfo.MAX_NAME, MgmtInfo.btName("{\"name\":\"${"a".repeat(200)}\"}")!!.length)
    @Test fun blankOrNonString() {
        assertNull(MgmtInfo.btName("{\"name\":\"\"}"))
        assertNull(MgmtInfo.btName("{\"name\":\"   \"}"))
        assertNull(MgmtInfo.btName("{\"name\":42}"))
        assertNull(MgmtInfo.btName("{\"name\":null}"))
    }
    @Test fun malformed() {
        assertNull(MgmtInfo.btName("{\"name\":\"unterminated"))
        assertNull(MgmtInfo.btName("not json"))
        assertNull(MgmtInfo.btName("{\"name\""))
        assertNull(MgmtInfo.btName("}{\"name\":\"x\"}"))
    }

    // ---- logChunks (E-F2, 2026-10-08): element-boundary splitting for the MGMT_INFO log lines ----

    @Test fun logChunks_shortInputIsOneChunk() = assertEquals(listOf(info("CarLink-0001")), MgmtInfo.logChunks(info("CarLink-0001")))

    @Test fun logChunks_neverSplitsInsideAString_andConcatenatesBack() {
        val json = info("CarLink-0001").replace("\"devices\":[\"00:11:22:33:44:77\"]",
            "\"devices\":[" + (0 until 40).joinToString(",") { "\"00:11:22:33:%02x:%02x\"".format(it, it) } + "]")
        for (max in listOf(30, 64, 110, 200)) {
            val chunks = MgmtInfo.logChunks(json, max)
            assertEquals(json, chunks.joinToString(""))
            for (c in chunks) {
                // Quotes balance within every chunk: no string value straddles a boundary.
                assertEquals(c, 0, c.count { it == '"' } % 2)
                // Every chunk but the last ends on the comma it was cut at.
                if (c !== chunks.last()) assertEquals(c, ',', c.last())
            }
            assertEquals(chunks.toString(), chunks.size > 1, json.length > max)
        }
    }

    @Test fun logChunks_overlongElementBecomesOneLongChunk() {
        val json = "{\"a\":\"" + "x".repeat(100) + "\",\"b\":1}"
        val chunks = MgmtInfo.logChunks(json, 20)
        assertEquals(json, chunks.joinToString(""))
        assertEquals(listOf("{\"a\":\"" + "x".repeat(100) + "\",", "\"b\":1}"), chunks)
    }

    @Test fun logChunks_escapedQuoteAndCommaInsideStringDoNotCut() {
        val json = "{\"n\":\"a\\\",b,c\",\"m\":2,\"k\":3}"
        val chunks = MgmtInfo.logChunks(json, 12)
        assertEquals(json, chunks.joinToString(""))
        assertEquals("{\"n\":\"a\\\",b,c\",", chunks[0])
    }
}
