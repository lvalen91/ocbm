package com.carlink.ocbm

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbEndpoint
import android.hardware.usb.UsbInterface
import android.hardware.usb.UsbManager
import android.os.Build
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The OCBM accessory over Android USB host mode.
 *
 * The box enumerates as VID 0x1314 / PID 0x2d00 with bDeviceClass 0, interface 0 class 0xFF, bulk
 * IN 0x81 / OUT 0x01 at 512-byte max packet (hardware-verified 2026-08-12; older docs said
 * 0x83/0x02 — the code is immune either way because it walks the interface). It is a RAW BYTE PIPE: there is no AOA control
 * handshake (51/52/53) to perform — claim interface 0 and start moving bytes. All framing is OCBM's.
 */
class UsbBulkTransport(
    private val ctx: Context,
    private val log: com.carlink.logging.ProbeLog.Logger,
) : RawBulkTransport {
    companion object {
        const val VID_CARLINKIT = 0x1314
        const val PID_OCBM = 0x2d00

        private const val ACTION_USB_PERMISSION = "com.carlink.ocbm.USB_PERMISSION"

        /** Android's bulkTransfer is unreliable above 16 KiB on some platforms; frames reassemble anyway. */
        private const val READ_BUF = 16384
        private const val READ_TIMEOUT_MS = 250
        private const val WRITE_TIMEOUT_MS = 2000
        private const val WRITE_CHUNK = 16384
    }

    // Application context, never the Activity: the read/heartbeat threads outlive an Activity and
    // holding one here leaks it for the whole session.
    private val appCtx = ctx.applicationContext
    private val usb = appCtx.getSystemService(Context.USB_SERVICE) as UsbManager

    @Volatile private var device: UsbDevice? = null

    @Volatile private var conn: UsbDeviceConnection? = null

    @Volatile private var iface: UsbInterface? = null

    @Volatile private var epIn: UsbEndpoint? = null

    @Volatile private var epOut: UsbEndpoint? = null

    @Volatile private var readThread: Thread? = null
    private val running = AtomicBoolean(false)

    @Volatile private var handler: ((ByteArray, Int) -> Unit)? = null
    private val writeLock = Object()

    @Volatile
    var lastError: String? = null
        private set

    /** Consecutive write failures. The write path is the unambiguous liveness signal, not reads. */
    @Volatile
    var writeFailures = 0
        private set

    /**
     * Same selection as [find] with no logging — for poll loops. [find] dumps the entire device list
     * on every call, which at a 2 s poll would bury the log in minutes.
     */
    fun findQuiet(): UsbDevice? {
        val all = usb.deviceList.values
        return all.firstOrNull { it.vendorId == VID_CARLINKIT && it.productId == PID_OCBM }
    }

    /** Cheap, authoritative check. The grant lives in system_server keyed by (device, uid). */
    fun hasPermission(dev: UsbDevice): Boolean = usb.hasPermission(dev)

    /**
     * Fire the permission request WITHOUT blocking on a broadcast.
     *
     * The dialog on this head unit frequently never appears, and when it does the result broadcast is
     * often lost — so a BLOCKING request is the wrong shape for a retry loop. The caller polls
     * [hasPermission] instead, which is authoritative regardless of whether the broadcast
     * arrives. Call this sparingly: each invocation can raise a system dialog.
     */
    fun requestPermissionAsync(dev: UsbDevice) {
        try {
            val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
            val pi =
                PendingIntent.getBroadcast(
                    appCtx,
                    0,
                    Intent(ACTION_USB_PERMISSION).setPackage(appCtx.packageName),
                    flags,
                )
            usb.requestPermission(dev, pi)
        } catch (t: Throwable) {
            log.w("requestPermission threw ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /**
     * Claim interface 0 and locate the bulk pair. Rather than hardcoding endpoint addresses we pick
     * the interface that actually carries a bulk IN + bulk OUT pair — some units differ.
     */
    fun open(dev: UsbDevice): Boolean {
        val c = usb.openDevice(dev)
        if (c == null) {
            lastError = "openDevice returned null (permission?)"
            log.w("claim FAILED: $lastError")
            return false
        }
        for (i in 0 until dev.interfaceCount) {
            val itf = dev.getInterface(i)
            var inEp: UsbEndpoint? = null
            var outEp: UsbEndpoint? = null
            for (e in 0 until itf.endpointCount) {
                val ep = itf.getEndpoint(e)
                if (ep.type != UsbConstants.USB_ENDPOINT_XFER_BULK) continue
                if (ep.direction == UsbConstants.USB_DIR_IN && inEp == null) inEp = ep
                if (ep.direction == UsbConstants.USB_DIR_OUT && outEp == null) outEp = ep
            }
            if (inEp != null && outEp != null) {
                if (!c.claimInterface(itf, true)) {
                    log.w("claimInterface(${itf.id}) FAILED — another process may hold it")
                    continue
                }
                conn = c
                iface = itf
                epIn = inEp
                epOut = outEp
                device = dev
                log.i(
                    "claimed interface ${itf.id} (class 0x%02x) IN=0x%02x OUT=0x%02x mps=%d"
                        .format(itf.interfaceClass, inEp.address, outEp.address, inEp.maxPacketSize),
                )
                log.i("no AOA control handshake performed — OCBM is a raw byte pipe")
                return true
            }
        }
        lastError = "no interface with a bulk IN+OUT pair"
        log.w("claim FAILED: $lastError")
        c.close()
        return false
    }

    override fun setReadHandler(handler: (ByteArray, Int) -> Unit) {
        this.handler = handler
    }

    override fun start() {
        if (!running.compareAndSet(false, true)) return
        val t = Thread({ readLoop() }, "ocbm-read")
        t.isDaemon = true
        readThread = t
        t.start()
    }

    private fun readLoop() {
        val buf = ByteArray(READ_BUF)
        while (running.get()) {
            // Any throw here would otherwise propagate out of the thread and Android's default
            // handler would kill the PROCESS — one malformed frame taking down the instrument
            // mid-session and leaving the box subscribed until its watchdog fires.
            try {
                val c = conn ?: break
                val ep = epIn ?: break
                val t0 = System.nanoTime()
                val n =
                    try {
                        c.bulkTransfer(ep, buf, buf.size, READ_TIMEOUT_MS)
                    } catch (t: Throwable) {
                        // Do not silently coerce to "idle" — a real failure must be visible.
                        log.w("read: bulkTransfer threw ${t.javaClass.simpleName}: ${t.message}")
                        -1
                    }
                // bulkTransfer returns -1 for BOTH a timeout and a hard failure. A timeout takes ~the
                // full READ_TIMEOUT_MS; a device-gone failure (ENODEV) returns almost immediately. An
                // immediate -1 spun this loop at ~1000/s; detect it by elapsed time, back off, and
                // after a sustained run declare the transport dead so the session can tear down.
                if (n > 0) {
                    readFailures = 0
                    rapidMinusOne = 0
                    bytesRead += n
                    reads++
                    // Saturation: the transfer filled the buffer, so there was probably more waiting.
                    // A high ratio is the evidence that would justify a larger READ_BUF or UsbRequest
                    // pipelining — until it fires, neither is earned.
                    if (n == buf.size) saturatedReads++
                    handler?.invoke(buf, n)
                } else {
                    val elapsedMs = (System.nanoTime() - t0) / 1_000_000
                    if (elapsedMs < 10) {
                        if (++rapidMinusOne >= 50) {
                            // A halted endpoint presents exactly like a dead device. Try to clear it
                            // once before giving up — a stall is recoverable, an unplug is not.
                            if (!triedClearHaltIn) {
                                triedClearHaltIn = true
                                if (clearHalt(ep)) {
                                    rapidMinusOne = 0
                                    log.w("read: cleared IN halt, continuing")
                                    continue
                                }
                            }
                            log.e("read: rapid -1 x$rapidMinusOne — device gone, stopping")
                            break
                        }
                        Thread.sleep(20)
                    } else {
                        rapidMinusOne = 0
                        Thread.sleep(1)
                    }
                }
            } catch (t: Throwable) {
                log.e("read loop error (continuing): ${t.javaClass.simpleName}: ${t.message}")
                readFailures++
                if (readFailures >= 20) {
                    log.e("read: too many consecutive errors — stopping")
                    break
                }
            }
        }
        log.i("read loop ended — $reads reads, $bytesRead B, $saturatedReads saturated")
        // Clear `running` BEFORE announcing death: otherwise every subsequent writeBulk keeps trying
        // against a pipe we already know is gone, burning a 2 s timeout each.
        val wasRunning = running.getAndSet(false)
        if (wasRunning) {
            val h = onTransportDead
            // NOT on this thread. The handler's natural reaction is to tear the session down, and
            // stop() joins the read thread — running it inline makes that join wait on itself for its
            // full 1500 ms and then take the leak-the-fd branch.
            if (h != null && deadAnnounced.compareAndSet(false, true)) {
                Thread({ runCatching { h() } }, "ocbm-dead").apply { isDaemon = true }.start()
            }
        }
    }

    private var readFailures = 0
    private var rapidMinusOne = 0
    private var triedClearHaltIn = false
    private var triedClearHaltOut = false
    private val deadAnnounced = AtomicBoolean(false)

    /** Read-path counters, for the session's periodic stat line. */
    @Volatile var bytesRead: Long = 0
        private set

    @Volatile var reads: Long = 0
        private set

    /** Reads that filled the whole buffer — the evidence for or against a bigger read. */
    @Volatile var saturatedReads: Long = 0
        private set

    /** Invoked if the read loop dies while still nominally running, so the session can tear down. */
    @Volatile
    var onTransportDead: (() -> Unit)? = null

    override fun setDeadHandler(handler: () -> Unit) {
        onTransportDead = handler
    }

    /**
     * Best-effort USB CLEAR_FEATURE(ENDPOINT_HALT) on [ep].
     *
     * Android's USB host API exposes no `clearHalt`, so this is the standard control transfer spelled
     * out: bmRequestType 0x02 (host-to-device, standard, endpoint recipient), bRequest 0x01
     * (CLEAR_FEATURE), wValue 0x00 (ENDPOINT_HALT), wIndex = the endpoint address.
     *
     * UNVERIFIED against this gadget. Deliberately never changes control flow on failure — it either
     * rescues a stalled endpoint or the caller proceeds to declare the link dead exactly as before.
     */
    private fun clearHalt(ep: UsbEndpoint): Boolean {
        val c = conn ?: return false
        return try {
            val r = c.controlTransfer(0x02, 0x01, 0x00, ep.address, null, 0, 1000)
            log.w("clearHalt(ep=0x%02x) -> %d".format(ep.address, r))
            r >= 0
        } catch (t: Throwable) {
            log.w("clearHalt threw ${t.javaClass.simpleName}: ${t.message}")
            false
        }
    }

    override fun writeBulk(data: ByteArray): Boolean {
        val c = conn ?: return false
        val ep = epOut ?: return false
        synchronized(writeLock) {
            if (!running.get()) return false
            var off = 0
            while (off < data.size) {
                val len = minOf(WRITE_CHUNK, data.size - off)
                val chunk = if (off == 0 && len == data.size) data else data.copyOfRange(off, off + len)
                val n =
                    try {
                        c.bulkTransfer(ep, chunk, len, WRITE_TIMEOUT_MS)
                    } catch (t: Throwable) {
                        -1
                    }
                // A stalled OUT endpoint fails identically to a dead one, so try to clear it once —
                // but do NOT re-send this chunk. `bulkTransfer` returns a bare -1 for both a stall and
                // a timeout and DISCARDS the partial-progress count either way, so some 512-byte
                // packets of this chunk may already have been accepted by the box. Re-sending would
                // duplicate them mid-frame and tear the OCBM stream. Report the failure instead: every
                // caller already tolerates a failed write (hello() retransmits, the heartbeat
                // escalates), so the retry would only have saved one of them.
                if (n < 0 && !triedClearHaltOut) {
                    triedClearHaltOut = true
                    if (clearHalt(ep)) log.w("write: cleared OUT halt — this frame is still lost")
                }
                if (n < 0) {
                    writeFailures++
                    lastError = "bulkTransfer OUT failed at offset $off"
                    log.w("write FAILED: $lastError (consecutive=$writeFailures)")
                    return false
                }
                off += n
                if (n == 0) {
                    lastError = "bulkTransfer OUT wrote 0 bytes"
                    return false
                }
            }
        }
        writeFailures = 0
        return true
    }

    override fun stop() {
        // Deliberately NOT gated on the running CAS: if open() succeeded but start() was never
        // reached, the interface would otherwise stay claimed for the life of the process — a
        // claimed-but-unusable device that nothing can recover.
        running.set(false)
        val rt = readThread
        try {
            rt?.join(1500)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
        readThread = null
        val readAlive = rt?.isAlive == true
        // Take the write lock before releasing: bulkTransfer is not interruptible, so without this
        // the connection can be closed with native bulk I/O still in flight on it. On this gadget
        // that is exactly the stall that needs a power cycle.
        synchronized(writeLock) {
            if (readAlive) {
                // The read thread is still parked in a bulkTransfer (gadget stall). Closing the
                // connection under a live native transfer is the exact power-cycle stall — defer the
                // release and just drop our refs. Leaks one fd until process exit; far better than a
                // wedged USB gadget.
                log.w("read thread did not exit in 1500ms — deferring interface release to avoid closing under a live transfer")
            } else {
                try {
                    iface?.let { conn?.releaseInterface(it) }
                } catch (_: Throwable) {
                }
                try {
                    conn?.close()
                } catch (_: Throwable) {
                }
            }
            conn = null
            iface = null
            epIn = null
            epOut = null
        }
        log.i("usb transport closed")
    }
}
