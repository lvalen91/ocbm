package zeno.gmccpa

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager

/**
 * plan v5.9 D6 trigger T1: the adapter attached (hot plug or box reboot) with the app closed.
 *
 * On this unit the fixed-handler branch runs: UsbHostManager broadcasts USB_DEVICE_ATTACHED to the
 * CURRENT user with FLAG_RECEIVER_INCLUDE_BACKGROUND (so a manifest receiver gets it despite the
 * implicit-broadcast ban), BEFORE the failing handler lookup that leaves UsbAttachActivity dormant
 * (D6 F1/F2). `exported="false"`: the sender is system_server, whose uid always passes the receiver
 * permission check (B-D6-1a confirms delivery on this build; U59-18). Not delivered for the at-boot
 * enumeration (that is user 0's; T2/T3 cover boot) or to a force-stopped package.
 *
 * No meta-data/device filter: those are matched for activities only. [UsbManager.EXTRA_DEVICE] is a
 * hint — anything that is not the OCBM device (0x1314:0x2d00) is logged and ignored here; the real
 * checks (enumeration, mode, permission) are AutoClaim's pre-check and policy. Never requests
 * permission, never starts an activity.
 */
class AdapterAttachReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != UsbManager.ACTION_USB_DEVICE_ATTACHED) return
        @Suppress("DEPRECATION")
        val dev = intent.getParcelableExtra<UsbDevice>(UsbManager.EXTRA_DEVICE)
        if (dev == null || dev.vendorId != zeno.gmccpa.ocbm.UsbBulkTransport.VID_CARLINKIT ||
            dev.productId != zeno.gmccpa.ocbm.UsbBulkTransport.PID_OCBM) {
            ProbeLog.sub("autoclaim").i("auto-claim (usb-attach): ignored — not the OCBM device")
            return
        }
        AutoClaim.trigger(context.applicationContext, AutoClaim.Origin.USB_ATTACH)
    }
}
