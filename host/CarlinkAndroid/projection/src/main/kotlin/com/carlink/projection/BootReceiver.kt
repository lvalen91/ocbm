package com.carlink.projection

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.carlink.logging.ProbeLog

/**
 * Starts the projection owner at boot.
 *
 * §5.1 fixes the ordering: **detect display → generate VehicleConfig → bring up BT + Wi-Fi →
 * auto-connect**. [ProjectionService.onCreate] does the first two steps before it starts anything
 * else, which is why this receiver only has to get the service running early rather than sequence
 * anything itself.
 *
 * `BOOT_COMPLETED` rather than `LOCKED_BOOT_COMPLETED`: the config is written to `/tmp` and the
 * device screen reads `SharedPreferences`, neither of which is available in direct-boot mode, and a
 * head unit has no lock screen to wait behind anyway.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(
        context: Context,
        intent: Intent,
    ) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        ProbeLog.sub("boot").i("BOOT_COMPLETED — starting projection service (connectedDevice only)")
        // The service is told WHERE it was started from, because that changes which foreground
        // service types it may claim. Android 15+ refuses mediaPlayback and microphone from a
        // BOOT_COMPLETED receiver. See ProjectionService.bootRestricted.
        ProjectionService.start(context, fromBoot = true)
    }
}
