package com.carlink.callui

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import android.os.Looper
import androidx.test.core.app.ApplicationProvider
import com.carlink.ocbm.Ocbm
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/**
 * The call card as posted, and its buttons as wired — the part of the call UI that can be proven
 * without an iPhone ringing through the box. The platform-side CallStyle admission rule (FGS or
 * fullScreenIntent, Android 14+) lives in system_server and is NOT exercised here.
 */
@RunWith(RobolectricTestRunner::class)
class CallNotificationHostTest {
    private val ctx: Context = ApplicationProvider.getApplicationContext()
    private val nm = ctx.getSystemService(NotificationManager::class.java)

    private fun call(
        status: Int,
        dir: Int = CallState.DIRECTION_INCOMING,
    ) = CallState(status, dir, "Ada", "+15550001", "u1", "mobile", null)

    @Test
    fun `a ringing call posts an ongoing CallStyle card with a full-screen intent and both actions`() {
        val pressed = mutableListOf<Byte>()
        val host = CallNotificationHost(ctx) { pressed += it }
        host.onCallState(call(CallState.STATUS_RINGING))

        val n = shadowOf(nm).getNotification(CallNotificationHost.NOTIFICATION_ID)
        assertNotNull(n)
        assertTrue(n.flags and Notification.FLAG_ONGOING_EVENT != 0)
        assertNotNull("fullScreenIntent is what admits CallStyle without an FGS on 14+", n.fullScreenIntent)
        assertEquals(Notification.CATEGORY_CALL, n.category)
        assertEquals("Ada", n.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
        // Platform CallStyle: two system actions (decline + answer) in the template.
        assertEquals(Notification.CallStyle::class.java.name, n.extras.getString(Notification.EXTRA_TEMPLATE))
        assertEquals(2, n.actions.size)

        // Fire "Answer" through its PendingIntent: the receiver maps it to the telephony HID index.
        val answer = n.actions.first { it.title.toString() == "Answer" }
        answer.actionIntent.send()
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(listOf(Ocbm.TEL_ANSWER), pressed)
        host.release()
    }

    @Test
    fun `active then idle replaces the card with hang-up and clears it`() {
        val pressed = mutableListOf<Byte>()
        val host = CallNotificationHost(ctx) { pressed += it }
        host.onCallState(call(CallState.STATUS_RINGING))
        host.onCallState(call(CallState.STATUS_ACTIVE))
        val n = shadowOf(nm).getNotification(CallNotificationHost.NOTIFICATION_ID)
        assertEquals(1, n.actions.size)
        n.actions[0].actionIntent.send()
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(listOf(Ocbm.TEL_END), pressed)

        host.onCallState(CallState(CallState.STATUS_DISCONNECTED, 0, null, null, null, null, null))
        assertNull(shadowOf(nm).getNotification(CallNotificationHost.NOTIFICATION_ID))
        assertEquals(CallRoster.Presentation.None, host.presentation)
        host.release()
    }
}
