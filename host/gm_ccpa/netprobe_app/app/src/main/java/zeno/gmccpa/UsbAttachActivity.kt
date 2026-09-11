package zeno.gmccpa

import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Bundle
import android.os.Process
import android.os.SystemClock
import zeno.gmccpa.ocbm.Ocbm
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture
import zeno.gmccpa.logging.SessionSummary
import zeno.gmccpa.logging.SessionTrace
import zeno.gmccpa.ocbm.OcbmExpect
import zeno.gmccpa.ocbm.UsbBulkTransport

/**
 * The USB attach entry point: a NoDisplay trampoline that filters to the OCBM adapter and forwards
 * into [MainActivity]'s attach path.
 *
 * ## Why this is no longer a package squat (2026-09-08)
 * This class used to be `android.car.usb.handler.UsbHostManagementActivity`, and the app used to
 * install under that package name, because GM's framework-res sets
 * `config_UsbDeviceConnectionHandling_component` to that exact component and then STRIPS the
 * package — so the theory was that squatting the name would make the framework grant USB permission
 * to our UID silently, with no dialog.
 *
 * **IT DID NOT WORK (owner, 2026-09-10).** This KDoc used to say the squat "made the framework grant
 * USB permission to our UID silently on every attach, with no dialog (device-proven 2026-08-17)" and
 * that "it worked, but only about half the time". That is too generous and is the kind of note that
 * gets a bad idea re-attempted: **the app still needed the user to grant permission for the
 * adapter.** The squat did not remove the permission requirement, which was its whole purpose. The
 * old wording also contradicted itself — "every attach, device-proven" against "about half the time
 * in the field" — and the owner's account is which of the two to believe. It made the app
 * impersonate a platform component for a benefit that never materialised.
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
 * ## Why this logs so much (2026-08-26; premise corrected 2026-09-10)
 * Written when the squat's behaviour was thought to be intermittent rather than absent. The fields
 * are still exactly the right evidence — for a dialog that does not appear, a grant that does not
 * land, or a box that returns under a different descriptor — so they are kept verbatim; only the
 * premise below was wrong. As recorded: some days every attach seemed silent, other days
 * the permission dialog returned for days on end. It was never reproducible with a Mac attached, so
 * the fault was never captured. (Tense corrected 2026-09-10: this paragraph still read as if the
 * squat were live, two days after it was reverted. The question it poses — grant landed or not, and
 * why — is unchanged under the ordinary resolver; only the mechanism that was supposed to grant is.) This is the EARLIEST code we control on an attach — the framework
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
        runCatching { LogCapture.start(applicationContext, CapturePrefs.config(applicationContext)) }
            .onFailure { ProbeLog.sub("cap").e("capture start failed at attach: ${it.message}") }
        try {
            val dev = intentDevice()
            if (dev == null) {
                ProbeLog.banner("USB ATTACH TRAMPOLINE launched with no device — nothing to forward")
                logIdentity(null)
                return
            }
            val isCcpaOcbm = dev.vendorId == UsbBulkTransport.VID_CARLINKIT &&
                dev.productId == UsbBulkTransport.PID_OCBM
            // Read the grant ONCE and reuse it: the banner and the session summary must not be able
            // to disagree about the single fact this whole exercise exists to attribute.
            val held = usbManager().hasPermission(dev)
            // Wording (2026-09-10): this banner used to read "USB HANDLER (fixed-handler squat) ...
            // silent grant held=", which described the package squat reverted 2026-09-08 (see the
            // class KDoc). A fresh capture carrying that text would tell its reader the squat is still
            // in place. Same fields, same order — nothing greps the old text (tools, docs and
            // evidence/ checked) — only the mechanism it names is corrected: `held` is now whether the
            // ORDINARY resolver's attach-time grant (or a remembered "always open") is in effect.
            ProbeLog.banner(
                "USB ATTACH TRAMPOLINE 0x%04x:0x%04x — permission held=%b, ccpa=%b"
                    .format(dev.vendorId, dev.productId, held, isCcpaOcbm)
            )
            val serialOutcome = logIdentity(dev)
            if (!isCcpaOcbm) return // not our adapter — do not raise any UI; grant already fired
            // Degraded, and worth a WARN here rather than only in the claim loop later: the attach
            // resolver is SUPPOSED to grant before launching us, so a missing grant at this point
            // means either the cold-start race (young process — see the class KDoc) or a dialog the
            // driver is about to get. The claim loop polls either way; this line dates the fact.
            if (!held) ProbeLog.sub("usb").w(
                "USB permission NOT held at the trampoline (process_age=%dms, serial=%s) — the attach resolver's grant did not land or has not committed yet; the claim loop will poll and may raise the dialog"
                    .format(SystemClock.elapsedRealtime() - Process.getStartElapsedRealtime(), serialOutcome)
            )

            // A session begins at the adapter attach, not at first video: everything we are hunting
            // happens before a session is "up" in any user-visible sense. begin() also closes out any
            // prior session that never ended cleanly, which is itself a fault worth a summary line.
            SessionSummary.begin(
                SessionSummary.AttachInfo(
                    origin = SessionSummary.Origin.USB_ATTACH,
                    hasPermissionAtTrampoline = held,
                    uid = Process.myUid(),
                    userId = Process.myUid() / 100_000,
                    processAgeMs = SystemClock.elapsedRealtime() - Process.getStartElapsedRealtime(),
                    serialOutcome = serialOutcome,
                    descriptorFingerprint = SessionSummary.descriptorFingerprint(dev),
                )
            )

            // What must happen next, and by when. MainActivity.handleAttachIntent submits a link
            // attempt to its single command executor as soon as the forwarded intent lands, and
            // OcbmProbe.runAllLocked meets this at its first line. If it does not arrive, the
            // executor is wedged behind an earlier task (an old awaitClaimable, a stop that never
            // returned) and the app is "attached but doing nothing" — the case nothing logged before.
            // Budget: Ocbm.HEARTBEAT_GRACE_MS, the protocol's own "a peer that has said nothing for
            // this long is gone" window; the hop itself is a startActivity plus one executor submit
            // and completes in well under a second when the executor is free. No number of its own
            // exists for this hop, and inventing one would be worse than borrowing that one.
            SessionTrace.expect(OcbmExpect.LINK_ATTEMPT, Ocbm.HEARTBEAT_GRACE_MS,
                "the attach trampoline forwarded 0x1314:0x2d00 to MainActivity (permission held=$held), whose attach handler submits a link attempt at once")
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
        val devLine = "attach dev: name=${dev.deviceName} id=${dev.deviceId} serial=$serial " +
            "mfr=${dev.manufacturerName} product=${dev.productName}"
        // The SecurityException IS the finding (grant not held), so that variant is a WARN; the
        // other two are inventory.
        if (outcome == SessionSummary.SerialOutcome.SECURITY_EXCEPTION) log.w(devLine) else log.i(devLine)
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
