package zeno.gmccpa.logging

import android.content.Context

/**
 * The one persisted knob the capture engine needs: which scope to run at.
 *
 * Kept out of [LogCapture] on purpose — the engine takes a [LogCapture.Config] and has no opinion on
 * where it came from, which is what let it be unit-checked in isolation. This is the boot receiver's
 * and the UI's shared view of the operator's choice, and nothing else.
 *
 * Default is [LogCapture.Scope.WHOLE_OS] rather than the safer-sounding own-process. That is
 * deliberate: whole-OS DEGRADES on its own when `READ_LOGS` is not granted (the engine probes, falls
 * back, and says so in the file header), so defaulting to it costs nothing on an ungranted unit and
 * means a freshly-granted unit starts capturing the system side without anyone remembering to come
 * back and flip a switch. The failure mode of the opposite default — a granted unit quietly
 * capturing only our own process for a week of drives — is the expensive one.
 */
object CapturePrefs {
    private const val FILE = "capture"
    private const val KEY_SCOPE = "scope"

    fun scope(ctx: Context): LogCapture.Scope =
        runCatching {
            LogCapture.Scope.valueOf(
                prefs(ctx).getString(KEY_SCOPE, LogCapture.Scope.WHOLE_OS.name)!!
            )
        }.getOrDefault(LogCapture.Scope.WHOLE_OS)

    fun setScope(ctx: Context, scope: LogCapture.Scope) {
        prefs(ctx).edit().putString(KEY_SCOPE, scope.name).apply()
    }

    /** The config the boot receiver, the USB trampoline and the UI all start from. */
    fun config(ctx: Context, appVersion: String): LogCapture.Config =
        LogCapture.Config(scope = scope(ctx), appVersion = appVersion)

    private fun prefs(ctx: Context) =
        ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
}
