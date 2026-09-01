package android.car.usb.handler

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import android.util.Log

/**
 * Secondary, best-effort confirmation. `deviceAttachedForFixedHandler` calls
 * `sendBroadcastAsUser(createDeviceAttachedIntent(device))` before launching the activity, so this
 * MAY fire and give an independent second log line that the attach reached user 10. Manifest
 * receivers for USB_DEVICE_ATTACHED are not a guaranteed delivery path, so this is never the sole
 * proof — [UsbHostManagementActivity] is. If both fire for one attach that is expected, not a bug.
 */
class UsbAttachReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != UsbManager.ACTION_USB_DEVICE_ATTACHED) return
        Log.i(UsbGrantProbe.TAG, "UsbAttachReceiver fired (broadcast path)")
        UsbGrantProbe.probe(context, receiverDevice(intent), source = "receiver")
    }

    @Suppress("DEPRECATION")
    private fun receiverDevice(intent: Intent): UsbDevice? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
        } else {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
        }
}
