package zeno.gmccpa.pair

import java.io.ByteArrayOutputStream

/**
 * TLV8 — the Apple/HomeKit type-length-value codec used by pair-setup, pair-verify and MFi-SAP.
 *
 * A port of `ccpa_custom/crates/vendor/pairing/src/tlv.rs`, not a reimplementation. The one rule
 * that is easy to get wrong: a value longer than 255 bytes is split into consecutive fragments **of
 * the same type**, every fragment but the last exactly 255 bytes, and a decoder must coalesce every
 * run of consecutive same-type items back into one value (the C `TLV8CopyCoalesced` behaviour).
 * Distinct values of the same type are separated by a zero-length `SEPARATOR` (0xFF), whose
 * different type byte breaks the run.
 */
object Tlv8 {

    const val MAX_FRAGMENT = 255

    // Types, from pairing/src/crypto.rs `tlv_type`. THIS TABLE IS A COMPLETE MIRROR OF THE REFERENCE
    // SPEC, not a list of what this file happens to use — several entries are unreferenced today.
    // Kept whole deliberately: a gappy enum invites someone to re-add a missing type with a wrong
    // value, which is a silent wire fault. Only in-use today: SALT, PUBLIC_KEY, PROOF, STATE, ERROR.
    const val METHOD: Int = 0x00
    const val IDENTIFIER: Int = 0x01
    const val SALT: Int = 0x02
    const val PUBLIC_KEY: Int = 0x03
    const val PROOF: Int = 0x04
    const val ENCRYPTED_DATA: Int = 0x05
    const val STATE: Int = 0x06
    const val ERROR: Int = 0x07
    const val RETRY_DELAY: Int = 0x08
    const val CERTIFICATE: Int = 0x09
    const val SIGNATURE: Int = 0x0A
    const val PERMISSIONS: Int = 0x0B
    const val FLAGS: Int = 0x13
    const val SEPARATOR: Int = 0xFF

    // Error codes (kTLVError_*).
    const val ERR_UNKNOWN: Byte = 0x01
    const val ERR_AUTHENTICATION: Byte = 0x02
    const val ERR_BACKOFF: Byte = 0x03
    const val ERR_MAX_PEERS: Byte = 0x04
    const val ERR_MAX_TRIES: Byte = 0x05
    const val ERR_BUSY: Byte = 0x07

    fun encode(items: List<Pair<Int, ByteArray>>): ByteArray {
        val out = ByteArrayOutputStream()
        for ((ty, value) in items) {
            if (value.isEmpty()) { out.write(ty); out.write(0); continue }
            var off = 0
            while (off < value.size) {
                val n = minOf(MAX_FRAGMENT, value.size - off)
                out.write(ty); out.write(n)
                out.write(value, off, n)
                off += n
            }
        }
        return out.toByteArray()
    }

    /** Coalescing decode. Returns the first value for each type, runs merged. */
    fun decode(buf: ByteArray): Map<Int, ByteArray> {
        val out = LinkedHashMap<Int, ByteArrayOutputStream>()
        var i = 0
        var lastType = -1
        while (i + 1 < buf.size) {
            val ty = buf[i].toInt() and 0xFF
            val len = buf[i + 1].toInt() and 0xFF
            if (i + 2 + len > buf.size) break     // truncated; take what we have
            val slice = buf.copyOfRange(i + 2, i + 2 + len)
            // HAP TLV8: only a CONSECUTIVE run of the same type is one fragmented value. A repeat of a
            // type after a different type intervened is a separate item — keep the FIRST value (do not
            // append), or a list separated by 0xFF decodes as one corrupted concatenation.
            if (ty == lastType && out.containsKey(ty)) out[ty]!!.write(slice)
            else if (!out.containsKey(ty)) out[ty] = ByteArrayOutputStream().apply { write(slice) }
            // else: non-consecutive repeat of an already-seen type — ignore, first value wins.
            lastType = ty
            i += 2 + len
        }
        return out.mapValues { it.value.toByteArray() }
    }

    fun state(m: Map<Int, ByteArray>): Int = m[STATE]?.firstOrNull()?.toInt() ?: -1

    fun error(state: Int, code: Byte): ByteArray =
        encode(listOf(STATE to byteArrayOf(state.toByte()), ERROR to byteArrayOf(code)))
}
