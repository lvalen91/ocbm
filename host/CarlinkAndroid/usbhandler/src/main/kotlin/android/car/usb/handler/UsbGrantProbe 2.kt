package android.car.usb.handler

import android.content.Context
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.util.Log

/**
 * The whole point of the proof, in one place.
 *
 * docs/host/01_ANDROID_AND_AAOS.md: on the GM Y181 image the fixed-handler path grants USB device-permission to the package
 * named `android.car.usb.handler` SILENTLY on every attach — IF that package is present in the
 * foreground user (10). This probe verifies exactly that claim, and NOTHING more:
 *
 *   1. It logs the device identity (VID/PID/serial) so an unknown adapter — e.g. a C2Air the first
 *      time it is plugged in — is discovered from logcat. The fixed handler sees every device.
 *   2. It reads [UsbManager.hasPermission] and asserts it is already `true`. The proof FAILS if we
 *      would have to call [UsbManager.requestPermission] — that call is the dialog we are trying to
 *      eliminate, so this code never makes it. `true` here with no prior request == the silent grant.
 *   3. It opens the device and, for the CCPA-OCBM, claims interface 0 / finds the 0x83·0x02 bulk
 *      pair, proving the grant is real and usable end to end — then releases and closes immediately.
 *
 * It drives no transport and keeps nothing open: production claiming lives in the real host app.
 */
object UsbGrantProbe {
    const val TAG = "UsbHandlerSquat"

    // CCPA identity — docs/carplay/00_ARCHITECTURE.md. VID is shared across stock/NCM/OCBM; only the PID says
    // which application protocol is on the wire.
    private const val CCPA_VID = 0x1314
    private const val CCPA_OCBM_PID = 0x2d00 // the converted, OCBM-speaking adapter — our real target
    private val CCPA_STOCK_PIDS = setOf(0x1520, 0x1521) // stock/NCM firmware, not yet converted

    fun probe(context: Context, device: UsbDevice?, source: String) {
        if (device == null) {
            Log.w(TAG, "[$source] attach with no EXTRA_DEVICE — nothing to probe")
            return
        }

        val vid = device.vendorId
        val pid = device.productId
        val id = "%04x:%04x".format(vid, pid)
        val kind = classify(vid, pid)
        Log.i(
            TAG,
            "[$source] attach $id ($kind) name=${device.deviceName} " +
                "serial=${runCatching { device.serialNumber }.getOrNull()} " +
                "class=${device.deviceClass}/${device.deviceSubclass} ifaces=${device.interfaceCount}",
        )

        val usb = context.getSystemService(Context.USB_SERVICE) as UsbManager

        // THE assertion. If this is false we must NOT requestPermission (that is the dialog); log the
        // failure of the whole premise and stop.
        val granted = usb.hasPermission(device)
        if (!granted) {
            Log.e(
                TAG,
                "[$source] hasPermission=FALSE for $id — the silent grant did NOT fire. Either this " +
                    "app is not installed in the foreground user (must be user 10, not 0 — see docs/host/01_ANDROID_AND_AAOS.md), " +
                    "or the fixed-handler path did not resolve us. NOT calling requestPermission.",
            )
            return
        }
        Log.i(TAG, "[$source] hasPermission=TRUE for $id — silent grant confirmed, no dialog")

        // Prove the grant is usable, only for the device we actually claim in production.
        if (!(vid == CCPA_VID && pid == CCPA_OCBM_PID)) {
            Log.i(TAG, "[$source] $id is not the CCPA-OCBM; logged and left alone (grant still held)")
            return
        }
        openAndReport(usb, device, source)
    }

    private fun openAndReport(usb: UsbManager, device: UsbDevice, source: String) {
        val conn = usb.openDevice(device)
        if (conn == null) {
            Log.e(TAG, "[$source] openDevice returned null despite hasPermission=true — grant unusable")
            return
        }
        try {
            val iface = device.getInterface(0)
            val claimed = conn.claimInterface(iface, true)
            var bulkIn = -1
            var bulkOut = -1
            for (e in 0 until iface.endpointCount) {
                val ep = iface.getEndpoint(e)
                if (ep.type == UsbConstants.USB_ENDPOINT_XFER_BULK) {
                    if (ep.direction == UsbConstants.USB_DIR_IN) bulkIn = ep.address
                    else bulkOut = ep.address
                }
            }
            Log.i(
                TAG,
                "[$source] CCPA-OCBM open OK: claimIf0=$claimed bulkIn=0x%02x bulkOut=0x%02x fd=%d — grant is real and usable"
                    .format(bulkIn.coerceAtLeast(0), bulkOut.coerceAtLeast(0), conn.fileDescriptor),
            )
            if (claimed) conn.releaseInterface(iface)
        } finally {
            // Never hold the device: the production host app claims it. Leaving it open would block
            // that app the moment it tries to claim interface 0.
            conn.close()
        }
    }

    private fun classify(vid: Int, pid: Int): String =
        when {
            vid == CCPA_VID && pid == CCPA_OCBM_PID -> "CCPA-OCBM"
            vid == CCPA_VID && pid in CCPA_STOCK_PIDS -> "CCPA-stock/NCM (unconverted)"
            else -> "other/unknown"
        }
}
