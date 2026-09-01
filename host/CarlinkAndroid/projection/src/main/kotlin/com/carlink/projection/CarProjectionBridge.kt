package com.carlink.projection

import android.car.Car
import android.car.CarProjectionManager
import android.car.drivingstate.CarUxRestrictions
import android.car.drivingstate.CarUxRestrictionsManager
import android.car.projection.ProjectionStatus
import android.content.Context
import android.content.pm.PackageManager
import com.carlink.logging.ProbeLog

/**
 * Binding to AAOS's own projection framework.
 *
 * `pi/docs/01_PROJECTION_APP_DESIGN.md` §4 is the reasoning; this implements the "adopt immediately"
 * half — the parts that do **not** require the platform to own a radio, which on this Pi it does not
 * (`carplay-wireless` drives `hci0` directly and `hostapd` runs outside the Wi-Fi framework).
 *
 * What we take:
 *
 *  - `updateProjectionStatus` — without it `CarProjectionService` reports no bound projection app,
 *    and the power policy's `PROJECTION` component, the HMI and every other app have no idea a
 *    CarPlay session exists. The single cheapest thing that turns "a process that happens to be
 *    running" into something AAOS understands.
 *  - `addKeyEventHandler` — steering-wheel voice and call keys. Registering makes the platform
 *    *suppress its own default handling*, which beats intercepting them anywhere else.
 *  - [CarUxRestrictionsManager] — drive state, forwarded to iOS as `setLimitedUI`.
 *
 * What we deliberately do not take: `startProjectionAccessPoint` (LocalOnlyHotspot) and
 * `getAvailableWifiChannels`, both of which assume the platform owns Wi-Fi. Standalone `hostapd` is
 * what currently works on 5 GHz here; §4.3 tracks testing the platform path as a simplification.
 *
 * ## The API is not what the name suggests
 *
 * `updateProjectionStatus` takes a whole [ProjectionStatus] object built through nested builders —
 * not a `(state, packageName, extras)` triple. The real signatures were read off the Pi's own
 * `/system/framework/android.car.jar`; see `car-system-stubs/` for the provenance and for why
 * compile-only stubs are needed at all (the public SDK strips every `@SystemApi` member).
 *
 * ## Every call is defensive on purpose
 *
 * `android.car` is optional, these APIs are `@SystemApi` behind privileged permissions, and a build
 * may decline any of it at runtime. A `SecurityException` from a nice-to-have integration must never
 * take down the session actually carrying the driver's music, so failures are logged and swallowed.
 */
