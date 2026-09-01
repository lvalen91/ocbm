package com.carlink.ocbm

/**
 * A minimal reader for Apple's `bplist00` binary property lists.
 *
 * It exists for ONE input: the `META_CMD` payload on `CH_METADATA`, which is the raw plist of an
 * inbound iPhone `POST /command` — `{type: "<verb>", params: {...}}`. The macOS host gets this for
 * free from `PropertyListSerialization`; Android has no equivalent, and the alternative considered
 * (scanning the bytes for the ASCII verb) is not sound — a verb name can appear inside a URL or a
 * nested string, so a scan would fire on payloads that never asked for anything.
 *
 * Deliberately partial. It decodes the types Apple's command plists actually use — dict, array,
 * ASCII/UTF-16 string, integer, real, boolean, data — and returns null for anything malformed
 * rather than guessing. UID and SET are not implemented; they do not appear in this traffic and a
 * silent wrong answer would be worse than no answer.
 *
 * **This parses data that crossed the wire from the phone.** Every read is bounds-checked against
 * the buffer, object references are validated against the object count, and container recursion is
 * depth-capped, so a truncated or hostile payload yields null instead of an exception on the OCBM
 * read thread (which would take the whole dispatch loop with it).
 */
object BinaryPlist {
    private val MAGIC = "bplist00".toByteArray(Charsets.US_ASCII)
    private const val TRAILER_LEN = 32

    /** Deep enough for `{type, params:{...}}` and then some; cheap insurance against a cyclic table. */
    private const val MAX_DEPTH = 16

    /**
     * Parse [bytes] and return the top-level object as a map, or null if it is not a well-formed
     * `bplist00` whose root is a dictionary.
     */
    fun parseDict(bytes: ByteArray): Map<String, Any?>? = runCatching { Reader(bytes).topDict() }.getOrNull()

    private class Reader(
        private val b: ByteArray,
    ) {
        private val offsetSize: Int
        private val refSize: Int
        private val count: Int
        private val topRef: Int
        private val tableOff: Long

        init {
            require(b.size >= MAGIC.size + TRAILER_LEN) { "too short" }
            require(MAGIC.indices.all { b[it] == MAGIC[it] }) { "bad magic" }
            val t = b.size - TRAILER_LEN
            offsetSize = b[t + 6].toInt() and 0xFF
            refSize = b[t + 7].toInt() and 0xFF
            require(offsetSize in 1..8 && refSize in 1..8) { "bad trailer widths" }
            val n = be(t + 8, 8)
            val top = be(t + 16, 8)
            tableOff = be(t + 24, 8)
            require(n in 1..(1 shl 24) && top < n) { "bad object count/root" }
            require(tableOff >= MAGIC.size && tableOff + n * offsetSize <= t) { "bad offset table" }
            count = n.toInt()
            topRef = top.toInt()
        }

        /** Big-endian unsigned integer of [len] bytes at [at]. */
        private fun be(
            at: Int,
            len: Int,
        ): Long {
            require(at >= 0 && len >= 0 && at + len <= b.size) { "read out of range" }
            var v = 0L
            for (i in 0 until len) v = (v shl 8) or (b[at + i].toLong() and 0xFF)
            return v
        }

        private fun offsetOf(ref: Int): Int {
            require(ref in 0 until count) { "object ref out of range" }
            val o = be((tableOff + ref.toLong() * offsetSize).toInt(), offsetSize)
            require(o >= 0 && o < b.size) { "object offset out of range" }
            return o.toInt()
        }

        @Suppress("UNCHECKED_CAST")
        fun topDict(): Map<String, Any?>? = obj(topRef, 0) as? Map<String, Any?>

        /**
         * Decode the object at [ref].
         *
         * Apple's encoding puts the type in the high nibble and, for sized types, the count in the
         * low nibble — with 0xF meaning "an integer object follows carrying the real count". The
         * split below follows that: fixed-width scalars need no count, everything else does.
         */
        private fun obj(
            ref: Int,
            depth: Int,
        ): Any? {
            require(depth <= MAX_DEPTH) { "too deep" }
            val at = offsetOf(ref)
            val marker = b[at].toInt() and 0xFF
            val hi = marker ushr 4
            val lo = marker and 0x0F
            val p = at + 1
            return when (hi) {
                0x0 -> singleton(lo)
                0x1 -> be(p, 1 shl lo) // int, 2^lo bytes, big-endian
                0x2 -> real(lo, p)
                else -> {
                    val c = Cursor(p)
                    val n = sizedCount(lo, c)
                    sized(hi, n, c.p, depth)
                }
            }
        }

        private class Cursor(
            var p: Int,
        )

        private fun singleton(lo: Int): Any? =
            when (lo) {
                0x8 -> false
                0x9 -> true
                else -> null // 0x0 null, and every unassigned low nibble
            }

        private fun real(
            lo: Int,
            p: Int,
        ): Any? =
            when (lo) {
                2 ->
                    java.lang.Float
                        .intBitsToFloat(be(p, 4).toInt())
                        .toDouble()
                3 -> java.lang.Double.longBitsToDouble(be(p, 8))
                else -> null
            }

        /** Element count for a sized type, consuming the trailing int object when [lo] is 0xF. */
        private fun sizedCount(
            lo: Int,
            c: Cursor,
        ): Int {
            if (lo != 0x0F) return lo
            val m = b[c.p].toInt() and 0xFF
            require(m ushr 4 == 0x1) { "bad count marker" }
            val n = 1 shl (m and 0x0F)
            c.p++
            val v = be(c.p, n)
            c.p += n
            require(v in 0..(1 shl 24)) { "implausible count" }
            return v.toInt()
        }

        private fun sized(
            hi: Int,
            n: Int,
            p: Int,
            depth: Int,
        ): Any? =
            when (hi) {
                0x4 -> { // data
                    require(p + n <= b.size) { "data overruns" }
                    b.copyOfRange(p, p + n)
                }
                0x5 -> { // ASCII string
                    require(p + n <= b.size) { "ascii overruns" }
                    String(b, p, n, Charsets.US_ASCII)
                }
                0x6 -> { // UTF-16BE string — n is in CHARACTERS, not bytes
                    require(p + n * 2 <= b.size) { "utf16 overruns" }
                    String(b, p, n * 2, Charsets.UTF_16BE)
                }
                0xA -> { // array
                    require(p + n * refSize <= b.size) { "array overruns" }
                    (0 until n).map { i -> obj(be(p + i * refSize, refSize).toInt(), depth + 1) }
                }
                0xD -> dict(n, p, depth)
                else -> null
            }

        /** A dict is n key refs followed by n value refs. */
        private fun dict(
            n: Int,
            p: Int,
            depth: Int,
        ): Map<String, Any?> {
            require(p + n * 2 * refSize <= b.size) { "dict overruns" }
            val out = LinkedHashMap<String, Any?>(n.coerceAtMost(64))
            for (i in 0 until n) {
                val k = obj(be(p + i * refSize, refSize).toInt(), depth + 1)
                val v = obj(be(p + (n + i) * refSize, refSize).toInt(), depth + 1)
                // A non-string key cannot be addressed by the callers here; skip rather than coerce,
                // so `d["type"]` never matches something that was not a string key.
                if (k is String) out[k] = v
            }
            return out
        }
    }
}
