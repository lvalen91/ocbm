package android.car.usb.handler

import android.app.Activity
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.util.Log

/**
 * The fixed-handler entry point. Its fully-qualified name MUST be
 * `android.car.usb.handler.UsbHostManagementActivity` — the literal string in GM's
 * `config_UsbDeviceConnectionHandling_component` (docs/host/01_ANDROID_AND_AAOS.md). The framework launches it by explicit
 * component from `deviceAttachedForFixedHandler`, AFTER it has already granted us permission, with
 * the device in [UsbManager.EXTRA_DEVICE].
 *
 * NoDisplay: it must never draw. It reads the device, hands it to [UsbGrantProbe], and finishes in
 * [onCreate] before the window would show. Theme.NoDisplay REQUIRES finish() before the activity
 * becomes visible, which is exactly the contract here.
 */
class UsbHostManagementActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        try {
            val device = intentDevice()
            Log.i(UsbGrantProbe.TAG, "UsbHostManagementActivity launched (fixed-handler path)")
            UsbGrantProbe.probe(this, device, source = "activity")
        } finally {
            // Always finish, even if the probe throws — a NoDisplay activity that lingers is a
            // visible black frame in front of the driver.
            finish()
        }
    }

    @Suppress("DEPRECATION")
    private fun intentDevice(): UsbDevice? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
        } else {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
        }
}
