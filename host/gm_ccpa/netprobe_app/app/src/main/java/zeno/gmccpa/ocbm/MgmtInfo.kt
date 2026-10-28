package zeno.gmccpa.ocbm

/**
 * Reads the adapter's Bluetooth name out of the MGMT_INFO JSON (ocbmd `mgmt_info`, ccpa/ocbmd
 * src/main.rs: `"name":"CarLink-xxxx"`, built by `bt_name_from`). Pure (no android.*, no org.json —
 * android.jar's org.json is a stub on the host JVM), so the host JUnit tests run this exact code.
 *
 * Only the TOP-LEVEL `name` key counts: `host_name` is a different key, and a `"name"` that appears
 * inside some other string value is escaped there and is never read as a key. The object also carries
 * `bt_mac`; that is deliberately never read here, so it cannot reach the screen.
 */
object MgmtInfo {
    /** The longest name shown. ocbmd's own names are 12 chars; anything longer is cut, not trusted. */
    const val MAX_NAME = 64

    /**
     * Splits [json] into log-sized lines without ever cutting inside a quoted string: a break is
     * only taken right after a `,` that lies outside a string, so a MAC in `devices` or the serial
     * can never straddle two logcat lines (E-F2: the export redactor works line by line, and a
     * value split across lines matches no rule on either half). Chunks are at most [max] chars
     * unless a single element is longer, in which case that element is one over-long chunk.
     * Concatenating the result reproduces [json] exactly. [max] defaults to well under liblog's
     * ~4 KB payload cap so the line is not truncated by logd either.
     */
    fun logChunks(json: String, max: Int = LOG_CHUNK_MAX): List<String> {
        if (json.length <= max) return listOf(json)
        val out = ArrayList<String>()
        var start = 0
        var lastComma = -1
        var inString = false
        var i = 0
        val n = json.length
        while (i < n) {
            val c = json[i]
            if (inString) {
                if (c == '\\') i++ else if (c == '"') inString = false
            } else if (c == '"') {
                inString = true
            } else if (c == ',') {
                lastComma = i
            }
            if (i - start + 1 >= max && lastComma >= start) {
                out.add(json.substring(start, lastComma + 1))
                start = lastComma + 1
                lastComma = -1
            }
            i++
        }
        if (start < n) out.add(json.substring(start))
        return out
    }

    /** Below liblog's payload cap (~4 KB) with room for the `[ocbm ]`/indent prefixes ProbeLog adds. */
    const val LOG_CHUNK_MAX = 3000

    /** The adapter's Bluetooth name, or null when [json] has no usable top-level `name` string. */
    fun btName(json: String?): String? {
        if (json.isNullOrEmpty()) return null
        val raw = topLevelString(json, "name") ?: return null
        // Display-safe: control characters (including newlines) dropped, whitespace trimmed, capped.
        val clean = raw.filter { !it.isISOControl() }.trim()
        if (clean.isEmpty()) return null
        return if (clean.length > MAX_NAME) clean.substring(0, MAX_NAME) else clean
    }

    /**
     * The string value of [key] in the outermost object of [json], or null if the key is absent, its
     * value is not a string, or the text is malformed before the key is reached. A minimal scanner:
     * depth is tracked over `{}`/`[]` outside strings, and only a string read at depth 1 in key
     * position (followed by `:`) is compared with [key].
     */
    internal fun topLevelString(json: String, key: String): String? {
        var i = 0
        var depth = 0
        var expectKey = false
        val n = json.length
        while (i < n) {
            val c = json[i]
            when {
                c == '{' -> { depth++; expectKey = depth == 1; i++ }
                c == '[' -> { depth++; expectKey = false; i++ }
                c == '}' || c == ']' -> { depth--; if (depth < 0) return null; expectKey = false; i++ }
                c == ',' -> { expectKey = depth == 1; i++ }
                c == '"' -> {
                    val (s, end) = readString(json, i) ?: return null
                    i = end
                    if (depth == 1 && expectKey) {
                        expectKey = false
                        var j = i
                        while (j < n && json[j].isWhitespace()) j++
                        if (j >= n || json[j] != ':') return null
                        j++
                        while (j < n && json[j].isWhitespace()) j++
                        if (s == key) {
                            if (j >= n || json[j] != '"') return null
                            return readString(json, j)?.first
                        }
                        i = j
                    }
                }
                else -> i++
            }
        }
        return null
    }

    /** Reads the JSON string starting at the quote at [start]; returns (decoded value, index past it). */
    private fun readString(s: String, start: Int): Pair<String, Int>? {
        val sb = StringBuilder()
        var i = start + 1
        while (i < s.length) {
            val c = s[i]
            when (c) {
                '"' -> return sb.toString() to (i + 1)
                '\\' -> {
                    if (i + 1 >= s.length) return null
                    when (val e = s[i + 1]) {
                        'u' -> {
                            if (i + 6 > s.length) return null
                            val cp = s.substring(i + 2, i + 6).toIntOrNull(16) ?: return null
                            sb.append(cp.toChar()); i += 6
                        }
                        'n' -> { sb.append('\n'); i += 2 }
                        't' -> { sb.append('\t'); i += 2 }
                        'r' -> { sb.append('\r'); i += 2 }
                        'b' -> { sb.append('\b'); i += 2 }
                        'f' -> { sb.append('\u000C'); i += 2 }
                        else -> { sb.append(e); i += 2 }   // \" \\ \/
                    }
                }
                else -> { sb.append(c); i++ }
            }
        }
        return null   // unterminated
    }
}
