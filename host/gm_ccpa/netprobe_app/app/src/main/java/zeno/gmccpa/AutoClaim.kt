package zeno.gmccpa

import android.content.Context
import android.os.UserManager
import zeno.gmccpa.av.CarPlaySessionService
import zeno.gmccpa.logging.CapturePrefs
import zeno.gmccpa.logging.LogCapture

/**
 * plan v5.9 D6 (D006, owner 2026-10-08 "fix D006"): claim the adapter when it attaches, when the box
 * reboots and at head-unit boot — with the app closed and without showing any UI. Three triggers call
 * [trigger]:
 *  - T1 `usb-attach`: [AdapterAttachReceiver] (manifest, exported=false; system_server's broadcast);
 *  - T2 `boot`: logging.BootReceiver (BOOT_COMPLETED, ~+56 s after unlock, with the boot FGS allowlist);
 *  - T3 `media-browser bind`: CarPlayMediaBrowserService.onCreate (~+7 s at boot when CCPA is the source).
 *
 * This is possible without a prompt only because the USB grant persists (docs/host/01 §"Bench
 * verification of the persistent USB grant"); nothing here ever requests permission or starts an
 * activity. The phone's arrival still raises CarPlayActivity through SessionCore (SYSTEM_ALERT_WINDOW
 * admits that background launch). An operator Stop suppresses every trigger for the rest of that
 * head-unit boot (OD-9D, [AutoClaimPrefs]).
 */
object AutoClaim {
    private val log = ProbeLog.sub("autoclaim")

    enum class Origin(val tag: String, val summary: zeno.gmccpa.logging.SessionSummary.Origin) {
        USB_ATTACH("usb-attach", zeno.gmccpa.logging.SessionSummary.Origin.AUTO_ATTACH),
        BOOT("boot", zeno.gmccpa.logging.SessionSummary.Origin.AUTO_BOOT),
        MEDIA_BIND("media-browser bind", zeno.gmccpa.logging.SessionSummary.Origin.AUTO_BIND),
    }

