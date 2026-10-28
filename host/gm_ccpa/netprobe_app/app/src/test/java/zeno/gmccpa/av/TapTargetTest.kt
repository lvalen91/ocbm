package zeno.gmccpa.av

import java.io.File
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * D-F1 (2026-10-08): the notification's and the media card's PendingIntent keys never collide. The
 * request-code half is also enforced textually by `tools/pending_intent_lint.py` in the gate; this
 * test holds the constants and that the sources use them (the action is not something a request-code
 * lint can see).
 */
class TapTargetTest {
    @Test fun requestCodesDiffer() = assertNotEquals(TapTarget.NOTIFICATION_REQUEST, TapTarget.MEDIA_CARD_REQUEST)

    @Test fun mediaCardActionIsAppSpecificAndNonEmpty() {
        assertTrue(TapTarget.MEDIA_CARD_ACTION.startsWith("zeno.gmccpa."))
        assertFalse(TapTarget.MEDIA_CARD_ACTION.isBlank())
    }

    /** Resolved by walking up from the working directory; a missing source is a failure, never a skip. */
    private fun mainSource(rel: String): File {
        var d: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (d != null) {
            for (prefix in listOf("", "host/gm_ccpa/")) {
                val f = File(d, prefix + "netprobe_app/app/src/main/java/zeno/gmccpa/" + rel)
                if (f.isFile) return f
            }
            d = d.parentFile
        }
        throw AssertionError("$rel not found above ${System.getProperty("user.dir")}")
    }

    @Test fun bothSitesUseTheConstants_andTheCardSetsItsAction() {
        val mbs = mainSource("av/CarPlayMediaBrowserService.kt").readText()
        assertTrue(mbs.contains("PendingIntent.getActivity(this, TapTarget.MEDIA_CARD_REQUEST,"))
        assertTrue(mbs.contains("setAction(TapTarget.MEDIA_CARD_ACTION)"))
        val css = mainSource("av/CarPlaySessionService.kt").readText()
        assertTrue(css.contains("PendingIntent.getActivity(ctx, TapTarget.NOTIFICATION_REQUEST,"))
    }
}
