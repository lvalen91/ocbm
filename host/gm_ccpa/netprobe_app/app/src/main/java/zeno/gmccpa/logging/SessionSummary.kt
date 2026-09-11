package zeno.gmccpa.logging

import zeno.gmccpa.ocbm.Ocbm
import android.hardware.usb.UsbDevice
import android.os.SystemClock
import org.json.JSONArray
import org.json.JSONObject
import zeno.gmccpa.ProbeLog
import java.util.Collections
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * One greppable line per CarPlay session, so a bad drive diffs against a good one without opening
 * the 6+ GB capture that produced it.
 *
 * ## Lifecycle
 * A session **begins at USB attach** — the earliest event this app controls — and **ends** at clean
 * teardown, at app death, or when a new attach supersedes it (the box was unplugged and replugged, or
 * the trampoline fired again, without this app ever seeing a clean close). [begin] returns the
 * [Session] handle; callers on the OCBM reader thread, the UI thread, and the HEVC render thread feed
 * it facts as they learn them via its setters, and [end] emits the line.
 *
 * Not every session HAS an attach, though. A launcher tap, `am start`, or a task switch with the
 * adapter already plugged in brings the link up without the trampoline ever running for it, and until
 * 2026-09-10 those sessions produced no line at all — the whole 2026-09-09 truck capture, a complete
 * and successful drive, holds zero `SESSION v=` lines. Such a session begins at the **first link
 * attempt with the adapter present** ([beginLaunch], called from `OcbmProbe.runAllLocked` before it
 * starts waiting for a claim) and says so: `origin=launch`, with every attach-only fact rendered as
 * the explicit absent token rather than a plausible value. See [AttachInfo.origin]. A session that never reaches
 * [end] — killed by the platform, a native crash, a wedged heartbeat — is exactly the interesting case
 * for fault-hunting, so [end] is NOT the only way a line gets written: a superseding [begin] and a JVM
 * shutdown hook both flush a best-effort line for whatever the current session knew at that moment,
 * tagged with why it closed early (see `exit=`).
 *
 * ## Why fields are pre-verdicted
 * `docs`/KDoc elsewhere (`UsbAttachActivity`, `OcbmProto`) already explains what each raw value
 * MEANS — which hypothesis a `SecurityException` on the serial read kills, why `BTP_WIFI_HANDOFF` is the
 * line between a BT fault and a Wi-Fi fault. This class carries those verdicts forward as booleans
 * (`prompted=`, `reached_handoff=`, `bond_changed=`, `no_video=`) so `grep 'prompted=true' *.log` answers
 * fault 1 across a week of drives without a human re-deriving the verdict from raw fields every time.
 *
 * ## Format contract
 * One line, `SESSION v=<schema> key=value ...`, fixed field order, emitted via [ProbeLog] under the
 * `sess` subsystem tag (so the full line in logcat reads `[sess ] SESSION v=1 id=... ...`). Absent data
 * is always an explicit token (`none`, `unknown`) — never a dropped key — because a missing field would
 * be indistinguishable from a real zero, and this whole exercise is about not fooling the next reader.
 * The field SET and ORDER are a versioned contract (`v=`): grow it by bumping [SCHEMA_VERSION] and
 * appending fields, never by reordering or repurposing an existing key, since these lines get diffed
 * across weeks of captures.
 *
 * Schema history:
 *  - `v=1` (2026-08-26): the original field set, `id=` … `exit=`.
 *  - `v=2` (2026-09-10): `origin=` appended at the end (`usb_attach` | `launch`); `perm_trampoline=`
 *    may now read `none` (a launch-origin session had no trampoline to sample). No key moved or
 *    changed meaning: a `v=1` reader that ignores unknown trailing keys still parses a `v=2` line, and
 *    `perm_trampoline=none` is the same absent-token rule every other field already follows.
 *
 * A lower-frequency `SESSION_DETAIL id=<id> ...` block (session-end only, one key per line) carries the
 * fields too long or too structured for one line: the raw phone-identity JSON and the two `MGMT_INFO`
 * bonded-device snapshots, verbatim and as parsed lists.
 *
 * `phone_device_id=` is the iPhone's BR/EDR MAC lifted from `CT_PHONE_IDENT`; it and the raw
 * `SESSION_DETAIL` JSON blocks are deliberately NOT redacted here — the export-time redactor
 * (`LogExport.kt`) is expected to find them by these key names.
 *
 * ## Thread safety / cost
 * Scalar facts are `@Volatile`; the two small, rare collections (session-event history, at most a few
 * events per session) are synchronized. Nothing here is touched by the A/V hot path — [Session.onAvFinal]
 * takes three already-computed `Long`s at teardown, exactly the "read counters at session end" the decoder
 * already does in its own `stop()` log line.
 */
