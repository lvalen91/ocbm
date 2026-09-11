package zeno.gmccpa.logging

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import zeno.gmccpa.ProbeLog

/**
 * Starts log capture at boot, so the window before the first USB attach is covered too.
 *
 * ## This is a bonus, not the load-bearing path
 * Nothing keeps this process alive. A process started only by `BOOT_COMPLETED` holds no foreground
 * service and LMK will reap it, quite possibly within minutes and certainly long before a drive
 * starts. Do not reason about capture coverage as if this receiver guarantees it.
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
    }
}
