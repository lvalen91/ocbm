package zeno.gmccpa.av

import android.car.Car
import android.car.VehicleGear
import android.car.VehiclePropertyIds
import android.car.drivingstate.CarUxRestrictions
import android.car.drivingstate.CarUxRestrictionsManager
import android.car.hardware.CarPropertyValue
import android.car.hardware.property.CarPropertyManager
import android.content.Context
import android.content.res.Configuration
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.pair.NativeCore

/**
 * Drive the two vehicle-state levers CarPlay exposes as pure runtime commands:
 * **`setLimitedUI`** (restrict the UI while driving, release it in Park) and **`setNightMode`**
 * (follow the head unit's day/night theme).
 *
 * ## Why this can exist at all
 * Both are `/command` messages on the live event channel — **no reconnect, no `/info` change, no SETUP
 * feature negotiation** (`receiver::events::send_set_limited_ui` / `send_set_night_mode`). That is the
 * whole reason this feature is cheap: it binds to a session that is already streaming and cannot
 * disturb pairing, capability negotiation, or the advert. If either had needed an `/info` key we would
 * be re-pairing every phone on the rig to test it.
 *
 * The macOS host drives the same two receiver functions over OCBM (`CMD_LIMITED_UI_ON`/`_OFF`), and
 * `carlink_native` drives night mode with the OLD Carlinkit adapter opcodes (16/17) — both because the
 * box owns the CarPlay session in those designs. Here the app IS the receiver, so both go straight
 * through JNI and the box is not involved.
 *
 * ## Sources, and why these ones
 *
 * **Night — `Configuration.uiMode`.** No permission, no Car API. Identical signal to
 * `carlink_native`'s Compose `isSystemInDarkTheme()`, which is device-proven on the old firmware.
 * `MainActivity` and `CarPlayActivity` both declare `uiMode` in `configChanges`, so a theme toggle
 * arrives as `onConfigurationChanged` WITHOUT recreating the activity — that is the precondition, and
 * it is already met; do not remove `uiMode` from either manifest entry.
 *
 * **Drive — `GEAR_SELECTION`, falling back to `CarUxRestrictions`.** Measured on gminfo38
 * (2026-09-08), the permission that gates gear is the reason this is possible at all:
 *
 * | Permission | Protection | Usable here |
 * |---|---|---|
 * | `CAR_POWERTRAIN` (gear) | **`normal`** | yes — install-time, no prompt, no privilege |
 * | `CAR_SPEED` | `dangerous` | yes, but runtime-granted and not needed |
 * | `CAR_DRIVING_STATE` | `signature\|privileged` | **NO** |
 *
 * So `CarDrivingStateManager` — which gives the cleanest `PARKED/IDLING/MOVING` — is out of reach for
 * an unprivileged app in `/data/app`, and reading it from `dumpsys` is not something the app can do.
 * `GEAR_SELECTION` (`0x11400400`) is reachable and was observed live on this vehicle with the gear set
 * including `GEAR_PARK` and `GEAR_DRIVE`, so it is a real signal rather than a declared stub.
 *
 * `CarUxRestrictionsManager` needs **no permission whatsoever** and is the fallback: it reports
 * `isRequiresDistractionOptimization`, observed flipping true in the same instant as the shift out of
 * Park. It is second only because it answers a slightly different question — "should the UI be
 * distraction-optimised", which is policy — where the ask here was specifically about gear.
 *
 * ## Failure posture
 * Every Car API touch is wrapped: `android.car` is a `required="false"` shared library, so on a head
 * unit without it the class loader throws `NoClassDefFoundError` at first use rather than at install.
 * A missing Car API degrades to **night-mode-only** and logs it once. Nothing here may ever take down
 * a CarPlay session — a vehicle-state lever failing is a cosmetic loss; the session is the product.
 *
 * ## Threading
 * `NativeCore.setLimitedUI`/`setNightMode` BLOCK on the global event mutex, and Car callbacks arrive
 * on the main looper, so every native call is handed to the single-thread `cp-vehicle` executor.
 * Serialising on one thread also makes the last write win, which is what we want when a gear change
 * and a theme change land together.
 */
