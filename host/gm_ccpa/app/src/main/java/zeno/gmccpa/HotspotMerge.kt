package zeno.gmccpa

/**
 * The credentials dialog's Save rule (plan v5.8 V7 / Q019): a field the driver left BLANK keeps the
 * value that is already running, exactly as [HotspotPrefs.save] keeps the stored one. Until v5.8
 * the dialog assigned the trimmed field text unconditionally, so a Save with one field cleared ran
 * the probe with a blank SSID or passphrase (`OcbmProbe.runAllLocked` then refuses the radio-wake
 * edge) while the persisted copy still held the old value — the running and the saved credentials
 * disagreed, which `HotspotPrefs` exists to prevent.
 *
 * Pure (no android.*): `HotspotMergeTest` runs it under the tier-0 JVM tests.
 */
object HotspotMerge {
    /** [typed] trimmed when it has any non-blank content, else [previous] unchanged. */
    fun merge(previous: String, typed: CharSequence?): String {
        val t = typed?.toString()?.trim().orEmpty()
        return if (t.isEmpty()) previous else t
    }
}
