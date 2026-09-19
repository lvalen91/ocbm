package com.carlink.callui

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CallRosterTest {
    private fun rec(
        status: Int,
        dir: Int = 1,
        uuid: String? = "u1",
        name: String? = "Ada",
        num: String? = "+15550001",
    ): CallState =
        CallState.fromJson(
            JSONObject().apply {
                put("kind", "callState")
                put("status", status)
                put("direction", dir)
                uuid?.let { put("callUuid", it) }
                name?.let { put("displayName", it) }
                num?.let { put("remoteId", it) }
            },
        )!!

    @Test
    fun parseIsVerbatimAndStatusIsRequired() {
        assertNull(CallState.fromJson(JSONObject("""{"kind":"callState","label":"mobile"}""")))
        val s = CallState.fromJson(JSONObject("""{"status":2,"direction":1,"remoteId":"+1","displayName":"","label":"mobile","startTimestamp":12}"""))!!
        assertEquals(2, s.status)
        assertNull("empty string is absent", s.displayName)
        assertEquals("+1", s.displayLine)
        assertEquals("mobile", s.label)
        assertEquals(12L, s.startTimestamp)
        assertTrue(s.isRingingIncoming)
    }

    @Test
    fun ringingThenActiveThenIdle() {
        val r = CallRoster()
        val p1 = r.update(rec(CallState.STATUS_RINGING), 1000)
        assertTrue(p1 is CallRoster.Presentation.Incoming)
        val p2 = r.update(rec(CallState.STATUS_ACTIVE), 2000)
        assertEquals(CallRoster.Presentation.Ongoing(rec(CallState.STATUS_ACTIVE), 2000), p2)
        // Idle is a bare status 0 with no identity: everything clears.
        val p3 = r.update(rec(CallState.STATUS_DISCONNECTED, uuid = null, name = null, num = null), 3000)
        assertEquals(CallRoster.Presentation.None, p3)
        assertEquals(0, r.size)
    }

    @Test
    fun secondIncomingWinsOverActiveAndEndingItRestoresOngoing() {
        val r = CallRoster()
        r.update(rec(CallState.STATUS_ACTIVE, uuid = "a"), 1000)
        val p = r.update(rec(CallState.STATUS_RINGING, uuid = "b", name = "Bob"), 2000)
        assertEquals("Bob", (p as CallRoster.Presentation.Incoming).call.displayLine)
        val back = r.update(rec(CallState.STATUS_DISCONNECTED, uuid = "b", name = "Bob"), 3000)
        val o = back as CallRoster.Presentation.Ongoing
        assertEquals("a", o.call.key)
        assertEquals("since stamp is the FIRST time the call was seen live", 1000L, o.sinceMs)
    }

    @Test
    fun outgoingRingingIsNotAnswerable() {
        val r = CallRoster()
        val p = r.update(rec(CallState.STATUS_RINGING, dir = CallState.DIRECTION_OUTGOING), 0)
        assertTrue(p is CallRoster.Presentation.Ongoing)
    }

    @Test
    fun placeholderNameWhenIosSendsNothing() {
        val s = rec(CallState.STATUS_RINGING, name = null, num = null)
        assertEquals("Unknown caller", s.displayLine)
    }
}
