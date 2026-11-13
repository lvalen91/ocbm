package zeno.gmccpa

/**
 * When the box re-claim may raise the USB permission dialog (2026-10-06).
 *
 * A replug of the adapter may need a user tap on this unit: USB permission is per device instance
 * unless the persistent "always open" grant holds (docs/host/01). The manifest attach ACTIVITY route
 * never runs (GM routes attach to an absent fixed handler); the attach broadcast's auto-claim
 * (plan v5.9 D6, AdapterAttachReceiver) never asks at all — it skips without a grant. So a re-claim
 * that DOES ask, under live CarPlay, would put a system dialog over the projection, and the driver
 * may be moving. The rule:
 *
 *  - no CarPlay session live: always (the claim loop re-raises every 30 s, as it always has);
 *  - session live: only when the vehicle is CONFIRMED in Park and this device instance has not been
 *    asked yet (the count is per instance, so a prompt raised before the session went live counts).
 *    Gear unknown (`parked == null`) is NOT parked. The current session does not need the box; the
 *    re-claim is for the NEXT control connection's /auth-setup, so deferring costs nothing until that
 *    reconnect. A deferral ends when the session ends, or when the gear reads PARK if this instance
 *    has not been asked yet; an instance already asked waits for the session end (or a replug).
 *
 * Pure, with no Android dependency, so the whole truth table is a host JVM unit test
 * (`app/src/test/.../UsbRequestPolicyTest.kt`): the bench cannot present a non-PARK gear (user build,
 * no VHAL injection), so that test is the bench proof of the gate logic.
 */
object UsbRequestPolicy {
    fun mayRequest(sessionLive: Boolean, parked: Boolean?, requestsThisInstance: Int): Boolean =
        !sessionLive || (parked == true && requestsThisInstance == 0)

    /**
     * The reason logged with a deferral, for the inputs [mayRequest] returned false on; null when it
     * returned true. Case-true (2026-10-06): live AND in Park defers only because this instance was
     * already asked, and saying "not confirmed in Park" there would be false.
     */
    fun deferredWhy(sessionLive: Boolean, parked: Boolean?, requestsThisInstance: Int): String? = when {
        mayRequest(sessionLive, parked, requestsThisInstance) -> null
        parked == true -> ASKED_WHY
        else -> DEFERRED_WHY
    }

    /** Live, gear not confirmed PARK (incl. unknown). */
    const val DEFERRED_WHY = "CarPlay live, vehicle not confirmed in Park"
    /** Live and in Park, but this device instance was already asked. */
    const val ASKED_WHY = "CarPlay live; the USB prompt for this adapter was already raised once since it was plugged in — asked again when CarPlay ends"
}
