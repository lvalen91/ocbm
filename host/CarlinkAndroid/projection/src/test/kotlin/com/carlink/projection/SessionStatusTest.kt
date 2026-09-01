package com.carlink.projection

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The session-vs-foreground split, which is the app's central structural claim.
 *
 * If `sessionActive` ever became false merely because the surface went away, the launcher tile
 * would route to the device screen instead of bringing projection forward, and AAOS would be told
 * projection had stopped — the exact head-unit bug where returning to CarPlay costs a full
 * re-handshake.
 */
class SessionStatusTest {
    @Test
    fun `phase advances seam then key then render`() {
        val s = SessionStatus()
        assertEquals(SessionStatus.Phase.IDLE, s.state.value.phase)

        s.setVideoSeamUp(true)
        assertEquals(SessionStatus.Phase.LINKING, s.state.value.phase)

        // The first stream key is the earliest proof on the wire of a paired, keyed session.
        s.setKeyed(true)
        assertEquals(SessionStatus.Phase.KEYED, s.state.value.phase)

        s.setRendering(true)
        assertEquals(SessionStatus.Phase.STREAMING, s.state.value.phase)
    }

    @Test
    fun `a session stays active while backgrounded`() {
        val s = SessionStatus()
        s.setVideoSeamUp(true)
        s.setKeyed(true)
        s.setRendering(true)
        s.setForeground(true)
        assertTrue(s.state.value.sessionActive)

        // Driver opens another app: the surface goes, the session does not.
        s.setForeground(false)
        s.setRendering(false)
        assertTrue("losing the surface must not end the session", s.state.value.sessionActive)
        assertEquals(SessionStatus.Phase.KEYED, s.state.value.phase)
    }

    @Test
    fun `audio alone keeps the session linking`() {
        // Music with no surface is the normal backgrounded case, and must not read as idle.
        val s = SessionStatus()
        s.setAudioSeamUp(true)
        assertEquals(SessionStatus.Phase.LINKING, s.state.value.phase)
    }

    @Test
    fun `sessionEnded clears stack state but not our own foreground fact`() {
        val s = SessionStatus()
        s.setForeground(true)
        s.setKeyed(true)
        s.setRendering(true)
        s.setDevice("AA:BB:CC:DD:EE:FF", "iPhone")

        s.sessionEnded()

        assertEquals(SessionStatus.Phase.IDLE, s.state.value.phase)
        assertFalse(s.state.value.sessionActive)
        assertEquals(null, s.state.value.deviceAddress)
        // The activity's visibility is a fact about OUR process and is unaffected by the phone
        // going away. Clearing it would make the tile think projection was backgrounded while it
        // is on screen showing the disconnected state.
        assertTrue("foreground is UI-owned, not stack-owned", s.state.value.foreground)
    }

    @Test
    fun `losing the key drops out of the active states`() {
        val s = SessionStatus()
        s.setVideoSeamUp(true)
        s.setKeyed(true)
        assertTrue(s.state.value.sessionActive)

        s.setKeyed(false)
        assertFalse(s.state.value.sessionActive)
        // The seam is still up, so this is LINKING — a session being set up — not IDLE.
        assertEquals(SessionStatus.Phase.LINKING, s.state.value.phase)
    }
}
