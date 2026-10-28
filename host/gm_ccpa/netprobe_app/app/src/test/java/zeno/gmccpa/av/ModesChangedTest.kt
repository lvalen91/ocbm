package zeno.gmccpa.av

import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A11's decode over the stored capture (docs/11): `docs/ops/captures/2026-07-24_carplay_cmd_capture.bin`
 * is u32-LE length-prefixed binary plists; its 35 `modesChanged` frames carry `speechMode -1` and
 * three app states. Plus synthetic maps for the 1/2 speech modes and the malformed shapes.
 */
class ModesChangedTest {
    /** The ccpa_custom checkout root, found by walking up from the working directory. */
    private fun captureFile(): File {
        var d: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (d != null) {
            val f = File(d, "docs/ops/captures/2026-07-24_carplay_cmd_capture.bin")
            if (f.isFile) return f
            d = d.parentFile
        }
        throw AssertionError("capture file not found above ${System.getProperty("user.dir")}")
    }

    private fun frames(): List<ByteArray> {
        val b = ByteBuffer.wrap(captureFile().readBytes()).order(ByteOrder.LITTLE_ENDIAN)
        val out = ArrayList<ByteArray>()
        while (b.remaining() >= 4) {
            val len = b.int
            assertTrue("frame length $len", len in 1..b.remaining())
            val f = ByteArray(len); b.get(f); out.add(f)
        }
        return out
    }

    @Test fun capture_all35ModesChangedFramesDecode() {
        val all = frames().map { BPlist.parse(it) }
        val modes = all.filter { (it as? Map<*, *>)?.get("type") == "modesChanged" }
        assertEquals(35, modes.size)
        for (root in modes) {
            val m = ModesChanged.decode(root)
            assertNotNull(root.toString(), m)
            assertEquals(-1L, m!!.speechMode)
        }
        // Frames of another type do not decode as modes.
        for (root in all - modes.toSet()) assertNull(ModesChanged.decode(root))
    }

    @Test fun syntheticSpeechModesAndEntities() {
        fun state(id: Long, entity: Long, speech: Long? = null): Map<String, Any?> =
            if (speech == null) mapOf("appStateID" to id, "entity" to entity)
            else mapOf("appStateID" to id, "entity" to entity, "speechMode" to speech)
        val root = mapOf("type" to "modesChanged", "params" to mapOf("appStates" to listOf(
            state(1L, 2L, 2L), state(2L, 1L), state(3L, 0L), "junk", mapOf("entity" to 9L))))
        val m = ModesChanged.decode(root)!!
        assertEquals(2L, m.speechMode); assertEquals(2L, m.speechEntity)
        assertEquals(1L, m.phoneEntity); assertEquals(0L, m.turnsEntity)
        val none = ModesChanged.decode(mapOf("params" to mapOf("appStates" to listOf(state(1L, 0L)))))!!
        assertEquals(-1L, none.speechMode)
        assertNull(ModesChanged.decode(null))
        assertNull(ModesChanged.decode(mapOf("params" to "x")))
        assertNull(ModesChanged.decode(mapOf("params" to mapOf("appStates" to "x"))))
    }
}