object SessionSummary {

    /** Bump on any field-set/order change; never reinterpret an existing key. History in the class KDoc. */
    const val SCHEMA_VERSION = 2

    /**
     * How long a permission-dialog observation with NO open session waits for a session to claim it.
     *
     * The race it closes: the app is launched with the adapter absent, so no session begins; the driver
     * plugs the box in; `OcbmProbe.awaitClaimable` polls every 2 s and can see the device, find no
     * grant and raise the dialog BEFORE the framework has started `UsbAttachActivity` for the same
     * attach — whose [begin] is what would have received the observation. Recorded on the session
     * that begins within this window instead of dropped; a session beginning later than this is not
     * plausibly the one the dialog was for, and the observation is discarded with a log line.
     */
    private const val PENDING_PROMPT_WINDOW_MS = 15_000L

    private const val MAX_SEV_HISTORY = 64

    private val log = ProbeLog.sub("sess")
    private val nextId = AtomicLong(0)
    private val lock = Any()

    @Volatile private var current: Session? = null
    /** elapsedRealtime of a [permissionDialogObserved] that found no open session, or 0. */
    @Volatile private var pendingPromptAt: Long = 0L

    init {
        // Best-effort line for a session that dies with the process instead of calling end(): a killed
        // app is itself one of the two fault shapes we're hunting (fault 2's failed reconnect often
        // shows up as the app never reaching a clean stop), so losing its summary would lose the fault.
        // Not guaranteed to run under a SIGKILL/native crash, but it is free and catches the rest.
        Runtime.getRuntime().addShutdownHook(Thread({
            current?.let { if (!it.closed.get()) emit(it, "process_death") }
        }, "session-summary-shutdown"))
    }

    /**
     * Start a new session at USB attach. If a prior session is still open (no [end] was ever called —
     * the box was replugged, or the trampoline fired again, without a clean teardown in between), emit
     * a best-effort line for it FIRST so that failure is not silently lost.
     */
    fun begin(attach: AttachInfo): Session {
        val s = Session(nextId.incrementAndGet(), attach)
        val prev: Session?
        val pendingAt: Long
        synchronized(lock) {
            prev = current
            current = s
            pendingAt = pendingPromptAt
            pendingPromptAt = 0L
        }
        if (prev != null && !prev.closed.get()) {
            // The label names what the superseded session WAS, not just what replaced it: a
            // launch-origin session that never got past waiting for the adapter, and was then
            // overtaken by the real attach, is expected behaviour — not a replug or a second
            // trampoline firing, which is what `superseded_by_new_attach` has always meant.
            val why = if (prev.attach.origin == Origin.LAUNCH) "launch_superseded_by_attach"
                      else "superseded_by_new_attach"
            log.w("session id=${prev.id} (origin=${prev.attach.origin.tag}) superseded by id=${s.id} " +
                "(origin=${attach.origin.tag}) before a clean close — exit=$why")
            emit(prev, why)
        }
        if (pendingAt != 0L) {
            val ageMs = SystemClock.elapsedRealtime() - pendingAt
            if (ageMs <= PENDING_PROMPT_WINDOW_MS) {
                s.permissionDialogObserved()
                log.i("session id=${s.id} inherits a permission-dialog observation from ${ageMs}ms before it began (no session was open then)")
            } else {
                log.w("session id=${s.id} discarding a permission-dialog observation from ${ageMs}ms ago — older than ${PENDING_PROMPT_WINDOW_MS}ms, not plausibly this session's")
            }
        }
        log.i(
            "session id=${s.id} begin origin=${attach.origin.tag} uid=${attach.uid} user=${attach.userId} " +
                "perm_trampoline=${attach.hasPermissionAtTrampoline ?: "none"} serial=${attach.serialOutcome.tag}"
        )
        return s
    }

