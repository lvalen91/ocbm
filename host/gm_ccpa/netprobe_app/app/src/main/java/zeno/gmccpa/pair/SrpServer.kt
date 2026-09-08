package zeno.gmccpa.pair

import java.math.BigInteger
import java.security.MessageDigest
import java.security.SecureRandom

/**
 * SRP-6a accessory/server — RFC 5054 3072-bit group, SHA-512.
 *
 * A faithful port of `ccpa_custom/crates/vendor/pairing/src/srp.rs`. Needs no third-party crypto:
 * `BigInteger` and SHA-512 are both on the platform at API 32, which is why pair-setup can be done
 * in Kotlin while pair-verify (X25519/Ed25519) cannot.
 *
 * **The one place HomeKit diverges from a naive reading**, carried over verbatim from the reference
 * and verified there against a live iPhone capture: in the client proof, `H(g)` is taken over the
 * MINIMAL generator byte `[5]` — *not* padded to the modulus length, unlike every other operand.
 * Getting this wrong yields a proof mismatch that looks exactly like a wrong setup code.
 */
class SrpServer(
    private val username: ByteArray,
    password: ByteArray,
    val salt: ByteArray,
    bSecret: ByteArray = ByteArray(32).also { SecureRandom().nextBytes(it) }
) {
    companion object {
        /** RFC 5054 3072-bit group. */
        private const val N_HEX =
            "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74" +
            "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437" +
            "4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED" +
            "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05" +
            "98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB" +
            "9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B" +
            "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718" +
            "3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33" +
            "A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7" +
            "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864" +
            "D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2" +
            "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF"

        val USERNAME: ByteArray = "Pair-Setup".toByteArray(Charsets.US_ASCII)
        /** The accessory's fixed setup code, as the reference bakes it in. */
        val SETUP_CODE: ByteArray = "3939".toByteArray(Charsets.US_ASCII)

        /** BigInteger.toByteArray() prepends a zero sign byte; SRP wants the minimal magnitude. */
        fun minimalBytes(x: BigInteger): ByteArray {
            val b = x.toByteArray()
            return if (b.size > 1 && b[0] == 0.toByte()) b.copyOfRange(1, b.size) else b
        }

        fun sha512(vararg parts: ByteArray): ByteArray {
            val md = MessageDigest.getInstance("SHA-512")
            for (p in parts) md.update(p)
            return md.digest()
        }
    }

    private val n: BigInteger = BigInteger(N_HEX, 16)
    private val nLen: Int = minimalBytes(n).size
    private val g: BigInteger = BigInteger.valueOf(5L)
    private val v: BigInteger
    private val b: BigInteger
    private val bPubBig: BigInteger

    init {
        // k = H(N | PAD(g))
        val k = BigInteger(1, sha512(pad(n), pad(g)))
        // x = H(s | H(I | ":" | P))
        val inner = sha512(username, ":".toByteArray(Charsets.US_ASCII), password)
        val x = BigInteger(1, sha512(salt, inner))
        v = g.modPow(x, n)
        b = BigInteger(1, bSecret)
        // B = (k*v + g^b) mod N
        bPubBig = (k.multiply(v).add(g.modPow(b, n))).mod(n)
    }

    /** The server public value B, padded to the modulus length (sent in M2). */
    fun bPub(): ByteArray = pad(bPubBig)

    /**
     * Verify the controller's M3 (`A`, client proof). Returns the server proof M2 on success and
     * stores the session key; null on mismatch.
     */
    fun verify(aPub: ByteArray, clientProof: ByteArray): ByteArray? {
        val a = BigInteger(1, aPub)
        if (a.mod(n).signum() == 0) return null
        // u = H(PAD(A) | PAD(B))
        val u = BigInteger(1, sha512(pad(a), pad(bPubBig)))
        // S = (A * v^u)^b mod N
        val s = a.multiply(v.modPow(u, n)).mod(n).modPow(b, n)
        // K = H(PAD(S))
        val k = sha512(pad(s))
        // M1 = H( H(N) XOR H(g) | H(I) | s | PAD(A) | PAD(B) | K )
        val hn = sha512(pad(n))
        // NOT padded — the documented HomeKit divergence.
        val hg = sha512(byteArrayOf(5))
        val hng = ByteArray(hn.size) { (hn[it].toInt() xor hg[it].toInt()).toByte() }
        val hi = sha512(username)
        val m1 = sha512(hng, hi, salt, pad(a), pad(bPubBig), k)
        if (!constantTimeEquals(m1, clientProof)) return null
        // M2 = H(PAD(A) | M1 | K)
        // K (the SRP session key) is deliberately NOT retained: it is HKDF input for pair-setup
        // M5/M6, which this class does not implement — CarPlayRx answers state 5 with an error and
        // the native core owns the real pairing. Storing it would be a write-only secret at rest.
        val m2 = sha512(pad(a), m1, k)
        return m2
    }

    /** RFC 5054 PAD(): left-pad big-endian bytes to the modulus length. */
    private fun pad(x: BigInteger): ByteArray {
        val bytes = minimalBytes(x)
        if (bytes.size >= nLen) return bytes
        val out = ByteArray(nLen)
        System.arraycopy(bytes, 0, out, nLen - bytes.size, bytes.size)
        return out
    }

    /** The setup-code proof must not leak how many prefix bytes of a guess matched. */
    private fun constantTimeEquals(a: ByteArray, b: ByteArray): Boolean {
        if (a.size != b.size) return false
        var diff = 0
        for (i in a.indices) diff = diff or (a[i].toInt() xor b[i].toInt())
        return diff == 0
    }
}
