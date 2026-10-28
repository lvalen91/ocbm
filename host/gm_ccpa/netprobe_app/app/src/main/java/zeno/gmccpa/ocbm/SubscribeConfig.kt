package zeno.gmccpa.ocbm

/**
 * The CT_SUBSCRIBE config blob, built from the hotspot credentials. Pure (no android.*), so the host
 * JUnit tests run the exact bytes the probe sends.
 *
 * `wireless`/`pairing`/`wifi_ap`/`wifi_*` are read by the box supervisor's raw greps;
 * `accessoryConfig` would be read by carplayd's serde. Unknown keys are silently ignored by both.
 *
 * There is deliberately NO `wifi_channel` line (owner, 2026-10-07): the app sends a blank channel on
 * purpose and never asks the driver for one. With the key absent the box leaves its stock hostapd
 * channel line alone (tools/session_supervisor.sh:845-848), so the 0x5703 handoff still carries the box's
 * stock channel. A channel an older build saved in HotspotPrefs is never passed in here.
 */
object SubscribeConfig {
    fun build(ssid: String?, pass: String?): ByteArray {
        val sb = StringBuilder()
        sb.append("name: gm_ccpa netprobe\n")
        sb.append("version: 1\n")
        sb.append("wireless: true\n")
        sb.append("pairing: just_works\n")
        // The box is the Bluetooth radio and the MFi coprocessor only — it raises no AP of its own.
        sb.append("wifi_ap: false\n")
        ssid?.takeIf { it.isNotBlank() }?.let { sb.append("wifi_ssid: ").append(it).append('\n') }
        pass?.takeIf { it.isNotBlank() }?.let { sb.append("wifi_pass: ").append(it).append('\n') }
        return sb.toString().toByteArray(Charsets.UTF_8)
    }
}