    /**
     * [begin] for a session that has NO attach: the process was brought up by the launcher, `am start`
     * or a task switch with the adapter already present, and `OcbmProbe.runAllLocked` is about to
     * bring the link up without the trampoline ever having run for this session.
     *
     * Everything the trampoline would have measured and cannot be measured honestly here is the
     * explicit absent token: `hasPermissionAtTrampoline` is null (rendered `perm_trampoline=none`)
     * because sampling `hasPermission()` now would be a real fact wearing a trampoline fact's label,
     * and `serialOutcome` is [SerialOutcome.UNKNOWN] because its `SECURITY_EXCEPTION` value is
     * defined as "the attach-time grant did not land". The descriptor fingerprint IS taken: it is a
     * device-identity fact, the same whenever it is read, and it is what lets a launch-origin session
     * be compared with an attach-origin one for the same box.
     */
    fun beginLaunch(dev: UsbDevice): Session = begin(
        AttachInfo(
            origin = Origin.LAUNCH,
            hasPermissionAtTrampoline = null,
            uid = android.os.Process.myUid(),
            userId = android.os.Process.myUid() / 100_000,
            processAgeMs = SystemClock.elapsedRealtime() - android.os.Process.getStartElapsedRealtime(),
            serialOutcome = SerialOutcome.UNKNOWN,
            descriptorFingerprint = descriptorFingerprint(dev),
        )
    )

    /**
     * The USB permission dialog was just raised by this app. Recorded on the open session if there
     * is one; otherwise held for the session that begins within [PENDING_PROMPT_WINDOW_MS] — see
     * that constant for the launch-then-plug-in race this closes. Callers should use this rather
     * than `current()?.permissionDialogObserved()`, which silently drops the observation in that race.
     */
    fun permissionDialogObserved() {
        // Decided under the same lock begin() takes, so the observation lands on exactly one of: the
        // session open now, or the pending slot the next begin() drains. No window in between.
        val s: Session?
        synchronized(lock) {
            s = current
            if (s == null) pendingPromptAt = SystemClock.elapsedRealtime()
        }
        s?.permissionDialogObserved()
    }

    /** The session started by the most recent [begin] that has not yet [end]ed, or null before any attach. */
    fun current(): Session? = current

    /** Clean or diagnosed close. `exitReason` is a short snake_case token, e.g. `ct_stop`, `usb_detach`,
     *  `heartbeat_dead`, `activity_destroyed`. Idempotent: a session already flushed (superseded, or by
     *  the shutdown hook) emits nothing a second time. */
    fun end(session: Session, exitReason: String) {
        emit(session, exitReason)
        synchronized(lock) { if (current === session) current = null }
    }

    /**
     * Build the descriptor-shape fingerprint used by [AttachInfo.descriptorFingerprint]: config count,
     * interface count, and each interface's class/subclass/protocol. The permission cache is keyed on
     * device identity, and `/script/ncm_only` flips the CCPA between an NCM shell-bearing descriptor and
     * a pure OCBM accessory (see `UsbAttachActivity` KDoc) — a fingerprint change on the same
     * VID:PID across two attaches means the cache is looking at a different device, not a flaky grant.
     */
    fun descriptorFingerprint(dev: UsbDevice): String {
        val ifaces = (0 until dev.interfaceCount).joinToString(",") { i ->
            val itf = dev.getInterface(i)
            "${itf.interfaceClass}/${itf.interfaceSubclass}/${itf.interfaceProtocol}"
        }
        return "c${dev.configurationCount}i${dev.interfaceCount}:$ifaces"
    }

