package zeno.gmccpa

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM truth table for [UsbRequestPolicy.mayRequest] (plan v5.4 OD-2): all 12 combinations of
 * parked null/false/true x session live/not x requests 0/>=1. Required: live && parked != true -> false.
 */
class UsbRequestPolicyTest {

    private fun expected(live: Boolean, parked: Boolean?, n: Int): Boolean = when {
        !live -> true                       // no session: today's behaviour, always request
        parked != true -> false             // live, gear not confirmed PARK (incl. unknown): defer
        else -> n == 0                      // live + PARK: once per device instance while live
    }

    @Test
    fun allTwelveCombinations() {
        var cases = 0
        for (live in listOf(false, true)) for (parked in listOf<Boolean?>(null, false, true)) for (n in listOf(0, 1)) {
            assertEquals("live=$live parked=$parked n=$n", expected(live, parked, n),
                UsbRequestPolicy.mayRequest(live, parked, n))
            cases++
        }
        assertEquals(12, cases)
    }

    @Test
    fun liveAndNotParkedNeverRequests() {
        for (parked in listOf<Boolean?>(null, false)) for (n in listOf(0, 1, 5))
            assertEquals(false, UsbRequestPolicy.mayRequest(true, parked, n))
    }

    @Test
    fun liveParkedOnlyFirstRequestPerInstance() {
        assertEquals(true, UsbRequestPolicy.mayRequest(true, true, 0))
        assertEquals(false, UsbRequestPolicy.mayRequest(true, true, 1))
        assertEquals(false, UsbRequestPolicy.mayRequest(true, true, 7))
    }

    @Test
    fun deferredWhyIsCaseTrue() {
        for (live in listOf(false, true)) for (parked in listOf<Boolean?>(null, false, true)) for (n in listOf(0, 1)) {
            val why = UsbRequestPolicy.deferredWhy(live, parked, n)
            val want = when {
                UsbRequestPolicy.mayRequest(live, parked, n) -> null
                parked == true -> UsbRequestPolicy.ASKED_WHY       // in Park: deferred only because already asked
                else -> UsbRequestPolicy.DEFERRED_WHY
            }
            assertEquals("live=$live parked=$parked n=$n", want, why)
        }
        assertEquals(UsbRequestPolicy.ASKED_WHY, UsbRequestPolicy.deferredWhy(true, true, 1))
    }

    @Test
    fun noSessionAlwaysRequests() {
        for (parked in listOf<Boolean?>(null, false, true)) for (n in listOf(0, 1, 9))
            assertEquals(true, UsbRequestPolicy.mayRequest(false, parked, n))
    }
}
