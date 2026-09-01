package com.carlink.projection

import com.carlink.logging.ProbeLog
import org.json.JSONObject
import java.io.BufferedReader
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Device management: the client half of `carplay-wireless`'s control socket.
 *
 * ## Why this had to be invented rather than reused
 *
 * On the CCPA there was no such surface, because the macOS app drove everything over OCBM and the
 * radio owner was the same box. On the Pi the radio owner (`carplay-wireless`, which drives `hci0`
 * directly) and the UI are separate processes on the same host, and §2 of the design doc rules out
 * the stock Settings ▸ Bluetooth pane entirely: Android's Bluetooth stack is disabled so
 * `carplay-wireless` can own the controller, so that pane has no stack behind it and can never list
 * or pair the phone. Whatever device list we show must come from here.
 *
 * ## Protocol
 *
 * Newline-delimited JSON over loopback TCP, request and response both one line:
 *
 * ```
 *   -> {"cmd":"list"}
 *   <- {"ok":true,"devices":[{"address":"AA:BB:…","name":"iPhone","bonded":true,"connected":false}]}
 *   -> {"cmd":"connect","address":"AA:BB:…"}      // omit address = first-to-connect over the list
 *   -> {"cmd":"disconnect"}
 *   -> {"cmd":"forget","address":"AA:BB:…"}
 *   -> {"cmd":"policy","autoConnect":true,"order":["AA:BB:…","CC:DD:…"]}
 *   -> {"cmd":"status"}
 * ```
 *
 * Deliberately line-JSON rather than a binary framing: this is a low-rate control path where being
 * able to drive it from `nc` during a bring-up session is worth more than efficiency, and every
 * other seam in this system is already binary and hard to inspect by hand.
 *
 * ## Absence is normal
 *
 * `carplay-wireless` may not be running — it is started per boot and can be down while the stack is
 * being worked on. A refused connect is therefore an expected state, not an error, and every call
 * degrades to "no devices" rather than throwing.
 */