    private fun emit(s: Session, exitReason: String) {
        if (!s.closed.compareAndSet(false, true)) return // already flushed (race between end() and shutdown hook)
        val nowElapsed = SystemClock.elapsedRealtime()
        val durMs = nowElapsed - s.startElapsedMs

        val phoneModel = jsonField(s.phoneIdentJson, "model")
        val phoneOs = jsonField(s.phoneIdentJson, "osVersion")
        val phoneDeviceId = jsonField(s.phoneIdentJson, "deviceID") // BR/EDR MAC — see class KDoc, not redacted here

        val bondStart = bondedList(s.mgmtInfoStartJson)
        val bondEnd = bondedList(s.mgmtInfoEndJson)
        val bondStartN = bondStart?.size?.toString() ?: "none"
        val bondEndN = bondEnd?.size?.toString() ?: "none"
        val bondChanged = when {
            bondStart == null || bondEnd == null -> "unknown"
            else -> (bondStart.toSet() != bondEnd.toSet()).toString()
        }

        val btpMax = s.highestBtpName()
        val reachedHandoff = s.highestBtpValue() >= (Ocbm.BTP_WIFI_HANDOFF.toInt() and 0xFF)

        // A/V counters: onAvFinal only ever runs on the clean path (HevcRenderer.stop()), so on
        // superseded/process_death/host_gone the pushed values are stale zeros while the renderer is
        // still counting. Sample the live source here and take the larger of the two — the pushed
        // value still wins when the renderer was detached before this emit.
        val sampled = s.avCountersSource?.let { src -> runCatching { src() }.getOrNull() }
        val frames = maxOf(s.framesRendered, sampled?.getOrElse(0) { 0L } ?: 0L)
        val auDrop = maxOf(s.ausDropped, sampled?.getOrElse(1) { 0L } ?: 0L)
        val avBytes = maxOf(s.avBytesIn, sampled?.getOrElse(2) { 0L } ?: 0L)
        val noVideo = frames <= 0L

        val line = buildString {
            append("SESSION v=$SCHEMA_VERSION")
            append(" id=${s.id}")
            append(" start_epoch_ms=${s.startWallClockMs}")
            append(" dur_ms=$durMs")
            append(" uid=${s.attach.uid}")
            append(" user=${s.attach.userId}")
            append(" proc_age_ms=${s.attach.processAgeMs}")
            append(" perm_trampoline=${s.attach.hasPermissionAtTrampoline ?: "none"}")
            append(" serial=${s.attach.serialOutcome.tag}")
            append(" desc_fp=${sanitize(s.attach.descriptorFingerprint)}")
            append(" prompted=${s.dialogPrompted}")
            append(" btp_max=$btpMax")
            append(" proj_mode=${s.projModeName()}")
            append(" mfi=${s.mfiFailureStatusName ?: "none"}")
            append(" sev_last=${s.lastSevName()}")
            append(" sev_count=${s.sevCount()}")
            append(" phone_model=${sanitize(phoneModel)}")
            append(" phone_os=${sanitize(phoneOs)}")
            append(" phone_device_id=${sanitize(phoneDeviceId)}")
            append(" bond_start_n=$bondStartN")
            append(" bond_end_n=$bondEndN")
            append(" bond_changed=$bondChanged")
            append(" ttff_ms=${if (s.timeToFirstFrameMs < 0) "none" else s.timeToFirstFrameMs.toString()}")
            append(" frames=$frames")
            append(" au_drop=$auDrop")
            append(" av_bytes=$avBytes")
            append(" no_video=$noVideo")
            append(" reached_handoff=$reachedHandoff")
            append(" exit=${sanitize(exitReason)}")
            // v=2: appended at the END, per the format contract — nothing above moved.
            append(" origin=${s.attach.origin.tag}")
        }
        log.i(line)

        // Detail block: session-end only, one key per line, for the fields too long/structured for the
        // one-liner above. Same "none" discipline — a missing snapshot is explicit, not a blank line.
        log.i("SESSION_DETAIL id=${s.id} phone_ident_json=${orNone(s.phoneIdentJson)}")
        log.i("SESSION_DETAIL id=${s.id} bond_start_json=${orNone(s.mgmtInfoStartJson)}")
        log.i("SESSION_DETAIL id=${s.id} bond_end_json=${orNone(s.mgmtInfoEndJson)}")
        log.i("SESSION_DETAIL id=${s.id} bond_start_list=${bondStart?.toString() ?: "none"}")
        log.i("SESSION_DETAIL id=${s.id} bond_end_list=${bondEnd?.toString() ?: "none"}")
        log.i("SESSION_DETAIL id=${s.id} sev_history=${s.sevHistoryNames()}")
    }

    // ---- small parsing helpers, all defensive: a malformed/partial JSON must never abort the emit ----

    private fun jsonField(json: String, key: String): String =
        if (json.isEmpty()) "none" else runCatching { JSONObject(json).optString(key, "none") }.getOrDefault("none")

    /** `MGMT_INFO`'s `"devices":[...]` is a flat array of MAC strings (`ocbmd::box_info_json`), not
     *  objects — see `ccpa_custom/ccpa/ocbmd/src/main.rs`. Null return means "no snapshot taken", which
     *  is distinct from "snapshot taken, zero bonded devices". */
    private fun bondedList(json: String?): List<String>? {
        if (json.isNullOrEmpty()) return null
        return runCatching {
            val arr: JSONArray = JSONObject(json).optJSONArray("devices") ?: return@runCatching emptyList<String>()
            (0 until arr.length()).map { arr.optString(it, "") }
        }.getOrNull()
    }

