package zeno.gmccpa.av

/**
 * A minimal Apple binary-plist reader, enough for the `/command` dictionaries iOS sends.
 *
 * ## Why this exists
 * The `META_CMD` records on the `:9004` seam are `bplist00` blobs — every inbound `POST /command`
 * the phone sends, forwarded verbatim by the receiver. Android has no public binary-plist API
 * (`NSKeyedUnarchiver` has no equivalent, and `android.util` offers nothing), and
 * `tools/build_apk.sh` compiles against `android.jar` + `kotlin-stdlib` only — no AndroidX, no
 * Gradle dependency resolution — so a third-party library is not an option either.
 *
 * ## Scope, deliberately small
 * Handles exactly the shapes a `/command` payload uses: dictionaries, ASCII and UTF-16 strings,
 * integers, booleans, reals and data; sets decode as arrays. No dates, no UIDs, no `bplist15`/keyed archives.
 * Anything unrecognised decodes to `null` rather than throwing, because this parses attacker-
 * adjacent bytes on a session thread: a malformed plist must cost one dropped record, never the
 * connection.
 *
 * Format reference: the trailer is the last 32 bytes — `[6 unused][offsetIntSize u8]
 * [objectRefSize u8][numObjects u64 BE][topObject u64 BE][offsetTableOffset u64 BE]`; object type is
 * the high nibble of the marker byte, and a low nibble of `0xF` means the real count follows as an
 * integer object.
 */
object BPlist {

    private const val HEADER = "bplist00"
    /** Cheap sanity bound: `/command` payloads are hundreds of bytes; artwork does not come this way. */
    private const val MAX_OBJECTS = 4096

    /** Decode a binary plist into Kotlin types, or null if it is not one / is malformed. */
    fun parse(b: ByteArray): Any? = runCatching { Reader(b).parse() }.getOrNull()

    /** `dict["a"]["b"]` as a String, or null. Convenience for the shallow `/command` shape. */
    fun str(root: Any?, vararg path: String): String? = dig(root, path) as? String

    /** `dict["a"]["b"]` as a Long, or null. */
    fun int(root: Any?, vararg path: String): Long? = when (val v = dig(root, path)) {
        is Long -> v
        is Int -> v.toLong()
        else -> null
    }

    @Suppress("UNCHECKED_CAST")
    private fun dig(root: Any?, path: Array<out String>): Any? {
        var cur = root
        for (k in path) {
            val m = cur as? Map<String, Any?> ?: return null
            cur = m[k] ?: return null
        }
        return cur
    }

    private class Reader(private val b: ByteArray) {
        private var offsetIntSize = 0
        private var objectRefSize = 0
        private var numObjects = 0
        private var topObject = 0
        private var offsetTableOffset = 0
        /** Total object expansions one parse may perform. Depth alone does not bound work: a dict whose
         *  entries all reference the same dict costs len^depth expansions, and nothing is memoised.
         *  Exhausting it throws; parse()'s runCatching turns that into the documented null. */
        private var budget = MAX_OBJECTS * 4

        fun parse(): Any? {
            if (b.size < 40 || String(b, 0, 8, Charsets.US_ASCII) != HEADER) return null
            val t = b.size - 32
            offsetIntSize = b[t + 6].toInt() and 0xFF
            objectRefSize = b[t + 7].toInt() and 0xFF
            numObjects = be(t + 8, 8).toInt()
            topObject = be(t + 16, 8).toInt()
            offsetTableOffset = be(t + 24, 8).toInt()
            if (offsetIntSize !in 1..8 || objectRefSize !in 1..8) return null
            if (numObjects !in 1..MAX_OBJECTS || topObject >= numObjects) return null
            if (offsetTableOffset + numObjects * offsetIntSize > b.size) return null
            // depth-bounded: a cyclic ref table would otherwise recurse forever
            return obj(topObject, 0)
        }

        private fun be(off: Int, n: Int): Long {
            var v = 0L
            for (i in 0 until n) v = (v shl 8) or (b[off + i].toLong() and 0xFF)
            return v
        }

        private fun offsetOf(i: Int) = be(offsetTableOffset + i * offsetIntSize, offsetIntSize).toInt()

        private fun obj(index: Int, depth: Int): Any? {
            if (depth > 16 || index < 0 || index >= numObjects) return null
            if (--budget < 0) throw IllegalStateException("bplist: expansion budget exhausted (cyclic or repeated refs)")
            var p = offsetOf(index)
            if (p < 0 || p >= b.size) return null
            val marker = b[p].toInt() and 0xFF
            val type = marker ushr 4
            val n = marker and 0x0F
            p++
            return when (type) {
                0x0 -> when (n) { 0x8 -> false; 0x9 -> true; else -> null }   // 0x0 = null
                0x1 -> be(p, 1 shl n)                                          // int, 2^n bytes
                0x2 -> when (n) {                                              // real
                    2 -> java.lang.Float.intBitsToFloat(be(p, 4).toInt()).toDouble()
                    3 -> java.lang.Double.longBitsToDouble(be(p, 8))
                    else -> null
                }
                0x4 -> { val (len, q) = count(n, p); b.copyOfRange(q, q + len) }   // data
                0x5 -> { val (len, q) = count(n, p); String(b, q, len, Charsets.US_ASCII) }
                0x6 -> { val (len, q) = count(n, p); String(b, q, len * 2, Charsets.UTF_16BE) }
                0xD -> {                                                        // dict
                    val (len, q) = count(n, p)
                    // The ref table must fit in the buffer BEFORE the map is sized from `len`: HashMap(len)
                    // allocates tableSizeFor(len) slots on the first put, so a crafted count is a ~4-8 GB
                    // allocation (OutOfMemoryError) rather than the AIOOBE every other malformed shape hits.
                    if (len < 0 || q + 2L * len * objectRefSize > b.size) throw IllegalArgumentException("bplist: dict ref table exceeds buffer")
                    val out = LinkedHashMap<String, Any?>(len)
                    for (i in 0 until len) {
                        val kRef = be(q + i * objectRefSize, objectRefSize).toInt()
                        val vRef = be(q + (len + i) * objectRefSize, objectRefSize).toInt()
                        val k = obj(kRef, depth + 1) as? String ?: continue
                        out[k] = obj(vRef, depth + 1)
                    }
                    out
                }
                0xA, 0xC -> {                                                   // array / set
                    val (len, q) = count(n, p)
                    (0 until len).map { obj(be(q + it * objectRefSize, objectRefSize).toInt(), depth + 1) }
                }
                else -> null
            }
        }

        /** A low nibble of 0xF means the count is the NEXT object (an int), not the nibble itself. */
        private fun count(n: Int, p: Int): Pair<Int, Int> {
            if (n != 0x0F) return n to p
            val m = b[p].toInt() and 0xFF
            if (m ushr 4 != 0x1) return 0 to p + 1
            val bytes = 1 shl (m and 0x0F)
            return be(p + 1, bytes).toInt() to (p + 1 + bytes)
        }
    }
}