class WirelessControlClient(
    private val status: SessionStatus,
) {
    private val log = ProbeLog.sub("ctrl")
    private val running = AtomicBoolean(false)

    @Volatile private var pollThread: Thread? = null

    /** True while the last request succeeded — i.e. the radio owner is reachable. */
    @Volatile
    var available: Boolean = false
        private set

    data class Device(
        val address: String,
        val name: String,
        val bonded: Boolean,
        val connected: Boolean,
    )

    fun start() {
        if (!running.compareAndSet(false, true)) return
        pollThread =
            Thread({ pollLoop() }, "ctrl-poll").apply {
                isDaemon = true
                start()
            }
    }

    fun stop() {
        running.set(false)
    }

    /**
     * Poll for connection state.
     *
     * Polling rather than a push subscription is deliberate for now: `carplay-wireless` has no event
     * loop that could push, and the seam already tells us the important transitions much sooner (the
     * first stream key is the earliest proof of a keyed session). This poll only supplies the
     * *identity* of the connected phone, which nothing else carries, and identity changing a second
     * late costs nothing.
     */
    private fun pollLoop() {
        while (running.get()) {
            val s = request(JSONObject().put("cmd", "status"))
            if (s != null) {
                val addr = s.optString("address").takeIf { it.isNotEmpty() }
                val name = s.optString("name").takeIf { it.isNotEmpty() }
                status.setDevice(addr, name)
            }
            try {
                Thread.sleep(POLL_INTERVAL_MS)
            } catch (_: InterruptedException) {
                return
            }
        }
    }

    data class Status(
        val active: Boolean,
        val address: String?,
        val autoConnect: Boolean,
    )

    /**
     * The box's own view of the session.
     *
     * The device screen must consult this, not just [list]: an ACCEPT-path session — first pairing,
     * and every phone-initiated connect — is live with no known bdaddr, so no row in [list] is
     * marked connected. Without this the screen showed a live phone as merely "Paired" and offered
     * Forget, which deletes the link key of the phone that is currently projecting.
     */
    fun status(): Status? {
        val r = request(JSONObject().put("cmd", "status")) ?: return null
        return Status(
            active = r.optBoolean("active", false),
            address = r.optString("address").takeIf { it.isNotEmpty() },
            autoConnect = r.optBoolean("autoConnect", true),
        )
    }

    fun list(): List<Device> {
        val r = request(JSONObject().put("cmd", "list")) ?: return emptyList()
        val arr = r.optJSONArray("devices") ?: return emptyList()
        return (0 until arr.length()).mapNotNull { i ->
            val o = arr.optJSONObject(i) ?: return@mapNotNull null
            val address = o.optString("address").takeIf { it.isNotEmpty() } ?: return@mapNotNull null
            Device(
                address = address,
                name = o.optString("name").ifEmpty { address },
                bonded = o.optBoolean("bonded", false),
                connected = o.optBoolean("connected", false),
            )
        }
    }

    /**
     * Ask the radio owner to connect.
     *
     * A null [address] means "first-to-connect": try the known devices in the configured order and
     * fall through to the next when one does not answer. That ordering lives on the box side because
     * the retry loop does — doing it here would mean a round trip per attempt and a UI process that
     * must stay alive for the whole sequence.
     */
    fun connect(address: String?): Boolean {
        val req = JSONObject().put("cmd", "connect")
        if (address != null) req.put("address", address)
        return request(req)?.optBoolean("ok", false) ?: false
    }

    fun disconnect(): Boolean = request(JSONObject().put("cmd", "disconnect"))?.optBoolean("ok", false) ?: false

    fun forget(address: String): Boolean = request(JSONObject().put("cmd", "forget").put("address", address))?.optBoolean("ok", false) ?: false

    /**
     * Push the connection policy: whether to auto-connect at boot, and the try order.
     *
     * GM's model, per §5.8 — auto-connect or tap-to-connect as a user setting, with a
     * first-to-connect hierarchy over known devices.
     */
    fun setPolicy(
        autoConnect: Boolean,
        order: List<String>,
    ): Boolean {
        val req =
            JSONObject()
                .put("cmd", "policy")
                .put("autoConnect", autoConnect)
                .put("order", org.json.JSONArray(order))
        return request(req)?.optBoolean("ok", false) ?: false
    }

    /**
     * Push only the auto-connect flag, leaving the stored order alone.
     *
     * `order` is OMITTED rather than sent empty on purpose: the box treats an absent field as "keep
     * the current value" and an empty array as "replace it with nothing", and this UI does not yet
     * manage the first-to-connect order.
     */
    fun setAutoConnect(autoConnect: Boolean): Boolean =
        request(JSONObject().put("cmd", "policy").put("autoConnect", autoConnect))
            ?.optBoolean("ok", false) ?: false

    private fun request(req: JSONObject): JSONObject? {
        return try {
            Socket().use { s ->
                s.connect(
                    InetSocketAddress(SeamContract.LOOPBACK, SeamContract.PORT_CONTROL),
                    CONNECT_TIMEOUT_MS,
                )
                s.soTimeout = READ_TIMEOUT_MS
                s.getOutputStream().apply {
                    write((req.toString() + "\n").toByteArray())
                    flush()
                }
                val line =
                    s.getInputStream().bufferedReader().let { r: BufferedReader -> r.readLine() }
                        ?: return null
                if (!available) log.i("control socket up (:${SeamContract.PORT_CONTROL})")
                available = true
                JSONObject(line)
            }
        } catch (e: IOException) {
            // Expected while carplay-wireless is down. Logged only on the transition.
            if (available) log.i("control socket down — ${e.message}")
            available = false
            null
        } catch (e: org.json.JSONException) {
            log.e("malformed control response: ${e.message}")
            null
        }
    }

    private companion object {
        const val CONNECT_TIMEOUT_MS = 400
        const val READ_TIMEOUT_MS = 1500

        /**
         * 3 s. Identity is the only thing this poll supplies that the seam does not, so a slow
         * cadence is fine; a fast one would wake the radio owner's accept loop for nothing.
         */
        const val POLL_INTERVAL_MS = 3000L
    }
}
