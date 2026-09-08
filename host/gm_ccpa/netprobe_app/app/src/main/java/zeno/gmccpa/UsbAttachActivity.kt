package zeno.gmccpa

import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Bundle
import android.os.Process
import android.os.SystemClock
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.SessionSummary
import zeno.gmccpa.ocbm.UsbBulkTransport

/**
 * The USB attach entry point: a NoDisplay trampoline that filters to the OCBM adapter and forwards
 * into [MainActivity]'s attach path.
 *
 * ## Why this is no longer a package squat (2026-09-08)
 * This class used to be `android.car.usb.handler.UsbHostManagementActivity`, and the app used to
 * install under that package name, because GM's framework-res sets
 * `config_UsbDeviceConnectionHandling_component` to that exact component and then STRIPS the
 * package — so squatting the name made the framework grant USB permission to our UID silently on
 * every attach, with no dialog (device-proven 2026-08-17). It worked, but only about half the time
 * in the field (see the logging rationale below), and it made the app impersonate a platform
 * component for a benefit that was never reliable.
 *
 * The app is back to `zeno.gmccpa`, so the fixed-handler path no longer resolves to us at all and
 * the ORDINARY attach resolver is the only route in: the `<intent-filter>` +
 * `res/xml/device_filter.xml` below match `0x1314:0x2d00`, the framework shows the standard USB
 * permission dialog, and "always open for this device" makes it a one-time cost per install. That
 * degrades correctly because [UsbBulkTransport] polls `hasPermission()` as the authoritative signal
 * rather than trusting the grant broadcast — the claim loop simply waits until the grant exists.
 *
 * The diagnostics below are kept as-is. They were written to explain the squat's intermittency, and
 * every one of them is equally the right evidence for a dialog that does not appear, a grant that
 * does not land, or a box that comes back with a different descriptor.
 *
 * ## Why this logs so much (2026-08-26)
 * The squat works maybe half the time in the field: some days every attach is silent, other days the
 * permission dialog returns for days on end. It is never reproducible with a Mac attached, so the
 * fault has never been captured. This is the EARLIEST code we control on an attach — the framework
 * launches it for every device whether or not the grant landed — so it is the right place to record
 * the state that separates the competing explanations. Every field below exists to kill one
 * hypothesis; none of it is decoration:
 *
 *  - **uid / user** — the grant is per-UID *and per Android user*. The app installs `--user 10`
 *    (docs/06 §86). If an attach is ever handled for a different user, the grant simply does not
 *    apply and the dialog is correct behaviour, not a bug.
 *  - **process age** — the framework grants and *then* launches us. On a cold start we may sample
 *    `hasPermission` before the grant commits, which looks identical to "no grant" in a log but is a
 *    race. A young process with `held=false` means race; an old process with `held=false` means the
 *    grant genuinely is not there.
 *  - **device identity (serial, config/interface shape)** — the permission cache is keyed on device
 *    identity. The CCPA can enumerate as more than one descriptor variant (`/script/ncm_only` flips
 *    it between a shell-bearing NCM build and a pure OCBM accessory), and a different descriptor is
 *    a different device to the cache, so a previously-granted box can come back unrecognised.
 *  - **sourceDir / versionName** — proves WHICH apk instance the framework actually resolved, which
 *    is the check that matters if a stock `android.car.usb.handler` is ever present to compete.
 *
 * A `SecurityException` reading the serial is itself a finding: since API 29 `getSerialNumber()` is
 * gated on holding USB permission for the device, so it fails exactly when the grant did not land.
 */
class UsbAttachActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // FIRST, before anything can throw. This is the earliest code we control on an attach — the
        // framework launches it for every USB device whether or not the grant landed — which makes it
        // the only place a capture can be armed ahead of the faults we are hunting. It is idempotent
        // and non-blocking, and because the engine drains logcat's ring buffer BACKWARDS on start, an
        // attach-time start still recovers the minutes that preceded the app being alive at all.
        runCatching { LogCapture.start(applicationContext, CapturePrefs.config(applicationContext, "4.0")) }
            .onFailure { ProbeLog.sub("cap").e("capture start failed at attach: ${it.message}") }
        try {
            val dev = intentDevice()
            if (dev == null) {
                ProbeLog.banner("USB HANDLER launched with no device — nothing to forward")
                logIdentity(null)
                return
            }
            val isCcpaOcbm = dev.vendorId == UsbBulkTransport.VID_CARLINKIT &&
                dev.productId == UsbBulkTransport.PID_OCBM
            // Read the grant ONCE and reuse it: the banner and the session summary must not be able
            // to disagree about the single fact this whole exercise exists to attribute.
            val held = usbManager().hasPermission(dev)
            ProbeLog.banner(
                "USB HANDLER (fixed-handler squat) 0x%04x:0x%04x — silent grant held=%b, ccpa=%b"
                    .format(dev.vendorId, dev.productId, held, isCcpaOcbm)
            )
            val serialOutcome = logIdentity(dev)
            if (!isCcpaOcbm) return // not our adapter — do not raise any UI; grant already fired

            // A session begins at the adapter attach, not at first video: everything we are hunting
            // happens before a session is "up" in any user-visible sense. begin() also closes out any
            // prior session that never ended cleanly, which is itself a fault worth a summary line.
            SessionSummary.begin(
                SessionSummary.AttachInfo(
                    hasPermissionAtTrampoline = held,
                    uid = Process.myUid(),
                    userId = Process.myUid() / 100_000,
                    processAgeMs = SystemClock.elapsedRealtime() - Process.getStartElapsedRealtime(),
                    serialOutcome = serialOutcome,
                    descriptorFingerprint = SessionSummary.descriptorFingerprint(dev),
                )
            )

            // Forward into MainActivity's existing ACTION_USB_DEVICE_ATTACHED handler. Same UID, so the
            // grant carries; MainActivity is singleTask and reads EXTRA_DEVICE in onCreate/onNewIntent.
            startActivity(
                Intent(this, MainActivity::class.java).apply {
                    action = UsbManager.ACTION_USB_DEVICE_ATTACHED
                    putExtra(UsbManager.EXTRA_DEVICE, dev)
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                }
            )
        } finally {
            finish() // NoDisplay: never linger in front of the driver, even if forwarding throws.
        }
    }

    /**
     * The discriminators, on their own lines so one attach is greppable as a block. Runs for a null
     * device too — "launched with no device" is itself one of the failure shapes worth attributing to
     * a user/uid.
     */
    private fun logIdentity(dev: UsbDevice?): SessionSummary.SerialOutcome {
        val log = ProbeLog.sub("usb")
        // userId is uid / PER_USER_RANGE. UserHandle.myUserId() is @hide on API 32; the division is
        // the same arithmetic the framework does and needs no reflection or hidden-API access.
        val uid = Process.myUid()
        val ageMs = SystemClock.elapsedRealtime() - Process.getStartElapsedRealtime()
        log.i("attach ctx: uid=$uid user=${uid / 100_000} pid=${Process.myPid()} process_age=${ageMs}ms")

        val pkg = runCatching { packageManager.getPackageInfo(packageName, 0) }.getOrNull()
        log.i("attach ctx: pkg=$packageName v=${pkg?.versionName} src=${applicationInfo.sourceDir}")

        if (dev == null) return SessionSummary.SerialOutcome.UNKNOWN

        // getSerialNumber() is permission-gated since API 29 — a throw here means the grant is NOT
        // held for this device, which is the single most direct read on the fault.
        var outcome = SessionSummary.SerialOutcome.OK
        val serial = try {
            dev.serialNumber ?: "<null>".also { outcome = SessionSummary.SerialOutcome.NULL_SERIAL }
        } catch (e: SecurityException) {
            outcome = SessionSummary.SerialOutcome.SECURITY_EXCEPTION
            "<SecurityException: ${e.message}> — USB permission NOT held for this device"
        }
        log.i(
            "attach dev: name=${dev.deviceName} id=${dev.deviceId} serial=$serial " +
                "mfr=${dev.manufacturerName} product=${dev.productName}"
        )
        log.i(
            "attach dev: class=${dev.deviceClass}/${dev.deviceSubclass}/${dev.deviceProtocol} " +
                "configs=${dev.configurationCount} ifaces=${dev.interfaceCount}"
        )
        // The interface shape is what distinguishes the adapter's descriptor variants from each
        // other, and therefore whether the permission cache should have recognised this box at all.
        for (i in 0 until dev.interfaceCount) {
            val itf = dev.getInterface(i)
            log.i(
                "attach dev: iface[$i] id=${itf.id} alt=${itf.alternateSetting} " +
                    "class=${itf.interfaceClass}/${itf.interfaceSubclass}/${itf.interfaceProtocol} " +
                    "eps=${itf.endpointCount} name=${itf.name}"
            )
        }
        return outcome
    }

    private fun usbManager() = getSystemService(USB_SERVICE) as UsbManager

    // Deprecated 1-arg form on purpose: the app compiles against API 32 (gminfo37), where the typed
    // getParcelableExtra(String, Class) and Build.VERSION_CODES.TIRAMISU do not exist. Same call
    // MainActivity uses.
    @Suppress("DEPRECATION")
    private fun intentDevice(): UsbDevice? =
        intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
}
