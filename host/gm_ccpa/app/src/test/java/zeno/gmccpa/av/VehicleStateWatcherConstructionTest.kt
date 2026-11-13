package zeno.gmccpa.av

import java.io.DataInputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * D-F3 (2026-10-08). The gate's runtime classpath has NO android.car (tools/test.sh), which is the
 * head unit without the Car API. Before the fix the three Car callbacks were plain field
 * initialisers — fields typed by anonymous classes implementing android.car interfaces, assigned in
 * the constructor — so constructing the watcher on such a unit threw NoClassDefFoundError from
 * OUTSIDE start()'s try, and `SessionCore.toggleCarPlayRx` never reached `rx.start()`. Now they are
 * `by lazy` (fields of type kotlin.Lazy, bodies run inside start()'s try).
 *
 * Checked on the class FILE, not through reflection: `Class.getDeclaredField` resolves every field
 * type, and the watcher's `ctx: Context` field alone makes that throw here. The field table is
 * parsed directly instead, which is also what proves the point — a field descriptor is inert, an
 * initialiser is not.
 */
class VehicleStateWatcherConstructionTest {

    private fun fields(cls: String): List<Pair<String, String>> {
        val bytes = javaClass.classLoader!!.getResourceAsStream(cls.replace('.', '/') + ".class")!!.readBytes()
        val d = DataInputStream(bytes.inputStream())
        assertEquals(0xCAFEBABE.toInt(), d.readInt()); d.readUnsignedShort(); d.readUnsignedShort()
        val n = d.readUnsignedShort()
        val utf8 = arrayOfNulls<String>(n)
        var i = 1
        while (i < n) {
            when (val tag = d.readUnsignedByte()) {
                1 -> utf8[i] = d.readUTF()
                7, 8, 16, 19, 20 -> d.readUnsignedShort()
                15 -> { d.readUnsignedByte(); d.readUnsignedShort() }
                3, 4, 9, 10, 11, 12, 17, 18 -> d.readInt()
                5, 6 -> { d.readLong(); i++ }
                else -> throw IllegalStateException("cp tag $tag")
            }
            i++
        }
        d.readUnsignedShort(); d.readUnsignedShort(); d.readUnsignedShort()
        repeat(d.readUnsignedShort()) { d.readUnsignedShort() }
        val out = ArrayList<Pair<String, String>>()
        repeat(d.readUnsignedShort()) {
            d.readUnsignedShort()
            val name = utf8[d.readUnsignedShort()]!!; val desc = utf8[d.readUnsignedShort()]!!
            repeat(d.readUnsignedShort()) { d.readUnsignedShort(); d.skipBytes(d.readInt()) }
            out.add(name to desc)
        }
        return out
    }

    @Test fun carCallbacksAreLazyDelegates_noEagerAnonymousCallbackField() {
        val f = fields("zeno.gmccpa.av.VehicleStateWatcher").toMap()
        for (n in listOf("gearCb\$delegate", "nightCb\$delegate", "uxrCb\$delegate")) {
            assertEquals(n, "Lkotlin/Lazy;", f[n])
        }
        // The pre-fix shape: `gearCb: Lzeno/gmccpa/av/VehicleStateWatcher$gearCb$1;` etc., assigned
        // in <init>. No field may be typed by an inner class of the watcher any more.
        val eager = f.filterValues { it.startsWith("Lzeno/gmccpa/av/VehicleStateWatcher$") && !it.endsWith("\$Companion;") }
        assertTrue("eager callback fields: $eager", eager.isEmpty())
        // Sanity: the parser read the real table (the context and the manager slots are there).
        assertEquals("Landroid/content/Context;", f["ctx"])
        assertTrue(f.containsKey("props"))
    }
}