class VehicleStateWatcher(private val ctx: Context) {
    private val log = ProbeLog.sub("vhcl")
    private val io = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "cp-vehicle").apply { isDaemon = true }
    }

    private var car: Car? = null
    private var props: CarPropertyManager? = null
    private var uxr: CarUxRestrictionsManager? = null

    /** Last state actually pushed to iOS. Null = never pushed, so the first assert always sends. */
    @Volatile private var sentLimited: Boolean? = null
    @Volatile private var sentNight: Boolean? = null

    /** Latest observed inputs, independent of what has been sent. */
    @Volatile private var gearLimited: Boolean? = null   // null = gear unknown / unavailable
    @Volatile private var uxrLimited: Boolean? = null
    /** From the NIGHT_MODE vehicle property. Null until it answers; [uiNight] is the fallback. */
    @Volatile private var propNight: Boolean? = null
    /** From `Configuration.uiMode`. On this head unit GM does NOT drive it — see [night]. */
    @Volatile private var uiNight: Boolean = false

    /** True once a session is live; commands before that are pointless (no event channel). */
    @Volatile private var sessionUp = false

    /**
     * Shared by the change callback and the initial seed read.
     *
     * Kept as a plain Int method rather than something taking a `CarPropertyValue`, because that
     * class exposes no public constructor to build one from a seed read — the seed has an Int and
     * nothing else, so an Int is the honest parameter.
     */
    private fun applyGear(g: Int) {
        // Anything that is not PARK is "driving" for our purposes. Reverse and Neutral restrict too:
        // the vehicle can move, which is the only thing the restriction is about. This matches what
        // AAOS itself does — distraction optimisation went true at IDLING, not at MOVING, when
        // measured on this rig.
        val limited = when (g) {
            VehicleGear.GEAR_PARK -> false
            // UNKNOWN (0) is not "parked". Treat it as no-opinion and let the UXR fallback speak,
            // rather than releasing the restriction on a garbage read.
            0 -> null
            else -> true
        }
        if (limited != gearLimited) {
            gearLimited = limited
            log.i("gear=${gearName(g)} -> limitedUI=${limited ?: "no opinion"}")
            reassert("gear")
        }
    }

    private val gearCb = object : CarPropertyManager.CarPropertyEventCallback {
        override fun onChangeEvent(v: CarPropertyValue<*>) {
            applyGear(v.value as? Int ?: return)
        }

        override fun onErrorEvent(propId: Int, zone: Int) {
            log.w("gear property error propId=0x${propId.toString(16)} zone=$zone — leaning on UXR")
            gearLimited = null
        }
    }

    private fun applyNightProp(on: Boolean) {
        if (propNight != on) {
            propNight = on
            log.i("NIGHT_MODE property -> $on")
            reassert("night-prop")
        }
    }

    private val nightCb = object : CarPropertyManager.CarPropertyEventCallback {
        override fun onChangeEvent(v: CarPropertyValue<*>) {
            applyNightProp(v.value as? Boolean ?: return)
        }

        override fun onErrorEvent(propId: Int, zone: Int) {
            log.w("NIGHT_MODE property error zone=$zone — falling back to uiMode")
            propNight = null
        }
    }

    private val uxrCb = CarUxRestrictionsManager.OnUxRestrictionsChangedListener { r ->
        val limited = r.isRequiresDistractionOptimization
        if (limited != uxrLimited) {
            uxrLimited = limited
            log.i("uxr: distractionOptimization=$limited (active=0x${r.activeRestrictions.toString(16)})")
            reassert("uxr")
        }
    }

    fun start() {
        uiNight = isNight(ctx.resources.configuration)
        log.i("night(uiMode)=$uiNight at start")
        try {
            val c = Car.createCar(ctx) ?: throw IllegalStateException("Car.createCar returned null")
            car = c
            props = (c.getCarManager(Car.PROPERTY_SERVICE) as? CarPropertyManager)?.also { pm ->
                val ok = pm.registerCallback(
                    gearCb, VehiclePropertyIds.GEAR_SELECTION, CarPropertyManager.SENSOR_RATE_ONCHANGE,
                )
                // Seed from the current value: ONCHANGE only fires on the next shift, so without this
                // the app sits with no opinion until the driver happens to move the lever.
                if (ok) {
                    runCatching { pm.getIntProperty(VehiclePropertyIds.GEAR_SELECTION, 0) }
                        .onSuccess { applyGear(it) }
                        .onFailure { log.w("gear seed read failed: ${it.javaClass.simpleName}: ${it.message}") }
                }
                log.i("GEAR_SELECTION subscribe=${if (ok) "OK" else "REFUSED"} (CAR_POWERTRAIN is `normal`)")

                // NIGHT_MODE (0x11200407), gated by CAR_EXTERIOR_ENVIRONMENT — also `prot=normal`.
                // This is the REAL day/night source on this head unit; see desiredNight().
                val nOk = pm.registerCallback(
                    nightCb, VehiclePropertyIds.NIGHT_MODE, CarPropertyManager.SENSOR_RATE_ONCHANGE,
                )
                if (nOk) {
                    runCatching { pm.getBooleanProperty(VehiclePropertyIds.NIGHT_MODE, 0) }
                        .onSuccess { applyNightProp(it) }
                        .onFailure { log.w("NIGHT_MODE seed read failed: ${it.javaClass.simpleName}: ${it.message}") }
                }
                log.i("NIGHT_MODE subscribe=${if (nOk) "OK" else "REFUSED"} (CAR_EXTERIOR_ENVIRONMENT is `normal`)")
            }
            uxr = (c.getCarManager(Car.CAR_UX_RESTRICTION_SERVICE) as? CarUxRestrictionsManager)?.also { m ->
                m.registerListener(uxrCb)
                runCatching { m.currentCarUxRestrictions }.getOrNull()?.let { uxrCb.onUxRestrictionsChanged(it) }
                log.i("CarUxRestrictions listener attached (no permission required)")
            }
        } catch (t: Throwable) {
            // NoClassDefFoundError when android.car is absent, SecurityException, or car service not
            // up yet. Night mode does not depend on any of it, so degrade rather than fail.
            log.w("Car API unavailable (${t.javaClass.simpleName}: ${t.message}) — night mode only")
        }
    }

    fun stop() {
        runCatching { props?.unregisterCallback(gearCb) }
        runCatching { props?.unregisterCallback(nightCb) }
        runCatching { uxr?.unregisterListener() }
        runCatching { car?.disconnect() }
        props = null; uxr = null; car = null
        io.shutdown()
    }

    /** Theme changed. Call from `onConfigurationChanged`, which fires without recreating the activity. */
    fun onConfigurationChanged(cfg: Configuration) {
        val n = isNight(cfg)
        if (n != uiNight) {
            uiNight = n
            log.i("night(uiMode) -> $n${if (propNight != null) " (ignored: NIGHT_MODE property wins)" else ""}")
            reassert("uiMode")
        }
    }

    /**
     * A session came up. Both levers are session-scoped — the receiver refuses them with no event
     * channel — so state is NOT persisted across sessions and has to be pushed fresh here.
     */
    fun onSessionUp() {
        sessionUp = true
        sentLimited = null
        sentNight = null
        reassert("session-up")
    }

    fun onSessionDown() {
        sessionUp = false
        sentLimited = null
        sentNight = null
    }

    /** Gear wins when it has an opinion; otherwise the permission-free UXR signal. */
    private fun desiredLimited(): Boolean? = gearLimited ?: uxrLimited

    /**
     * The vehicle property wins over `uiMode`.
     *
     * DEVICE-MEASURED 2026-09-08, and the reason this is not just `isSystemInDarkTheme()`: with the
     * truck's night state active, `cmd uimode night` still reported `no` and `Configuration.uiMode`
     * read day. GM does not propagate the vehicle's night state into the Android `uiMode` on this
     * head unit, so `carlink_native`'s Compose signal — correct on the old Carlinkit firmware — is
     * simply not wired here. `NIGHT_MODE` (`0x11200407`) is, and was live with 2 subscribers.
     *
     * `uiMode` is kept as the fallback rather than deleted: it costs nothing, needs no permission,
     * and is the right answer on a head unit that DOES wire it.
     */
    private fun desiredNight(): Boolean = propNight ?: uiNight

    private fun reassert(why: String, attempt: Int = 0) {
        if (!sessionUp) return
        io.execute {
            if (!sessionUp) return@execute
            val wantLimited = desiredLimited()
            val wantNight = desiredNight()
            var refused = false

            if (wantLimited != null && wantLimited != sentLimited) {
                if (NativeCore.setLimitedUI(wantLimited)) {
                    sentLimited = wantLimited
                    log.i("setLimitedUI($wantLimited) sent [$why]")
                } else {
                    refused = true
                }
            }
            if (wantNight != sentNight) {
                if (NativeCore.setNightMode(wantNight)) {
                    sentNight = wantNight
                    log.i("setNightMode($wantNight) sent [$why]")
                } else {
                    refused = true
                }
            }

            // RETRY, because "session up" and "event channel usable" are NOT the same instant.
            // Device-measured 2026-09-08: the supervisor logged `awaiting dial-back -> CarPlay live`
            // at 12:52:07.779 and BOTH commands were refused at 12:52:07.833 — 54 ms later. State was
            // only recovered because the driver happened to shift gear afterwards. Start a session
            // already in Drive, never touch the lever, and the restriction would never be asserted at
            // all — which is the failure that actually matters.
            if (refused) {
                if (attempt < RETRY_MAX) {
                    io.schedule({ reassert(why, attempt + 1) }, RETRY_DELAY_MS, TimeUnit.MILLISECONDS)
                } else {
                    log.w("gave up asserting vehicle state [$why] after $RETRY_MAX tries — no event channel")
                }
            }
        }
    }

    private companion object {
        /** ~8 s of cover at 400 ms — comfortably past the observed 54 ms gap without spinning. */
        const val RETRY_MAX = 20
        const val RETRY_DELAY_MS = 400L

        fun isNight(cfg: Configuration): Boolean =
            (cfg.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES

        fun gearName(g: Int): String = when (g) {
            VehicleGear.GEAR_PARK -> "PARK"
            VehicleGear.GEAR_DRIVE -> "DRIVE"
            VehicleGear.GEAR_NEUTRAL -> "NEUTRAL"
            VehicleGear.GEAR_REVERSE -> "REVERSE"
            0 -> "UNKNOWN"
            else -> "0x${g.toString(16)}"
        }
    }
}
