package com.carlink.car

import android.car.Car
import android.car.drivingstate.CarUxRestrictionsManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import com.carlink.logging.Logger
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn

/**
 * Reports whether the vehicle is being driven, so CarPlay's `setLimitedUI` can follow it.
 *
 * **Why UX restrictions and not `GEAR_SELECTION`.** Reading the gear directly needs
 * `android.car.permission.CAR_POWERTRAIN`, which is `signature|privileged` — unavailable to a
 * sideloaded app. [CarUxRestrictionsManager] needs no permission to *listen*, and AAOS already
 * derives it from gear and speed: injecting `GEAR_SELECTION=DRIVE` on the emulator flips
 * `DO: false UxR: 0` to `DO: true UxR: 16` and back on `PARK`, verified on device.
 *
 * It is also the better semantic match. `isRequiresDistractionOptimization` and CarPlay's
 * `setLimitedUI` are the same statement — "the driver is driving, restrict the UI" — whereas gear
 * is one input a vehicle might use to reach it. A vehicle that restricts on speed rather than gear, or
 * keeps restrictions through a rolling stop, is handled for free.
 *
 * Everything here is guarded for non-automotive builds: the class is only touched behind a
 * [PackageManager.FEATURE_AUTOMOTIVE] check, `android.car` is declared `required="false"`, and
 * `Car` is linked `compileOnly`. On a phone this object constructs, reports nothing, and costs
 * nothing.
 */
class DriveStateMonitor(
    private val context: Context,
) {
    /**
     * Invoked on the main thread with true when the vehicle requires distraction optimization.
     *
     * Fires on every change AND once on connect with the current value, so a caller that pushes
     * this to the box does not need its own priming step.
     */
    @Volatile var onDriveStateChanged: ((Boolean) -> Unit)? = null

    /** Last known state. False until the car service reports otherwise, which is the safe default. */
    @Volatile var driving: Boolean = false
        private set

    /** True once the car service has actually answered — [driving] is a guess until then. */
    @Volatile var connected: Boolean = false
        private set

    private var car: Car? = null
    private var uxrManager: CarUxRestrictionsManager? = null
    private val handler = Handler(Looper.getMainLooper())

    private val listener =
        CarUxRestrictionsManager.OnUxRestrictionsChangedListener { r ->
            publish(r.isRequiresDistractionOptimization)
        }

    /** Idempotent. Safe to call on any device; a no-op where there is no car service. */
    fun start() {
        if (car != null) return
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_AUTOMOTIVE)) {
            logInfo("[DRIVE] Not an automotive build — drive-state tracking disabled", tag = Logger.Tags.ADAPTR)
            return
        }

        // CAR_WAIT_TIMEOUT_DO_NOT_WAIT: the blocking form of createCar can stall for seconds if
        // car service is still coming up, and this runs on the main thread during startup.
        runCatching {
            Car.createCar(context, null, Car.CAR_WAIT_TIMEOUT_DO_NOT_WAIT) { c, ready ->
                if (ready) onCarReady(c) else onCarLost()
            }
        }.onSuccess { car = it }
            .onFailure { logWarn("[DRIVE] Car.createCar failed: ${it.message}", tag = Logger.Tags.ADAPTR) }
    }

    fun stop() {
        runCatching { uxrManager?.unregisterListener() }
        uxrManager = null
        runCatching { car?.disconnect() }
        car = null
        connected = false
    }

    private fun onCarReady(c: Car) {
        val mgr =
            runCatching { c.getCarManager(Car.CAR_UX_RESTRICTION_SERVICE) as? CarUxRestrictionsManager }
                .getOrNull()
        if (mgr == null) {
            logWarn("[DRIVE] CarUxRestrictionsManager unavailable", tag = Logger.Tags.ADAPTR)
            return
        }
        uxrManager = mgr
        connected = true

        runCatching {
            mgr.registerListener(listener)
            // Prime from the current value: registerListener does not replay, so without this a
            // session that starts while already in Drive would never be told.
            publish(mgr.currentCarUxRestrictions?.isRequiresDistractionOptimization ?: false)
        }.onFailure { logWarn("[DRIVE] registerListener failed: ${it.message}", tag = Logger.Tags.ADAPTR) }
    }

    private fun onCarLost() {
        logWarn("[DRIVE] Car service disconnected", tag = Logger.Tags.ADAPTR)
        uxrManager = null
        connected = false
    }

    /** Edge-triggered: the box only needs a command when the state actually changes. */
    private fun publish(now: Boolean) {
        val changed = now != driving || !connected
        driving = now
        if (!changed) return
        logInfo("[DRIVE] requiresDistractionOptimization=$now", tag = Logger.Tags.ADAPTR)
        handler.post { onDriveStateChanged?.invoke(now) }
    }
}
