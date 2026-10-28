package zeno.gmccpa.av

import zeno.gmccpa.ProbeLog
import zeno.gmccpa.pair.NativeCore
import java.util.concurrent.Executors

/**
 * The ONE thread that writes accessory-initiated commands to the phone's event channel from the
 * media side: the transport keys ([CarPlayMediaBrowserService]) and the OD-9C MainAudio take /
 * give-back ([MainAudioHandover]). Plan v5.9 MED M5.
 *
 * One queue, not one per caller, because the ORDER matters: a MediaSession PLAY after an OD-9C take
 * queues the give-back first (CMBS.onPlay -> resume intent -> [MainAudioHandover.giveBack]) and the
 * HID PLAY key second; on two threads the key could reach the phone while the head unit still owns
 * MainAudio. FIFO on one thread makes "give back, then play" the only possible order.
 *
 * Every task blocks on the native event-channel mutex (`NativeCore` contract), which is why none of
 * this may run on a binder or the main thread. Process-wide and never shut down: the media browser
 * service is bound and unbound by AAOS independently of the CarPlay screen, and a daemon thread
 * holds nothing that outlives the process.
 */
object CarPlayEventTx {
    private val ex = Executors.newSingleThreadExecutor { Thread(it, "cp-event-tx").apply { isDaemon = true } }
    private val log = ProbeLog.sub("aac")

    fun execute(task: Runnable) = ex.execute(task)

    /**
     * The process's OD-9C MainAudio handover. Process-wide like the take it tracks (the receiver's
     * state outlives a CarPlay screen); the player's focus listener and CMBS's resume intent share it.
     */
    val mainAudio: MainAudioHandover by lazy {
        MainAudioHandover(
            object : MainAudioHandover.Port {
                override fun take(): Boolean = NativeCore.takeMainAudio()
                override fun release(): Int = NativeCore.releaseMainAudio()
            },
            { r -> execute(r) },
            { msg -> log.i(msg) },
        )
    }
}
