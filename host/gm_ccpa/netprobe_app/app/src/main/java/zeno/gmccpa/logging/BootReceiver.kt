package zeno.gmccpa.logging

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import zeno.gmccpa.ProbeLog

/**
 * Starts log capture at boot, so the window before the first USB attach is covered too, and (plan
 * v5.9 D6, trigger T2) runs the auto-claim: an adapter present at boot is claimed with the app closed.
 *
 * Corrected 2026-10-08 (D6): BOOT_COMPLETED IS delivered to this receiver on gminfo37 — the u10 record
 * dispatched it ~+55.7 s after `Posting BOOT_COMPLETED user #10` on the 2026-10-07 cold boot — about
 * 48 s AFTER the media-browser bind (T3) has usually claimed already, so T2 is mostly a NOOP. Its value:
 * the boot FGS temp allowlist (REASON 200) lets it place the adapter-link hold if T3's start was
 * refused, and it claims when FM was the source and T3 never ran.
 *
 * ## Capture here is a bonus, not the load-bearing path
 * A process started only by `BOOT_COMPLETED` holds no foreground service unless the auto-claim placed
 * the adapter-link hold, and LMK may reap it. Do not reason about capture coverage as if this
 * receiver guarantees it.
 *
 * Coverage is guaranteed by a different property: `logcat -d` dumps what is ALREADY in the ring
 * buffer, so [LogCapture] started at USB attach still recovers the history that predates it. The
 * trampoline is the real entry point ([zeno.gmccpa.UsbAttachActivity]); this
 * receiver just widens the window for free when the process happens to survive.
 *
 * Registered for `BOOT_COMPLETED`, not `LOCKED_BOOT_COMPLETED`: capture writes under `filesDir`,
 * which on a file-based-encryption device is credential-encrypted and not readable until the user
 * is unlocked. Starting earlier would fail to open the very first file.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val log = ProbeLog.sub("cap")
        val started = runCatching { LogCapture.start(context, CapturePrefs.config(context)) }
            .onFailure { log.e("boot start failed: ${it.message}") }
            .getOrDefault(false)
        log.i("BOOT_COMPLETED — capture start=$started scope=${CapturePrefs.scope(context)}")
        zeno.gmccpa.AutoClaim.trigger(context.applicationContext, zeno.gmccpa.AutoClaim.Origin.BOOT)
    }
}
