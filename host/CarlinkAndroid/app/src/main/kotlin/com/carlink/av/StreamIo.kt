package com.carlink.av

import java.io.IOException
import java.io.InputStream

/** Blocking read helpers shared by the seam consumers. */
object StreamIo {
    /** Fill [dst] with exactly [n] bytes from [ins]; false on EOF or a read error (the caller closes). */
    fun readFully(
        ins: InputStream,
        dst: ByteArray,
        n: Int,
    ): Boolean {
        var off = 0
        while (off < n) {
            val r =
                try {
                    ins.read(dst, off, n - off)
                } catch (_: IOException) {
                    return false
                }
            if (r <= 0) return false
            off += r
        }
        return true
    }
}
