package zeno.gmccpa

import android.content.Context

/**
 * The vehicle hotspot credentials the `0x5703` handoff carries, persisted across process restarts.
 *
 * Same shape as [zeno.gmccpa.logging.CapturePrefs]: one small file, read once by [LauncherUi] at
 * construction, written only when the driver confirms the credentials dialog. Until 2026-10-06 the
 * values lived only in `LauncherUi`'s fields, so every force-stop or AAOS-initiated process death
 * reverted them to the compiled-in defaults — and the next CT_SUBSCRIBE handed the iPhone the WRONG
 * SSID. iOS then removed the bench network it had been happily joined to (`Remove network …,
 * requested by "carkitd"`), which is the failure the persisted copy exists to end.
 *
 * Stored in PLAINTEXT in app-private SharedPreferences. That is the same exposure as the fields
 * themselves (the passphrase is shown unmasked on screen by design — see the dialog in `Ui.kt`),
 * and this head unit offers nothing stronger to an ordinary app. It is never logged from here.
 *
 * Blank or null values are never written: a blank SSID would be loaded back as "no SSID" and make
 * the bring-up refuse the radio-wake edge (`OcbmProbe.runAllLocked`), so an accidental empty Save
 * must not be able to poison the next launch. A key that was never written reads as null and the
 * caller starts blank (OD-7A, 2026-10-07: there are no compiled-in defaults any more, and autoStart
 * prompts instead of handing off). The `--es ssid/pass` scripted overrides deliberately do NOT come
 * through here — they stay in-memory for that run only, so test credentials cannot outlive the test
 * (`MainActivity.handleRunExtra`).
 *
 * There is no channel: the app sends no wifi_channel (owner, 2026-10-07). A `chan` an older build
 * stored is purged by the next [save] (plan v5.9 UI-2, D015) and is never read.
 */
object HotspotPrefs {
    private const val FILE = "hotspot"
    private const val KEY_SSID = "ssid"
    private const val KEY_PASS = "pass"
    private const val KEY_CHAN = "chan"

    /** A stored value, or null when none was ever saved (or it was blank — never stored). */
    fun ssid(ctx: Context): String? = prefs(ctx).getString(KEY_SSID, null)?.takeIf { it.isNotBlank() }
    fun pass(ctx: Context): String? = prefs(ctx).getString(KEY_PASS, null)?.takeIf { it.isNotBlank() }

    /**
     * Persist only the non-blank values; a blank leaves the stored one untouched. Also drops a
     * channel an older build stored, so the dead key does not linger (D015).
     */
    fun save(ctx: Context, ssid: String?, pass: String?) {
        val e = prefs(ctx).edit()
        ssid?.takeIf { it.isNotBlank() }?.let { e.putString(KEY_SSID, it) }
        pass?.takeIf { it.isNotBlank() }?.let { e.putString(KEY_PASS, it) }
        e.remove(KEY_CHAN)
        e.apply()
    }

    private fun prefs(ctx: Context) =
        ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
}
