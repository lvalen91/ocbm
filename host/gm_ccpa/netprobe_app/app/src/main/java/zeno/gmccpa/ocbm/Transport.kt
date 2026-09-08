package zeno.gmccpa.ocbm

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

    /** Install the handler the read thread calls with each raw chunk. Set before [start]. */
    fun setReadHandler(handler: (ByteArray, Int) -> Unit)

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
    private var running = false

    override fun writeBulk(data: ByteArray): Boolean {
        written.add(data.copyOf())
        return true
    }

    override fun setReadHandler(handler: (ByteArray, Int) -> Unit) { this.handler = handler }
    override fun start() { running = true }
    override fun stop() { running = false }

    /** Deliver bytes to the client as though the box had written them. */
    fun feed(data: ByteArray) { if (running) handler?.invoke(data, data.size) }

    /** Pop the next frame the client wrote, or null if it wrote nothing within [ms]. */
    fun takeWritten(ms: Long): ByteArray? = written.poll(ms, TimeUnit.MILLISECONDS)
}
