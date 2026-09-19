package com.carlink.car

import android.car.Car
import android.car.CarAppFocusManager
import android.content.Context
import android.content.pm.PackageManager
import com.carlink.logging.Logger
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn

/**
 * Tells AAOS "this app is the active navigation app" while CarPlay route guidance is running.
 *
 * [CarAppFocusManager] is the one piece of the car API that is permission-free AND meaningful for a
 * projection app: holding `APP_FOCUS_TYPE_NAVIGATION` is what makes the platform's navigation-context
 * owner point at us (`AppFocusService` -> instrument-cluster/`ClusterHome` nav-context owner) and
 * what tells the previous holder — a native maps app mid-route — that it has lost the road
 * (`onAppFocusOwnershipLost`). That is exactly the arbitration a native CarPlay integration performs
 * when the phone starts navigating. Reporting the turn-by-turn itself (`CarNavigationStatusManager`)
 * is privileged and out of scope; this claim is the part a third party may do.
 *
 * `APP_FOCUS_TYPE_VOICE_COMMAND` is deliberately NOT claimed: nothing in the AOSP car stack acts on it
 * for a third party, and the only truthful edge we have for "Siri is listening" is the mic-uplink
 * gate, which is also raised for a phone call.
 *
 * Edge-driven from iAP2 `RouteGuidanceUpdate.routeGuidanceState` (0 = no route). Everything is
 * guarded for non-automotive builds exactly like [DriveStateMonitor]; on a phone this is a no-op.
 */
class AppFocusClaimer(
    private val context: Context,
) {
    private var car: Car? = null
    private var mgr: CarAppFocusManager? = null

    /** What the session wants; applied when the car service is ready, re-applied on reconnect. */
    @Volatile private var wantNavigation = false

    /** True while the platform has granted us NAVIGATION focus. */
    @Volatile var holdsNavigation = false
        private set

    private val ownership =
        object : CarAppFocusManager.OnAppFocusOwnershipCallback {
            override fun onAppFocusOwnershipLost(appType: Int) {
                if (appType != CarAppFocusManager.APP_FOCUS_TYPE_NAVIGATION) return
                holdsNavigation = false
                logInfo("[FOCUS] NAVIGATION focus lost to another app", tag = Logger.Tags.ADAPTR)
            }

            override fun onAppFocusOwnershipGranted(appType: Int) {
                if (appType != CarAppFocusManager.APP_FOCUS_TYPE_NAVIGATION) return
                holdsNavigation = true
                logInfo("[FOCUS] NAVIGATION focus granted", tag = Logger.Tags.ADAPTR)
            }
        }

    /** Idempotent; a no-op where there is no car service. Main thread. */
    fun start() {
        if (car != null) return
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_AUTOMOTIVE)) return
        runCatching {
            Car.createCar(context, null, Car.CAR_WAIT_TIMEOUT_DO_NOT_WAIT) { c, ready ->
                if (ready) onCarReady(c) else onCarLost()
            }
        }.onSuccess { car = it }
            .onFailure { logWarn("[FOCUS] Car.createCar failed: ${it.message}", tag = Logger.Tags.ADAPTR) }
    }

    fun stop() {
        setNavigating(false)
        mgr = null
        runCatching { car?.disconnect() }
        car = null
    }

    /**
     * Edge from route guidance: true while iOS reports an active route. Safe to call repeatedly and
     * before the car service is up — the wish is remembered and applied on [onCarReady].
     */
    fun setNavigating(active: Boolean) {
        if (wantNavigation == active) return
        wantNavigation = active
        apply()
    }

    private fun apply() {
        val m = mgr ?: return
        runCatching {
            if (wantNavigation) {
                val r = m.requestAppFocus(CarAppFocusManager.APP_FOCUS_TYPE_NAVIGATION, ownership)
                holdsNavigation = r == CarAppFocusManager.APP_FOCUS_REQUEST_SUCCEEDED
                logInfo("[FOCUS] requestAppFocus(NAVIGATION) -> ${if (holdsNavigation) "granted" else "refused ($r)"}", tag = Logger.Tags.ADAPTR)
            } else if (holdsNavigation) {
                m.abandonAppFocus(ownership, CarAppFocusManager.APP_FOCUS_TYPE_NAVIGATION)
                holdsNavigation = false
                logInfo("[FOCUS] abandoned NAVIGATION focus", tag = Logger.Tags.ADAPTR)
            }
        }.onFailure { logWarn("[FOCUS] app-focus call failed: ${it.message}", tag = Logger.Tags.ADAPTR) }
    }

    private fun onCarReady(c: Car) {
        mgr =
            runCatching { c.getCarManager(Car.APP_FOCUS_SERVICE) as? CarAppFocusManager }
                .getOrNull()
                ?: run {
                    logWarn("[FOCUS] CarAppFocusManager unavailable", tag = Logger.Tags.ADAPTR)
                    return
                }
        apply()
    }

    private fun onCarLost() {
        mgr = null
        holdsNavigation = false
    }
}
