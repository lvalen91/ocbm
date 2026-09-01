package com.carlink.projection

import android.app.Activity
import android.app.ActivityManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.os.IBinder
import com.carlink.logging.ProbeLog

/**
 * The launcher tile: "CarPlay" on the AAOS home screen.
 *
 * This is a trampoline with no UI of its own. It decides between three outcomes and finishes
 * immediately, so it never appears in the back stack.
 *
 * | State | What happens |
 * |---|---|
 * | Session running | Bring the existing projection task forward — **never a relaunch** |
 * | No session | Open the device screen so the driver can connect |
 * | Service not up yet | Start it, then fall through to one of the above |
 *
 * ## Why not just point the launcher at ProjectionActivity
 *
 * Because a launcher icon that starts an activity does exactly that: with no session it would open a
 * black surface with nothing behind it, and the driver has no route to the device list. Deciding
 * here is what makes one icon correct in both states.
 *
 * The "bring forward" path is GM's mechanism exactly (`MANAGE_ACTIVITY_TASKS` / `REAL_GET_TASKS` →
 * `moveTaskToFront`, per §3). `moveTaskToFront` is used when we hold the permission and the task is
 * findable; `FLAG_ACTIVITY_REORDER_TO_FRONT` is the fallback that needs no permission at all and
 * achieves the same thing for our own task.
 *
 * ## Why the theme is transparent rather than `Theme.NoDisplay`
 *
 * `NoDisplay` is the obvious choice for a trampoline and it crashes here:
 *
 * ```
 * java.lang.IllegalStateException: Activity ... did not call finish() prior to onResume() completing
 * ```
 *
 * A `NoDisplay` activity MUST finish synchronously within `onCreate`/`onResume`. Our decision needs
 * the service's session state, and `bindService` delivers that on a later main-loop turn — so we
 * cannot finish in time by construction. A transparent theme has no such rule, and the user sees
 * nothing either way because we finish as soon as the bind lands.
 *
 * There is a deadline as well: if the bind never completes we would sit invisible forever, so
 * [BIND_DEADLINE_MS] routes to the device screen rather than leaving the driver with a dead icon.
 */
class LaunchTileActivity : Activity() {
    private val log = ProbeLog.sub("tile")

    private var bound = false

    /** Guards against routing twice — the bind callback and the deadline can race. */
    private val routed =
        java.util.concurrent.atomic
            .AtomicBoolean(false)

    private val deadline = android.os.Handler(android.os.Looper.getMainLooper())

    private val conn =
        object : ServiceConnection {
            override fun onServiceConnected(
                name: ComponentName?,
                binder: IBinder?,
            ) {
                val svc = (binder as? ProjectionService.LocalBinder)?.service
                route(svc?.status?.state?.value)
            }

            override fun onServiceDisconnected(name: ComponentName?) = Unit
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ProjectionService.start(this)
        // Bind rather than guess: the session state lives in the service, and starting the wrong
        // screen is the difference between "return to CarPlay" and "restart CarPlay".
        bound = bindService(Intent(this, ProjectionService::class.java), conn, Context.BIND_AUTO_CREATE)
        if (!bound) {
            log.w("could not bind projection service — falling back to the device screen")
            route(null)
            return
        }
        // The bind should land in milliseconds, but "should" is not a guarantee and an invisible
        // activity that never finishes is a dead launcher icon with no way to report itself.
        deadline.postDelayed({
            if (!routed.get()) {
                log.w("projection service did not bind within ${BIND_DEADLINE_MS}ms — device screen")
                route(null)
            }
        }, BIND_DEADLINE_MS)
    }

    private fun route(state: SessionStatus.State?) {
        if (!routed.compareAndSet(false, true)) return
        deadline.removeCallbacksAndMessages(null)
        if (state != null && state.sessionActive) {
            log.i("session active → bringing projection forward")
            if (!moveExistingTaskToFront()) ProjectionActivity.bringToFront(this)
        } else {
            log.i("no session → opening the device screen")
            startActivity(
                Intent(this, DeviceManagerActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }
        finish()
    }

    /**
     * GM's path. Requires `REAL_GET_TASKS` to see our own task in `getAppTasks` on a modern
     * platform; without the privileged grant this returns false and the caller uses the intent-flag
     * route, which is not as precise but needs nothing.
     */
    private fun moveExistingTaskToFront(): Boolean =
        try {
            val am = getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
            val target = ProjectionActivity::class.java.name
            val task =
                am.appTasks.firstOrNull { t ->
                    // taskInfo is nullable on modern platforms: a task can be reaped between the
                    // list snapshot and this read.
                    val info = t.taskInfo
                    info?.baseIntent?.component?.className == target ||
                        info?.topActivity?.className == target
                }
            if (task != null) {
                task.moveToFront()
                true
            } else {
                false
            }
        } catch (e: SecurityException) {
            log.i("moveTaskToFront unavailable (${e.message}) — using REORDER_TO_FRONT")
            false
        } catch (e: Exception) {
            log.w("moveTaskToFront failed: $e")
            false
        }

    private companion object {
        /** Generous: this only fires when something is badly wrong with the service. */
        const val BIND_DEADLINE_MS = 3000L
    }

    override fun onDestroy() {
        deadline.removeCallbacksAndMessages(null)
        if (bound) {
            runCatching { unbindService(conn) }
            bound = false
        }
        super.onDestroy()
    }
}
