package zeno.gmccpa.ocbm

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.7 U8 / audit12 F057: CH_MGMT has one reply queue and no request id, and both
 * [OcbmClient.mgmtGetInfo] and [OcbmClient.mgmtAction] clear it before sending. The tests force the
 * exact interleaving that loses a reply when the two are unlocked: the box's reply to the first
 * request is already queued when the second request starts. With [OcbmClient]'s mgmtLock the second
 * request waits, and both get their own replies. On v56's [FakeTransport]; trace = false and a silent
 * logger, so no Android class is touched at runtime.
 */
class OcbmClientMgmtTest {

    private val fake = FakeTransport()
    // v5.8 V7 / Q015: the client's deadlines run on an injected monotonic clock; the tier-0 harness
    // stubs android.os.SystemClock to THROW, so the default would fail the first subscribe().
    private val c = OcbmClient(fake, zeno.gmccpa.ProbeLog.silent(), 0x5A5A1234, trace = false, monoMs = { System.nanoTime() / 1_000_000L })
    /** Every box->host feed goes through one thread: the client's Reassembler is single-reader. */
    private val box = Executors.newSingleThreadExecutor()
    private val infoJson = "{\"devices\":[\"00:11:22:33:44:55\"]}"

    private fun mgmt(vararg b: Byte) = Framing.frame(Ocbm.CH_MGMT, Ocbm.F_BOTH, 0, b)
    private fun isMgmt(f: ByteArray, verb: Byte) =
        f.size > Ocbm.HDR_LEN && f[8] == 0x40.toByte() && f[9] == 0.toByte() && f[Ocbm.HDR_LEN] == verb
    private fun isCtrl(f: ByteArray, type: Byte) =
        f.size > Ocbm.HDR_LEN && f[8] == 0.toByte() && f[9] == 0.toByte() && f[Ocbm.HDR_LEN] == type
    private fun feedSync(frame: ByteArray) { box.submit { fake.feed(frame) }.get(2, TimeUnit.SECONDS) }
    private fun infoReply() = mgmt(Ocbm.MGMT_INFO, *infoJson.toByteArray(Charsets.UTF_8))
    private fun ackReply(verb: Byte, st: Byte = 0) = mgmt(Ocbm.MGMT_ACK, verb, st)

    private fun bringUp() {
        fake.start()
        c.start()
        // Arms logStreamWanted, so a successful FORGET's reArmAfterMgmt is visible as a CT_LOG_CTL write.
        assertTrue(c.logCtl(true))
    }

    @After fun tearDown() { c.stop(); box.shutdownNow() }

    /** A FORGET is waiting for its ACK (already queued) when a getInfo starts on another thread. */
    @Test fun getInfoStartingWhileForgetAwaitsItsAckDoesNotEatTheAck() {
        bringUp()
        val infoResult = AtomicReference<String?>()
        val infoSent = CountDownLatch(1)
        val infoThread = Thread { infoResult.set(c.mgmtGetInfo(3000)) }
        fake.writeHook = { f ->
            if (isMgmt(f, Ocbm.MGMT_FORGET_ALL)) {
                feedSync(ackReply(Ocbm.MGMT_FORGET_ALL))
                infoThread.start()
                // Unlocked, getInfo clears the queued ACK and writes here. Locked, it cannot start.
                infoSent.await(600, TimeUnit.MILLISECONDS)
            } else if (isMgmt(f, Ocbm.MGMT_GET_INFO)) {
                infoSent.countDown()
                feedSync(infoReply())
            }
        }
        val st = c.mgmtAction(Ocbm.MGMT_FORGET_ALL, timeoutMs = 1500)
        infoThread.join(6000)
        assertEquals("FORGET got its own ACK", 0, st)
        assertEquals("getInfo got its own MGMT_INFO", infoJson, infoResult.get())
        val frames = fake.attempts.toList()
        val forgetAt = frames.indexOfFirst { isMgmt(it, Ocbm.MGMT_FORGET_ALL) }
        assertTrue("CH_LOG re-armed after the FORGET (reArmAfterMgmt ran)",
            frames.drop(forgetAt + 1).any { isCtrl(it, Ocbm.CT_LOG_CTL) })
    }

    /** The reverse: a getInfo is waiting for its MGMT_INFO (already queued) when a FORGET starts. */
    @Test fun forgetStartingWhileGetInfoAwaitsItsReplyDoesNotEatTheReply() {
        bringUp()
        val actResult = AtomicReference<Int?>()
        val forgetSent = CountDownLatch(1)
        val actThread = Thread { actResult.set(c.mgmtAction(Ocbm.MGMT_FORGET_ALL, timeoutMs = 3000)) }
        fake.writeHook = { f ->
            if (isMgmt(f, Ocbm.MGMT_GET_INFO)) {
                feedSync(infoReply())
                actThread.start()
                forgetSent.await(600, TimeUnit.MILLISECONDS)
            } else if (isMgmt(f, Ocbm.MGMT_FORGET_ALL)) {
                forgetSent.countDown()
                feedSync(ackReply(Ocbm.MGMT_FORGET_ALL))
            }
        }
        val info = c.mgmtGetInfo(1500)
        actThread.join(6000)
        assertEquals("getInfo got its own MGMT_INFO", infoJson, info)
        assertEquals("FORGET got its own ACK", 0, actResult.get())
        val frames = fake.attempts.toList()
        val forgetAt = frames.indexOfFirst { isMgmt(it, Ocbm.MGMT_FORGET_ALL) }
        assertTrue(forgetAt >= 0)
        assertTrue("CH_LOG re-armed after the FORGET",
            frames.drop(forgetAt + 1).any { isCtrl(it, Ocbm.CT_LOG_CTL) })
    }

    /** A request that times out releases the lock: the next one still runs and gets its reply. */
    @Test fun timedOutRequestReleasesTheLock() {
        bringUp()
        assertEquals(null, c.mgmtAction(Ocbm.MGMT_RESTART_WIRELESS, timeoutMs = 300))
        fake.writeHook = { f -> if (isMgmt(f, Ocbm.MGMT_GET_INFO)) feedSync(infoReply()) }
        assertEquals(infoJson, c.mgmtGetInfo(1000))
    }

    /** A failed write releases the lock too (the early `return null` inside the locked block). */
    @Test fun failedWriteReleasesTheLock() {
        bringUp()
        fake.failNext.set(1)
        assertEquals(null, c.mgmtGetInfo(500))
        fake.writeHook = { f -> if (isMgmt(f, Ocbm.MGMT_FORGET_ALL)) feedSync(ackReply(Ocbm.MGMT_FORGET_ALL, 1)) }
        assertEquals("error status passed through", 1, c.mgmtAction(Ocbm.MGMT_FORGET_ALL, timeoutMs = 1000))
    }
}
