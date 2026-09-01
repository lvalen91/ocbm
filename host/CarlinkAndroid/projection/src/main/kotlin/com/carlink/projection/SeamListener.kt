package com.carlink.projection

import com.carlink.logging.ProbeLog
import java.io.IOException
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * One loopback listener that accepts a single producer at a time and pumps its bytes to [onBytes].
 *
 * `airplayd` keeps exactly one connection per sink alive and reconnects after any failure, so
 * serving one connection at a time is the whole requirement. A second concurrent connection is a
 * bug on the producer side, and accepting it would interleave two byte streams into one seam
 * parser — which surfaces much later as an unrecoverable framing desync rather than as an error.
 * So: accept, serve, close, accept again.
 *
 * ## Lifetime
 *
 * [start] is non-blocking. [stop] closes the server socket and the live connection, which is what
 * unblocks the accept and the read; the thread then exits on its own. Closing sockets is the only
 * reliable way to interrupt a blocking socket read on Android — interrupting the thread does not.
 */
class SeamListener(
    private val port: Int,
    private val name: String,
    private val onConnect: () -> Unit = {},
    private val onDisconnect: () -> Unit = {},
    private val onBytes: (ByteArray, Int) -> Unit,
) {
    private val log = ProbeLog.sub("seam")
    private val running = AtomicBoolean(false)

    @Volatile private var server: ServerSocket? = null

    @Volatile private var live: Socket? = null

    @Volatile private var thread: Thread? = null

    val bytesIn = AtomicLong(0)
    val connections = AtomicLong(0)

    /** True while a producer is connected — surfaced so the UI can distinguish "no session" from "no frames". */
    @Volatile
    var connected: Boolean = false
        private set

    fun start() {
        if (!running.compareAndSet(false, true)) return
        val t =
            Thread({ run() }, "seam-$name").apply {
                isDaemon = true
                start()
            }
        thread = t
    }

    fun stop() {
        running.set(false)
        // Order matters: close the server first so the accept loop cannot immediately take a new
        // connection after we close the live one.
        runCatching { server?.close() }
        runCatching { live?.close() }
    }

    fun join(millis: Long) {
        runCatching { thread?.join(millis) }
    }

    private fun run() {
        val s =
            try {
                // Bound to loopback explicitly. The default binds every interface, which on the Pi
                // would expose the decrypted-frame seam to the CarPlay SoftAP the phone is sitting
                // on — the one network where an untrusted device is by definition present.
                ServerSocket(port, BACKLOG, InetAddress.getByName(SeamContract.LOOPBACK))
            } catch (e: IOException) {
                // The overwhelmingly likely cause is a previous instance that has not died yet, or
                // a leftover native consumer. Say which, because "no video" otherwise looks
                // identical to a protocol failure three layers down.
                log.e("$name: bind ${SeamContract.LOOPBACK}:$port failed — ${e.message}")
                running.set(false)
                return
            }
        server = s
        log.i("$name: listening on ${SeamContract.LOOPBACK}:$port")

        val buf = ByteArray(READ_BUF)
        while (running.get()) {
            val c =
                try {
                    s.accept()
                } catch (e: IOException) {
                    if (running.get()) log.w("$name: accept failed — ${e.message}")
                    continue
                }
            live = c
            connected = true
            connections.incrementAndGet()
            log.i("$name: producer connected (#${connections.get()})")
            runCatching { onConnect() }
            try {
                // TCP_NODELAY: these are latency-sensitive media lanes on loopback, and Nagle
                // would coalesce a small frame header with the next frame's body for 40 ms.
                c.tcpNoDelay = true
                val ins = c.getInputStream()
                while (running.get()) {
                    val n = ins.read(buf)
                    if (n < 0) break
                    if (n > 0) {
                        bytesIn.addAndGet(n.toLong())
                        onBytes(buf, n)
                    }
                }
            } catch (e: IOException) {
                if (running.get()) log.i("$name: producer read ended — ${e.message}")
            } finally {
                connected = false
                runCatching { c.close() }
                live = null
                runCatching { onDisconnect() }
                log.i("$name: producer disconnected (${bytesIn.get()} B total)")
            }
        }
        runCatching { s.close() }
        log.i("$name: listener stopped")
    }

    private companion object {
        /**
         * One producer, but a backlog of 1 rather than 0: `airplayd` may reconnect before we have
         * looped back to accept, and a refused connection there costs a frame AND a 2 s
         * connect-timeout stall on its screen thread.
         */
        const val BACKLOG = 2

        /**
         * 64 KiB. The seam parsers buffer and reassemble internally, so this only trades syscalls
         * against copy size; it is not a frame-size limit. A 4K IDR arrives across many reads.
         */
        const val READ_BUF = 64 * 1024
    }
}
