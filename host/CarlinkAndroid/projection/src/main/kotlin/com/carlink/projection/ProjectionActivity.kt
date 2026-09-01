package com.carlink.projection

import android.app.Activity
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.os.IBinder
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowManager
import com.carlink.logging.ProbeLog

/**
 * The projection surface. A `SurfaceView` and input forwarding — nothing else.
 *
 * Per `pi/docs/01` §7.2 this is deliberately thin and cheap to create and destroy. It **attaches to**
 * a running session; it never starts or stops one. Everything durable — the seam, the key, the
 * decoder's parameter sets, audio — lives in [ProjectionService].
 *
 * The practical consequence: pressing Home here stops video and leaves music playing, and coming
 * back re-attaches a decoder to a session that never dropped. That is the behaviour the oemIcon flow
 * and the launcher tile both depend on.
 *
 * ## Not the HOME app
 *
 * §1 settled that projection is an ordinary activity plus a launcher tile, not the system Home. So
 * there is no `HOME` intent filter here — leaving AAOS's own launcher in place is what makes
 * "navigate out of CarPlay and back" mean anything.
 */
class ProjectionActivity : Activity() {
    private val log = ProbeLog.sub("ui")

    private var service: ProjectionService? = null
    private var surfaceView: SurfaceView? = null
    private var forwarder: TouchForwarder? = null
    private var bound = false

    /** Set once the Surface exists, so a late service bind still attaches. */
    private var pendingSurface: SurfaceHolder? = null

    private val conn =
        object : ServiceConnection {
            override fun onServiceConnected(
                name: ComponentName?,
                binder: IBinder?,
            ) {
                val svc = (binder as? ProjectionService.LocalBinder)?.service ?: return
                service = svc
                forwarder = TouchForwarder(svc.inputSender)
                svc.setUiForeground(true)
                // The Surface may have been created before the bind completed — that ordering is not
                // guaranteed and depends on how fast the service starts. Attaching here covers it.
                pendingSurface?.let { svc.attachSurface(it.surface) }
                log.i("bound to projection service")
            }

            override fun onServiceDisconnected(name: ComponentName?) {
                log.w("projection service disconnected")
                service = null
                forwarder = null
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Keep the screen on while projecting. A head unit that dims mid-navigation is worse than
        // useless, and this is the window-level flag rather than a wake lock so it is scoped to the
        // activity being visible — no lock to leak if we are killed.
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        val sv = SurfaceView(this)
        sv.holder.addCallback(
            object : SurfaceHolder.Callback {
                override fun surfaceCreated(holder: SurfaceHolder) {
                    pendingSurface = holder
                    service?.attachSurface(holder.surface)
                }

                override fun surfaceChanged(
                    holder: SurfaceHolder,
                    format: Int,
                    width: Int,
                    height: Int,
                ) {
                    // Re-attach: a format or size change invalidates the codec's output surface, and
                    // MediaCodec cannot be re-pointed at a new Surface without a reconfigure.
                    pendingSurface = holder
                    service?.attachSurface(holder.surface)
                }

                override fun surfaceDestroyed(holder: SurfaceHolder) {
                    pendingSurface = null
                    // MUST be synchronous. Returning from surfaceDestroyed while the decoder still
                    // holds the Surface is a native crash, not an exception — detachSurface joins
                    // the consumer thread for exactly this reason.
                    service?.detachSurface()
                }
            },
        )
        sv.setOnTouchListener { v, e ->
            forwarder?.onTouch(e, v.width, v.height)
            true
        }
        surfaceView = sv
        setContentView(sv)
        goFullscreen(sv)
    }

    /**
     * Full-bleed, and let CarPlay own every pixel it was told it had.
     *
     * The advertised `displayPanelsConfig` is the whole panel, so anything the system draws over the
     * surface — status bar, navigation bar — covers CarPlay content that iOS believes is visible,
     * and touch coordinates would be normalised against a view smaller than the advertised panel.
     */
    private fun goFullscreen(v: View) {
        v.systemUiVisibility = (
            View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_FULLSCREEN
                or View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
        )
    }

    override fun onStart() {
        super.onStart()
        // Make sure the owner exists even if we were launched directly (notification, adb, a
        // deep link) rather than by the tile.
        ProjectionService.start(this)
        bound = bindService(Intent(this, ProjectionService::class.java), conn, Context.BIND_AUTO_CREATE)
    }

    override fun onResume() {
        super.onResume()
        service?.setUiForeground(true)
    }

    override fun onPause() {
        super.onPause()
        // The session keeps running; only our claim on the foreground ends. This is what flips the
        // AAOS projection state from ACTIVE_FOREGROUND to ACTIVE_BACKGROUND.
        service?.setUiForeground(false)
    }

    override fun onStop() {
        super.onStop()
        if (bound) {
            runCatching { unbindService(conn) }
            bound = false
        }
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        // Covers taps that miss the SurfaceView (letterbox margins when the stream's aspect ratio
        // differs from the panel's). Without this a touch in the margin is silently swallowed.
        val v = surfaceView
        if (v != null) forwarder?.onTouch(event, v.width, v.height)
        return true
    }

    companion object {
        /**
         * Bring projection forward without restarting it.
         *
         * `FLAG_ACTIVITY_REORDER_TO_FRONT` on an existing task is the Activity-level equivalent of
         * GM's `moveTaskToFront`, and unlike a plain `startActivity` it does not recreate the
         * activity or restart the session. Combined with `launchMode="singleTask"` in the manifest
         * this is the whole "return to CarPlay" mechanism.
         */
        fun bringToFront(ctx: Context) {
            ctx.startActivity(
                Intent(ctx, ProjectionActivity::class.java).apply {
                    addFlags(
                        Intent.FLAG_ACTIVITY_NEW_TASK or
                            Intent.FLAG_ACTIVITY_REORDER_TO_FRONT or
                            Intent.FLAG_ACTIVITY_SINGLE_TOP,
                    )
                },
            )
        }
    }
}