    private fun orNone(s: String?): String = if (s.isNullOrEmpty()) "none" else s

    /** Keep the one-liner's `key=value` tokens unambiguous: collapse whitespace a free-text field
     *  (phone name/model, exit reason) could otherwise inject. */
    private fun sanitize(s: String): String {
        if (s.isEmpty() || s == "none") return "none"
        return s.replace(Regex("\\s+"), "_")
    }

    // ------------------------------------------------------------------------------------------------

    /** How the session came to exist. Rendered as the `origin=` token; see [AttachInfo.origin]. */
    enum class Origin(val tag: String) {
        /** The framework launched `UsbAttachActivity` for the adapter; every [AttachInfo] field is real. */
        USB_ATTACH("usb_attach"),
        /** Launcher / `am start` / task switch with the adapter already present; no trampoline ran for
         *  this session, so the attach-only fields carry the absent token. Built by [beginLaunch]. */
        LAUNCH("launch"),
    }

    /**
     * Facts about how the session began, immutable for its life. For [Origin.USB_ATTACH] they are
     * captured once at the trampoline (`UsbAttachActivity`) — see that class's KDoc for what each
     * field kills. For [Origin.LAUNCH] the trampoline never ran, and the fields it alone can measure
     * are null/[SerialOutcome.UNKNOWN] rather than a value sampled elsewhere and mislabelled.
     */
    data class AttachInfo(
        val origin: Origin,
        /** Null ONLY for [Origin.LAUNCH]: there was no trampoline to sample at. Rendered `none`. A
         *  `false` here would satisfy `grep perm_trampoline=false` — the fault-1 query — for a session
         *  in which no grant was ever expected at that point. */
        val hasPermissionAtTrampoline: Boolean?,
        val uid: Int,
        val userId: Int,
        val processAgeMs: Long,
        val serialOutcome: SerialOutcome,
        /** From [descriptorFingerprint]; opaque here, compared as a string across sessions. */
        val descriptorFingerprint: String
    )

    /** `getSerialNumber()` is permission-gated since API 29: [SECURITY_EXCEPTION] IS the fault signal —
     *  it means the grant did not land for this device, not merely that the serial is unavailable. */
    enum class SerialOutcome(val tag: String) {
        OK("ok"), NULL_SERIAL("null"), SECURITY_EXCEPTION("sec_exception"), UNKNOWN("unknown")
    }

