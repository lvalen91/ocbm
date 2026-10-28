package zeno.gmccpa.ocbm

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/** Wave 3: the CT_SUBSCRIBE config never carries a Wi-Fi channel. Synthetic credentials only. */
class SubscribeConfigTest {
    private fun cfg(s: String?, p: String?) = String(SubscribeConfig.build(s, p), Charsets.UTF_8)

    @Test fun exactBlob_noChannel() = assertEquals(
        "name: gm_ccpa netprobe\nversion: 1\nwireless: true\npairing: just_works\nwifi_ap: false\n" +
        "wifi_ssid: TestNet\nwifi_pass: test-pass-1\n",
        cfg("TestNet", "test-pass-1"))

    @Test fun neverAChannelKey() {
        for ((s, p) in listOf("A" to "B", null to null, "" to "", "A" to null)) {
            val c = cfg(s, p)
            assertFalse(c, c.contains("wifi_channel"))
            assertFalse(c, c.contains("channel"))
        }
    }

    @Test fun blanksOmitted() {
        val c = cfg("  ", null)
        assertFalse(c.contains("wifi_ssid"))
        assertFalse(c.contains("wifi_pass"))
    }
}
