package com.carlink.ocbm

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * The transport seam — a Kotlin port of the macOS host's `RawBulkTransport` protocol shape.
 *
 * Four methods, no platform types. Everything above this line (framing, the client state machine,
 * the MFi relay) compiles and runs against [FakeTransport] with no adapter and no emulator, and
 * graduating the client into a product app is a file move rather than a rewrite.
 */
interface RawBulkTransport {
    /** Write one buffer to the OUT endpoint. Returns false if the write failed or timed out. */
    fun writeBulk(data: ByteArray): Boolean

    /**
     * Install the handler the read thread calls with each raw chunk. Set before [start].
     *
     * The buffer handed to the handler is REUSED across reads — copy synchronously or lose it.
     * `Reassembler.push` does exactly that, which is why routing at the frame level is safe.
     */
    fun setReadHandler(handler: (ByteArray, Int) -> Unit)

    /**
     * Install the handler invoked ONCE if the read loop dies while the transport was still nominally
     * running. Set before [start].
     *
     * It is invoked on a thread of the transport's choosing, never the read thread — the natural
     * reaction is to tear the session down, and [stop] joins the read thread, so running this inline
     * would make that join wait on itself.
     */
    fun setDeadHandler(handler: () -> Unit)

    /** Begin reading. Idempotent. */
    fun start()

    /** Stop reading and release the device. Idempotent. */
    fun stop()
}

/**
 * An in-memory transport for headless testing. Whatever the client writes lands in [written]; call
 * [feed] to deliver bytes as if the box had sent them.
 */
class FakeTransport : RawBulkTransport {
    val written = LinkedBlockingQueue<ByteArray>()
    private var handler: ((ByteArray, Int) -> Unit)? = null
    private var dead: (() -> Unit)? = null
    private var running = false

    override fun writeBulk(data: ByteArray): Boolean {
        written.add(data.copyOf())
        return true
    }

    override fun setReadHandler(handler: (ByteArray, Int) -> Unit) {
        this.handler = handler
    }

    override fun setDeadHandler(handler: () -> Unit) {
        this.dead = handler
    }

    override fun start() {
        running = true
    }

    override fun stop() {
        running = false
    }

    /** Deliver bytes to the client as though the box had written them. */
    fun feed(data: ByteArray) {
        if (running) handler?.invoke(data, data.size)
    }

    /** Simulate the read loop dying, so teardown paths are testable without hardware. */
    fun killLink() {
        running = false
        dead?.invoke()
    }

    /** Pop the next frame the client wrote, or null if it wrote nothing within [ms]. */
    fun takeWritten(ms: Long): ByteArray? = written.poll(ms, TimeUnit.MILLISECONDS)
}