    /**
     * The mutable per-session record. Obtained from [begin]; facts are pushed in from whichever thread
     * learns them first (OCBM reader thread for the box-side state, UI thread for the permission dialog
     * and lifecycle, HEVC render thread for A/V outcome) and read back only at [emit] time.
     */
    class Session internal constructor(
        val id: Long,
        val attach: AttachInfo
    ) {
        val startWallClockMs: Long = System.currentTimeMillis()
        /** Monotonic baseline for [timeToFirstFrameMs] and duration — wall clock can jump (CT_SETTIME
         *  lands mid-session on a box with no RTC battery), elapsedRealtime cannot. */
        val startElapsedMs: Long = SystemClock.elapsedRealtime()

        internal val closed = AtomicBoolean(false)

        // ---- fault 1: was the USB permission dialog observed this attach? ----
        @Volatile var dialogPrompted: Boolean = false; private set
        /** Fed from the log-stream watcher (the caller owns detecting the dialog; this just records it). */
        fun permissionDialogObserved() { dialogPrompted = true }

        // ---- fault 2 + general BT/session state, from the OCBM reader thread ----
        // Tracked as the raw unsigned byte value rather than the enum so an out-of-band future BTP_*
        // still participates in "reached furthest" (advisory/monotonic-ish, per Ocbm.btpName's own
        // contract: treat an unknown value as progress, never gate on ordering).
        private val highestBtpRaw = AtomicInteger(-1)
        fun onBtPhase(phase: Byte) {
            val v = phase.toInt() and 0xFF
            while (true) {
                val cur = highestBtpRaw.get()
                if (v <= cur) break
                if (highestBtpRaw.compareAndSet(cur, v)) break
            }
        }
        internal fun highestBtpValue(): Int = highestBtpRaw.get()
        internal fun highestBtpName(): String {
            val v = highestBtpRaw.get()
            return if (v < 0) "none" else Ocbm.btpName(v.toByte())
        }

        @Volatile private var projMode: Byte = -1
        fun onProjMode(mode: Byte) { projMode = mode }
        internal fun projModeName(): String = if (projMode < 0) "none" else Ocbm.pmName(projMode)

        /** Set on an MFi failure/unsupported response (e.g. `Mfi.Response.statusName()`); left null on
         *  a session with no MFi traffic or where every request succeeded. */
        @Volatile var mfiFailureStatusName: String? = null
        fun onMfiFailure(statusName: String) { mfiFailureStatusName = statusName }

        // Bounded so a session stuck flapping HOST_PRESENT/HOST_GONE can't grow this unboundedly; the
        // cap is generous for a real session (a healthy one emits a handful of events total) and a
        // session that hits it is itself a finding visible in sev_count.
        private val sevHistory = Collections.synchronizedList(ArrayList<Byte>(8))
        fun onSessionEvent(sev: Byte) {
            synchronized(sevHistory) { if (sevHistory.size < MAX_SEV_HISTORY) sevHistory.add(sev) }
        }
        internal fun lastSevName(): String =
            synchronized(sevHistory) { sevHistory.lastOrNull() }?.let { Ocbm.sevName(it) } ?: "none"
        internal fun sevCount(): Int = synchronized(sevHistory) { sevHistory.size }
        internal fun sevHistoryNames(): List<String> =
            synchronized(sevHistory) { sevHistory.map { Ocbm.sevName(it) } }

        /** Raw `CT_PHONE_IDENT` JSON, or "" if never identified this session. */
        @Volatile var phoneIdentJson: String = ""; private set
        fun onPhoneIdent(json: String) { phoneIdentJson = json }

        /** `MGMT_INFO` JSON snapshots at session start and end. Capture BOTH ends deliberately — a diff
         *  between them is the bond-asymmetry signature behind fault 2 (iOS shows "known", box's own
         *  bonded list disagrees), and a single end-of-session snapshot alone can't show a diff. */
        @Volatile var mgmtInfoStartJson: String? = null; private set
        @Volatile var mgmtInfoEndJson: String? = null; private set
        fun onMgmtInfoStart(json: String) { mgmtInfoStartJson = json }
        fun onMgmtInfoEnd(json: String) { mgmtInfoEndJson = json }

        // ---- A/V outcome, from the HEVC render thread ----
        /** Emitted as `ttff_ms`: **USB attach -> first frame**, not dial-to-first-frame — the baseline
         *  is [startElapsedMs], stamped by [begin] at attach. A large value therefore includes the whole
         *  BT/Wi-Fi handoff and dial, and is not by itself a decode fault.
         *
         *  -1 until the first frame is observed. Call once, from wherever the caller detects it (e.g.
         *  watching `HevcRenderer.framesRendered` go 0->1, or the "FIRST FRAME RENDERED" log line) —
         *  this class does not touch HevcRenderer directly so it stays off the decode hot path. */
        @Volatile var timeToFirstFrameMs: Long = -1; private set
        fun onFirstFrameObserved() {
            if (timeToFirstFrameMs < 0) timeToFirstFrameMs = SystemClock.elapsedRealtime() - startElapsedMs
        }

        @Volatile var framesRendered: Long = 0; private set
        @Volatile var ausDropped: Long = 0; private set
        @Volatile var avBytesIn: Long = 0; private set

        /**
         * Live counter source, read once per emit. Returns `longArrayOf(framesRendered, ausDropped,
         * bytesIn)`; must be cheap and non-throwing (it is invoked under `runCatching` and a throw or a
         * short array simply yields the pushed values). Set by whoever owns the renderer when it is
         * attached, cleared to null when it is torn down. Read-only sampling — this is not a hook onto
         * the per-frame path.
         */
        @Volatile var avCountersSource: (() -> LongArray)? = null

        /** Read `HevcRenderer`'s counters ONCE at teardown and hand them the final values — never wire
         *  this into the per-frame path (see class KDoc "cost" section). Still the clean-path value:
         *  it covers the renderer being detached before [end] runs. */
        fun onAvFinal(framesRendered: Long, ausDropped: Long, bytesIn: Long) {
            this.framesRendered = framesRendered
            this.ausDropped = ausDropped
            this.avBytesIn = bytesIn
        }
    }
}