class CarProjectionBridge(
    private val ctx: Context,
    private val status: SessionStatus,
    private val onSiriDown: () -> Unit,
    private val onSiriUp: () -> Unit,
    private val onCallKey: () -> Unit,
) {
    private val log = ProbeLog.sub("carfw")

    private var car: Car? = null
    private var projection: CarProjectionManager? = null
    private var ux: CarUxRestrictionsManager? = null
    private var keyHandler: CarProjectionManager.ProjectionKeyEventHandler? = null

    /** True once `updateProjectionStatus` has been accepted — i.e. we are visible to the platform. */
    @Volatile
    var statusPublished: Boolean = false
        private set

    /**
     * Everything the published status actually depends on — not just the state.
     *
     * Keying on `state` alone meant the device NAME could never reach AAOS: it arrives from
     * `WirelessControlClient`'s poll, normally AFTER the state has settled, so the update carrying
     * it was skipped as a no-op and the HMI showed an unnamed device for the whole session.
     */
    private data class Published(
        val state: Int,
        val name: String?,
        val projecting: Boolean,
    )

    /** Written from the car-service callback thread and from the service scope. */
    @Volatile private var lastPublished: Published? = null

    fun connect() {
        if (!ctx.packageManager.hasSystemFeature(PackageManager.FEATURE_AUTOMOTIVE)) {
            log.w("not an automotive build — AAOS projection integration disabled")
            return
        }
        try {
            // The listener form is non-blocking. The blocking form can wait on car_service, and this
            // runs from a service started at BOOT_COMPLETED where car_service may not be up yet.
            car =
                Car.createCar(ctx, null, Car.CAR_WAIT_TIMEOUT_DO_NOT_WAIT) { c, ready ->
                    if (ready) onCarReady(c) else onCarLost()
                }
        } catch (e: Exception) {
            log.e("Car.createCar failed: $e")
        }
    }

    private fun onCarReady(c: Car) {
        log.i("car service connected")
        // The typed getCarManager(Class) overload, not the String one: Car.PROJECTION_SERVICE is
        // itself @SystemApi and absent from the public stub, and this avoids depending on the
        // service-name literal at all.
        projection =
            runCatching { c.getCarManager(CarProjectionManager::class.java) }
                .onFailure { log.e("CarProjectionManager unavailable: $it") }
                .getOrNull()
        ux =
            runCatching { c.getCarManager(CarUxRestrictionsManager::class.java) }
                .onFailure { log.e("CarUxRestrictionsManager unavailable: $it") }
                .getOrNull()
        registerKeyEvents()
        registerUxRestrictions()
        // Publish immediately so the platform's view is correct even if no state change follows.
        publish(status.state.value)
    }

    private fun onCarLost() {
        log.w("car service disconnected")
        projection = null
        ux = null
        statusPublished = false
        lastPublished = null
    }

    // ---- projection status -----------------------------------------------------------------------

    /**
     * Publish session state system-wide.
     *
     * The state vocabulary is the framework's own, and it maps onto our model exactly — which is
     * itself evidence the framework was designed for the session-vs-foreground split §1 settled on:
     *
     * | Ours | AAOS |
     * |---|---|
     * | no session | `PROJECTION_STATE_INACTIVE` |
     * | seam up, not yet keyed | `PROJECTION_STATE_ATTEMPTING` |
     * | session, surface backgrounded | `PROJECTION_STATE_ACTIVE_BACKGROUND` |
     * | session, surface foreground | `PROJECTION_STATE_ACTIVE_FOREGROUND` |
     *
     * `ACTIVE_BACKGROUND` is the one that matters: it is how AAOS is told "CarPlay is still running,
     * the driver is just looking at something else", which is precisely what the launcher tile and
     * the oemIcon flow depend on being true.
     */
    @Synchronized
    fun publish(s: SessionStatus.State) {
        val p = projection ?: return
        val state =
            when {
                s.sessionActive && s.foreground -> ProjectionStatus.PROJECTION_STATE_ACTIVE_FOREGROUND
                s.sessionActive -> ProjectionStatus.PROJECTION_STATE_ACTIVE_BACKGROUND
                s.phase == SessionStatus.Phase.LINKING -> ProjectionStatus.PROJECTION_STATE_ATTEMPTING
                else -> ProjectionStatus.PROJECTION_STATE_INACTIVE
            }
        val name = s.deviceName ?: s.deviceAddress
        val now = Published(state, name, s.sessionActive)
        if (now == lastPublished) return
        try {
            val builder =
                ProjectionStatus
                    .builder(ctx.packageName, state)
                    .setProjectionTransport(ProjectionStatus.PROJECTION_TRANSPORT_WIFI)
            if (name != null) {
                builder.addMobileDevice(
                    ProjectionStatus.MobileDevice
                        // The id is our own stable handle for the device; AAOS treats it as opaque.
                        // Deriving it from the address keeps it stable across sessions without
                        // needing a counter that would have to survive a restart.
                        .builder(s.deviceAddress.hashCode(), name)
                        .setProjecting(s.sessionActive)
                        .addTransport(ProjectionStatus.PROJECTION_TRANSPORT_WIFI)
                        .build(),
                )
            }
            p.updateProjectionStatus(builder.build())
            lastPublished = now
            if (!statusPublished) log.i("projection status published to AAOS (state=$state)")
            statusPublished = true
        } catch (e: Exception) {
            // Almost always a missing privileged permission. Logged on the transition only, or a
            // reconnect storm becomes a log storm.
            if (statusPublished) log.e("updateProjectionStatus rejected: $e")
            statusPublished = false
        }
    }

    // ---- steering-wheel keys ---------------------------------------------------------------------

    private fun registerKeyEvents() {
        val p = projection ?: return
        val handler =
            CarProjectionManager.ProjectionKeyEventHandler { event ->
                when (event) {
                    // Siri is a HOLD pair on the iOS side: the turn stays open until buttonup, and
                    // the bare one-shot requestSiri is ignored by iOS entirely. So a long press maps
                    // straight through, and a short press is synthesised as an immediate down/up.
                    CarProjectionManager.KEY_EVENT_VOICE_SEARCH_LONG_PRESS_KEY_DOWN -> onSiriDown()
                    CarProjectionManager.KEY_EVENT_VOICE_SEARCH_LONG_PRESS_KEY_UP -> onSiriUp()
                    CarProjectionManager.KEY_EVENT_VOICE_SEARCH_SHORT_PRESS_KEY_UP -> {
                        onSiriDown()
                        onSiriUp()
                    }
                    CarProjectionManager.KEY_EVENT_CALL_SHORT_PRESS_KEY_UP,
                    CarProjectionManager.KEY_EVENT_CALL_LONG_PRESS_KEY_UP,
                    -> onCallKey()
                    else -> Unit
                }
            }
        val events =
            setOf(
                CarProjectionManager.KEY_EVENT_VOICE_SEARCH_SHORT_PRESS_KEY_UP,
                CarProjectionManager.KEY_EVENT_VOICE_SEARCH_LONG_PRESS_KEY_DOWN,
                CarProjectionManager.KEY_EVENT_VOICE_SEARCH_LONG_PRESS_KEY_UP,
                CarProjectionManager.KEY_EVENT_CALL_SHORT_PRESS_KEY_UP,
                CarProjectionManager.KEY_EVENT_CALL_LONG_PRESS_KEY_UP,
            )
        try {
            p.addKeyEventHandler(events, handler)
            keyHandler = handler
            log.i("registered ${events.size} projection key events (voice + call)")
        } catch (e: Exception) {
            log.e("addKeyEventHandler rejected: $e — steering-wheel keys will not reach CarPlay")
        }
    }

    // ---- drive state -----------------------------------------------------------------------------

    private fun registerUxRestrictions() {
        val u = ux ?: return
        try {
            // Seed from the current value first: the listener fires only on CHANGE, so a service
            // started while already driving would otherwise never send the initial setLimitedUI.
            u.currentCarUxRestrictions?.let { emitRestrictions(it) }
            u.registerListener { r -> emitRestrictions(r) }
            log.i("registered CarUxRestrictions listener")
        } catch (e: Exception) {
            log.e("CarUxRestrictions registration rejected: $e")
        }
    }

    private fun emitRestrictions(r: CarUxRestrictions) {
        status.setDriveRestricted(r.isRequiresDistractionOptimization)
    }

    fun disconnect() {
        runCatching { keyHandler?.let { projection?.removeKeyEventHandler(it) } }
        runCatching { ux?.unregisterListener() }
        // Tell the platform we are gone. Without this the last published state persists and the HMI
        // keeps offering "return to CarPlay" for a session that no longer exists.
        runCatching {
            projection?.updateProjectionStatus(
                ProjectionStatus
                    .builder(ctx.packageName, ProjectionStatus.PROJECTION_STATE_INACTIVE)
                    .build(),
            )
        }
        runCatching { car?.disconnect() }
        keyHandler = null
        projection = null
        ux = null
        car = null
        statusPublished = false
        lastPublished = null
    }
}
