package zeno.gmccpa.bench

import android.content.Context
import android.content.Intent
import zeno.gmccpa.ProbeLog

/**
 * RELEASE stub of the bench-only operator hooks (plan v5.9 MIC). `tools/build_apk.sh` compiles this
 * file unless `GMCCPA_BENCH=1`; the real hooks live in src/bench and are never in a release APK.
 *
 * It reads no property, touches no file or socket and never calls `MicInject.arm`, so a release
 * build cannot arm a mic injection whatever an exported-activity caller sends.
 */
object BenchHooks {
    fun micInject(@Suppress("UNUSED_PARAMETER") ctx: Context, @Suppress("UNUSED_PARAMETER") i: Intent) {
        ProbeLog.sub("mic").i("mic_inject: not in this build (release)")
    }

    /** plan v5.9 D6: the bench-only `finish_launcher` verb; a release build only logs. */
    fun finishLauncher(@Suppress("UNUSED_PARAMETER") act: android.app.Activity) {
        ProbeLog.sub("mic").i("finish_launcher: not in this build (release)")
    }
}
