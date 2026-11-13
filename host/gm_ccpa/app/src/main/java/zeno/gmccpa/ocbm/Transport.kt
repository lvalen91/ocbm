package zeno.gmccpa.ocbm

import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * The transport seam — a Kotlin port of the macOS host's `RawBulkTransport` protocol shape.
 *
 * Five methods, no platform types. Everything above this line (framing, the client state machine,
 * the MFi relay) compiles and runs against [FakeTransport] with no adapter and no emulator, and
 * graduating the client into a product app is a file move rather than a rewrite.
 */
interface RawBulkTransport {
    /** Write one buffer to the OUT endpoint. Returns false if the write failed or timed out. */
    fun writeBulk(data: ByteArray): Boolean

    /** Install the handler the read thread calls with each raw chunk. Set before [start]. */
    fun setReadHandler(handler: (ByteArray, Int) -> Unit)

    /** Begin reading. Idempotent. */
    fun start()

    /** Stop reading and release the device. Idempotent. */
    fun stop()

    /**
     * Ask the read loop to end as if the transport had died, so the transport-death edge runs
     * (`UsbBulkTransport.onTransportDead` -> `OcbmProbe.onLinkDead` -> the box relink) while
     * [stop] is NOT called — stop() clears the running flag first, which suppresses that edge.
     * Returns true when a live read loop took the request (the edge follows once it exits, within
     * one read timeout); false when there is no running read loop to end, in which case the caller
     * must deliver the box-lost edge itself. v5.6 H1-1 (2026-10-06): the heartbeat calls this when
     * its writes fail 5x while reads still time out normally — the half-dead link that never ended
     * the read loop on its own.
     */
    fun declareDead(why: String): Boolean = false
}

/**
 * An in-memory transport for headless testing. Whatever the client writes lands in [written]; call
 * [feed] to deliver bytes as if the box had sent them.
 */
class FakeTransport : RawBulkTransport {
    val written = LinkedBlockingQueue<ByteArray>()
    /** Every write ATTEMPT, accepted or failed, in order (tests assert on what was tried). */
    val attempts = ConcurrentLinkedQueue<ByteArray>()
    @Volatile private var handler: ((ByteArray, Int) -> Unit)? = null
    @Volatile private var running = false

    /** Every write fails (returns false, nothing lands in [written]) while this is set. */
    @Volatile var failWrites = false
    /** The next N writes fail, then writes succeed again (unless [failWrites]). */
    val failNext = AtomicInteger(0)
    /** Called with each frame before it is accepted or failed — a test may block here. */
    @Volatile var writeHook: ((ByteArray) -> Unit)? = null

    /** [declareDead] calls, and its scripted answer. [onDeclareDead] runs before it returns. */
    val declareDeadCalls = AtomicInteger(0)
    @Volatile var declareDeadResult = false
    @Volatile var onDeclareDead: ((String) -> Unit)? = null

    override fun writeBulk(data: ByteArray): Boolean {
        val copy = data.copyOf()
        writeHook?.invoke(copy)
        attempts.add(copy)
        if (failWrites) return false
        if (failNext.get() > 0 && failNext.getAndDecrement() > 0) return false
        written.add(copy)
        return true
    }

    override fun declareDead(why: String): Boolean {
        declareDeadCalls.incrementAndGet()
        onDeclareDead?.invoke(why)
        return declareDeadResult
    }

    override fun setReadHandler(handler: (ByteArray, Int) -> Unit) { this.handler = handler }
    override fun start() { running = true }
    override fun stop() { running = false }

    /** Deliver bytes to the client as though the box had written them. */
    fun feed(data: ByteArray) { if (running) handler?.invoke(data, data.size) }

    /** Pop the next frame the client wrote, or null if it wrote nothing within [ms]. */
    fun takeWritten(ms: Long): ByteArray? = written.poll(ms, TimeUnit.MILLISECONDS)
}
