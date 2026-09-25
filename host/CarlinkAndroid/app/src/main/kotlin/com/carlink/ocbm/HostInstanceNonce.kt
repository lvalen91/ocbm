package com.carlink.ocbm

import java.util.concurrent.ThreadLocalRandom

/**
 * The host INSTANCE NONCE carried in every `CT_HELLO` (`Ocbm.CT_HELLO`, the trailing u32).
 *
 * It is how the box tells one host session from another: the same value on a reattach means
 * "the same host, warm-reuse", a different value means "the previous host is gone, re-arm". Zero is
 * the wire's "not supplied", so a fresh nonce is never zero — and, for a rotation, never the value
 * it replaces, or the box would read the clean-slate reset as a mere USB blip.
 */
object HostInstanceNonce {
    /** A nonce that is neither 0 nor [previous]. */
    fun fresh(previous: Int = 0): Int {
        while (true) {
            val n = ThreadLocalRandom.current().nextInt()
            if (n != 0 && n != previous) return n
        }
    }
}