    /** Main thread; cheap; never throws. Steps 1-6 of the D6 design. */
    fun trigger(ctx: Context, origin: Origin) {
        try {
            val app = ctx.applicationContext
            val tag = "auto-claim (${origin.tag})"
            // 1. The user-0 copy (installed, stopped, PKG:21039) never claims: the adapter belongs to
            //    the driver's user.
            if (runCatching { app.getSystemService(UserManager::class.java)?.isSystemUser == true }.getOrDefault(false)) {
                log.i("$tag: ignored — system user"); return
            }
            // 2. OD-9D: an operator Stop holds for the rest of this boot.
            if (AutoClaimPrefs.suppressed(app)) { log.i("$tag: suppressed — operator Stop"); return }
            // 3. R6: no FGS (a microphone-type one, with its notification) for a box that is not there.
            val core = SessionHolder.core(app)
            val st = core.readAdapterState()
            val a = SessionCore.adapterKind(st)
            if (a != AutoClaimPolicy.Adapter.PRESENT) {
                val d = when (a) {
                    AutoClaimPolicy.Adapter.ABSENT -> AutoClaimPolicy.Decision.SKIP_ABSENT
                    AutoClaimPolicy.Adapter.WRONG_MODE -> AutoClaimPolicy.Decision.SKIP_WRONG_MODE
                    else -> AutoClaimPolicy.Decision.SKIP_NO_GRANT
                }
                log.i("$tag: ${d.name} — ${d.why} (pre-check, no FGS started; adapter $st)")
                return
            }
            // 4. Capture, as every entry point does (idempotent).
            runCatching { LogCapture.start(app, CapturePrefs.config(app)) }
            // 5. The LINK hold, BEFORE the decision (I3: a boot T2 must acquire even when the policy
            //    will say NOOP, if T3's start was refused).
            val acquired = acquireLink(app, origin.tag)
            // 6. The decision and the claim run on netprobe-cmd.
            core.requestAutoClaim(origin, acquired)
        } catch (t: Throwable) {
            log.e("auto-claim (${origin.tag}): trigger failed — ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /**
     * Place the LINK hold if nothing placed it yet. True only if THIS call added it (D6 rev b R5: a
     * task releases only what it acquired). A refusal (ForegroundServiceStartNotAllowedException:
     * no SAW, no allowlist) is logged and swallowed — the claim then runs unheld (I6, U59-22).
     */
    fun acquireLink(ctx: Context, why: String): Boolean {
        if (CarPlaySessionService.holds(CarPlaySessionService.LINK_TOKEN)) return false
        return runCatching { CarPlaySessionService.start(ctx.applicationContext, CarPlaySessionService.LINK_TOKEN) }
            .onSuccess { log.i("link hold acquired ($why)") }
            .onFailure { log.w("link hold refused ($why): ${it.javaClass.simpleName}: ${it.message}") }
            .isSuccess
    }

    /** Drop the LINK hold if it is held. Every releaser goes through here. */
    fun releaseLink(ctx: Context, why: String) {
        if (!CarPlaySessionService.holds(CarPlaySessionService.LINK_TOKEN)) return
        runCatching { CarPlaySessionService.stop(ctx.applicationContext, CarPlaySessionService.LINK_TOKEN) }
            .onSuccess { log.i("link hold released ($why)") }
            .onFailure { log.w("link hold release failed ($why): ${it.message}") }
    }
}

/**
 * OD-9D (owner 2026-10-08): an operator Stop suppresses the auto-claim for the rest of the head-unit
 * boot, and NOT after a reboot. D6 rev c C2: "same boot" is a real boot identity —
 * `Settings.Global.BOOT_COUNT`, else `/proc/sys/kernel/random/boot_id` — stored at the Stop and
 * compared now ([AutoClaimPolicy.sameBoot]); neither readable -> not suppressed. The in-memory flag
 * covers the live process whatever the identity (an unreadable identity costs only a Stop surviving a
 * same-boot process death). File `autoclaim` (CE storage; the triggers run after unlock).
 */
object AutoClaimPrefs {
    private const val FILE = "autoclaim"
    private const val KEY_STOPPED = "stopped"
    private const val KEY_COUNT = "stopped_boot_count"
    private const val KEY_ID = "stopped_boot_id"

    @Volatile private var stoppedThisProcess = false

    private fun prefs(ctx: Context) = ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    /*
     * The boot identity, cached per PROCESS (rc2 review NIT): a process never outlives the boot it
     * started in, so the first good read is the answer for its whole life. [suppressed] runs inside
     * the claim-wait poll lambdas (several times a second for up to 20 s), which used to re-read
     * Settings.Global and /proc on every poll. A failed read (-1 / "") is not cached and is retried.
     */
    @Volatile private var cachedBootCount = -1
    @Volatile private var cachedBootId = ""

    private fun bootCount(ctx: Context): Int {
        cachedBootCount.takeIf { it != -1 }?.let { return it }
        return runCatching { android.provider.Settings.Global.getInt(ctx.contentResolver, android.provider.Settings.Global.BOOT_COUNT, -1) }
            .getOrDefault(-1).also { if (it != -1) cachedBootCount = it }
    }

    private fun bootId(): String {
        cachedBootId.takeIf { it.isNotEmpty() }?.let { return it }
        return runCatching { java.io.File("/proc/sys/kernel/random/boot_id").readText().trim() }
            .getOrDefault("").also { if (it.isNotEmpty()) cachedBootId = it }
    }

    /**
     * The operator pressed Stop (main thread, before the stop is queued). Returns the identity name used.
     *
     * `apply()`, not `commit()` (rc2 review MINOR): the ordering the plan needs — the flag visible to
     * the queued stop and to every later [suppressed] in this process — comes from [stoppedThisProcess]
     * and SharedPreferences' in-memory map, both updated synchronously here; only the flash write moves
     * off the main thread (Android waits for pending apply()s at the next Activity/Service lifecycle step).
     */
    fun markStopped(ctx: Context): String {
        stoppedThisProcess = true
        val c = bootCount(ctx); val id = bootId()
        runCatching { prefs(ctx).edit().putBoolean(KEY_STOPPED, true).putInt(KEY_COUNT, c).putString(KEY_ID, id).apply() }
        return AutoClaimPolicy.identityName(c, id)
    }

    /** Start, Restart (and the merged Recover-as-Start): the operator wants the adapter again. */
    fun clear(ctx: Context) {
        val had = stoppedThisProcess
        stoppedThisProcess = false
        runCatching {
            val p = prefs(ctx)
            // apply(): same reasoning as [markStopped] — memory first, flash off the main thread.
            if (had || p.getBoolean(KEY_STOPPED, false)) p.edit().remove(KEY_STOPPED).remove(KEY_COUNT).remove(KEY_ID).apply()
        }
    }

    /** True while a Stop of THIS boot stands. */
    fun suppressed(ctx: Context): Boolean {
        if (stoppedThisProcess) return true
        return runCatching {
            val p = prefs(ctx)
            p.getBoolean(KEY_STOPPED, false) &&
                AutoClaimPolicy.sameBoot(p.getInt(KEY_COUNT, -1), p.getString(KEY_ID, "") ?: "", bootCount(ctx), bootId())
        }.getOrDefault(false)
    }
}
