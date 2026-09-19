package com.carlink.ui

import android.content.Intent
import android.graphics.Bitmap
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.view.MotionEvent
import android.view.PixelCopy
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.RestartAlt
import androidx.compose.material.icons.filled.SettingsVoice
import androidx.compose.material.icons.filled.Usb
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import com.carlink.BuildConfig
import com.carlink.CarlinkManager
import com.carlink.logging.logDebug
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn
import com.carlink.protocol.MessageSerializer
import com.carlink.protocol.MultiTouchAction
import com.carlink.protocol.PhoneType
import com.carlink.ui.adaptive.DashboardArrangement
import com.carlink.ui.adaptive.DashboardLayout
import com.carlink.ui.adaptive.asWindowInsets
import com.carlink.ui.adaptive.rememberWindowLayoutInfo
import com.carlink.ui.components.LoadingSpinner
import com.carlink.ui.components.VideoSurface
import com.carlink.ui.components.VideoSurfaceState
import com.carlink.ui.components.rememberVideoSurfaceState
import com.carlink.ui.settings.DisplayMode
import com.carlink.ui.settings.PhonesTabContent
import com.carlink.ui.theme.AutomotiveDimens
import com.carlink.ui.theme.GlassButton
import com.carlink.ui.theme.GlassShapes
import com.carlink.ui.theme.frostedGlass
import com.carlink.util.EdgeInsets
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

/**
 * The single app screen (cp-stripped). When STREAMING it is the CarPlay projection (SurfaceView
 * / HWC overlay) with touch forwarding; otherwise it shows the [CarlinkDashboard] (adapter status,
 * adapter controls, known devices) — no separate Settings screen / overlay.
 *
 * The video surface is laid out to the DECLARED PANEL the session was built for
 * ([CarlinkManager.displayProfile]`.surfaceInsets` off the edge-to-edge window: the visible bars
 * plus the parity pixel): the same rectangle that went out in CT_SUBSCRIBE and that the decoder is
 * sized to, so touch — normalised over the surface — lands where iOS drew. A bar the display mode keeps visible is therefore outside the
 * video, never under it.
 */
@Composable
fun MainScreen(
    carlinkManager: CarlinkManager,
    onResetConnection: (() -> Unit)? = null,
    displayMode: DisplayMode = DisplayMode.DEFAULT,
    onDisplayModeSelected: (DisplayMode) -> Unit = {},
) {
    // Key state on carlinkManager identity — when the manager is replaced (Reset Connection
    // rebuild), all session-scoped state resets automatically, preventing stale callbacks /
    // touch state from the old manager leaking into the new session.
    var connectionState by remember(carlinkManager) { mutableStateOf(CarlinkManager.State.DISCONNECTED) }
    var statusText by remember(carlinkManager) { mutableStateOf("Connect Adapter") }
    // True when the OEM "Exit" icon was pressed during a live session → overlay the dashboard.
    var showHostUi by remember(carlinkManager) { mutableStateOf(false) }
    val surfaceState = rememberVideoSurfaceState()

    LaunchedEffect(connectionState) {
        logInfo("[UI_STATE] MainScreen connection state: $connectionState", tag = "UI")
        // The host-UI overlay only makes sense over a live session; reset it otherwise so the
        // idle dashboard shows normally.
        if (connectionState != CarlinkManager.State.STREAMING) showHostUi = false
    }

    // Forward the AAOS day/night state to CarPlay. Fires when a session reaches STREAMING (initial
    // sync) and whenever the system theme toggles mid-session (isSystemInDarkTheme recomposes
    // because MainActivity handles the uiMode config change without recreating).
    val darkTheme = isSystemInDarkTheme()
    LaunchedEffect(darkTheme, connectionState) {
        if (connectionState == CarlinkManager.State.STREAMING) {
            carlinkManager.setNightMode(darkTheme)
        }
    }

    var lastTouchTime by remember(carlinkManager) { mutableLongStateOf(0L) }

    // Plain map, not mutableStateMapOf: never read in composition (only the touch handler
    // mutates/reads it on the UI thread), so snapshot machinery was pure overhead.
    val activeTouches = remember(carlinkManager) { mutableMapOf<Int, TouchPoint>() }

    // Streaming gate drops UP/CANCEL while paused — clear stale pointers when the session
    // leaves STREAMING so they can't resurface as phantom MOVE pointers on resume.
    LaunchedEffect(connectionState) {
        if (connectionState != CarlinkManager.State.STREAMING) activeTouches.clear()
    }

    // Set once initialize() has run with a real surface + size; gates the one-shot start
    // effect below so start() never races an uninitialized manager.
    var initializedForStart by remember(carlinkManager) { mutableStateOf(false) }

    // Container (content-area) dimensions, used for the adapter OPEN/BoxSettings resolution.
    var containerSize by remember(carlinkManager) { mutableStateOf(IntSize.Zero) }

    // Surface init for the adapter — uses container (content-area) dimensions. Idempotent across
    // Surface recreations; start() runs once per manager (the decoupled effect below).
    LaunchedEffect(surfaceState.surface, containerSize) {
        surfaceState.surface?.let { surface ->
            if (containerSize.width <= 0 || containerSize.height <= 0) return@let

            val adapterWidth = containerSize.width and 1.inv()
            val adapterHeight = containerSize.height and 1.inv()

            logInfo(
                "[CARLINK_RESOLUTION] Container size: ${adapterWidth}x$adapterHeight " +
                    "(surface: ${surfaceState.width}x${surfaceState.height})",
                tag = "UI",
            )

            carlinkManager.initialize(
                surface = surface,
                surfaceWidth = adapterWidth,
                surfaceHeight = adapterHeight,
                callback =
                    object : CarlinkManager.Callback {
                        override fun onStateChanged(state: CarlinkManager.State) {
                            connectionState = state
                        }

                        override fun onStatusTextChanged(text: String) {
                            statusText = text
                        }

                        override fun onHostUIPressed() {
                            // OEM "Exit" icon pressed in CarPlay → overlay the dashboard cards on
                            // the live video. The CarPlay session keeps running; dismissing returns
                            // to projection (and flushes the codec so video resumes cleanly).
                            logInfo("[UI_NAV] Host UI requested — overlaying dashboard", tag = "UI")
                            showHostUi = true
                        }

                        override fun onPhoneTypeChanged(phoneType: PhoneType) {
                            logInfo("[UI_SURFACE] Phone type changed: $phoneType", tag = "UI")
                        }
                    },
            )
            initializedForStart = true
        }
    }

    // One-shot start per manager, DECOUPLED from the surface/size effect above. Previously
    // start() ran inside that effect: a surface swap or late layout pass mid-connect
    // cancelled it, and the hasStartedConnection gate (already true) meant the restarted
    // effect never re-issued it — the app sat at "Searching for adapter..." with no retry.
    // This effect is keyed only on the manager, so surface/size churn can't cancel it.
    LaunchedEffect(carlinkManager) {
        snapshotFlow { initializedForStart }.first { it }
        carlinkManager.start()
    }

    val isLoading = connectionState != CarlinkManager.State.STREAMING

    // True while the dashboard overlays a LIVE session (host-UI/"Exit" pressed mid-stream).
    val overlayingSession = showHostUi && !isLoading

    // Frozen frosted-glass backdrop: when the overlay opens, PixelCopy one frame of the video
    // surface; we draw it (blurred via Modifier.blur) behind the cards so the CarPlay feed
    // appears frozen + frosted. Null when not overlaying or if the copy fails (then the live
    // translucent bleedthrough shows instead). The SurfaceView can't be GPU-blurred live (it's
    // a hardware overlay), so a snapshot is the only way to get a real blur — see commit notes.
    var frostedBackdrop by remember(carlinkManager) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(overlayingSession) {
        frostedBackdrop =
            if (overlayingSession) captureSurfaceBitmap(surfaceState) else null
    }

    // The surface rect comes from the SAME profile the config was built from — the bars the
    // session's display mode keeps visible plus the parity pixel (DisplayProfile.surfaceInsets) —
    // not from the live Compose insets, which flip with transient bar reveals and would move the
    // surface out from under the pushed geometry.
    val profile = carlinkManager.displayProfile
    val surfaceInsets = profile?.surfaceInsets ?: EdgeInsets.NONE
    val safeAreaInsets = profile?.safeAreaInsets ?: EdgeInsets.NONE

    // Edge-to-edge window: the video occupies the declared panel (behind hidden bars and into the
    // cutout; a visible bar is excluded by surfaceInsets). The cutout/waterfall/corner safe area is
    // emitted separately by CarlinkManager/MessageSerializer, not from this file.
    Box(modifier = Modifier.fillMaxSize().background(Color.Black)) {
        val density = LocalDensity.current

        BoxWithConstraints(
            modifier = Modifier.fillMaxSize().windowInsetsPadding(surfaceInsets.asWindowInsets()).clipToBounds(),
        ) {
            // Track the content area for the adapter OPEN resolution.
            val containerPx =
                with(density) {
                    IntSize(maxWidth.roundToPx(), maxHeight.roundToPx())
                }
            LaunchedEffect(containerPx) {
                if (containerPx.width > 0 && containerPx.height > 0) {
                    containerSize = containerPx
                }
            }

            // Key the VideoSurface on manager identity so a Reset Connection rebuild drops the
            // AndroidView slot, disposing the old SurfaceView and releasing its HWC overlay plane;
            // a fresh SurfaceView is then inflated against the current window rect.
            key(carlinkManager) {
                VideoSurface(
                    modifier = Modifier.fillMaxSize(),
                    onSurfaceAvailable = { surface, width, height ->
                        logInfo("[UI_SURFACE] Surface available: ${width}x$height", tag = "UI")
                        surfaceState.onSurfaceAvailable(surface, width, height)
                    },
                    onSurfaceDestroyed = {
                        logInfo("[UI_SURFACE] Surface destroyed", tag = "UI")
                        surfaceState.onSurfaceDestroyed()
                        carlinkManager.onSurfaceDestroyed()
                    },
                    onSurfaceSizeChanged = { width, height ->
                        logInfo("[UI_SURFACE] Surface size changed: ${width}x$height", tag = "UI")
                        surfaceState.onSurfaceSizeChanged(width, height)
                    },
                    onTouchEvent = { event ->
                        if (connectionState == CarlinkManager.State.STREAMING) {
                            if (BuildConfig.DEBUG) {
                                val now = System.currentTimeMillis()
                                if (now - lastTouchTime > 1000) {
                                    logDebug(
                                        "[UI_TOUCH] touch: action=${event.actionMasked}" +
                                            ", pointers=${event.pointerCount}" +
                                            ", surface=${surfaceState.width}x${surfaceState.height}" +
                                            ", container=${containerSize.width}x${containerSize.height}",
                                        tag = "UI",
                                    )
                                    lastTouchTime = now
                                }
                            }
                            handleTouchEvent(
                                event,
                                activeTouches,
                                carlinkManager,
                                surfaceState.width,
                                surfaceState.height,
                                containerSize.width,
                                containerSize.height,
                            )
                        }
                        true
                    },
                )
            }
        }

        // Frozen frosted-glass backdrop: the captured frame, blurred, drawn over the (now hidden)
        // live SurfaceView and under the dashboard cards. Only present while overlaying a session
        // and the PixelCopy succeeded; otherwise the live translucent bleedthrough shows.
        if (overlayingSession) {
            frostedBackdrop?.let { backdrop ->
                Image(
                    bitmap = backdrop,
                    contentDescription = null,
                    modifier = Modifier.fillMaxSize().blur(SNAPSHOT_BLUR_RADIUS),
                    contentScale = ContentScale.Crop,
                )
            }
        }

        // Dashboard shows when idle, OR overlaid on a live session when the OEM "Exit" icon was
        // pressed (showHostUi). When overlaying a session it gets a Return-to-CarPlay dismiss.
        if (isLoading || showHostUi) {
            CarlinkDashboard(
                carlinkManager = carlinkManager,
                statusText = statusText,
                frame = DashboardFrame(safeAreaInsets, displayMode, onDisplayModeSelected),
                onResetConnection = onResetConnection,
                onReturnToProjection =
                    if (overlayingSession) {
                        {
                            showHostUi = false
                            carlinkManager.recoverVideoFromOverlay()
                        }
                    } else {
                        null
                    },
            )
        }
        // System back dismisses the host-UI overlay and returns to projection.
        BackHandler(enabled = overlayingSession) {
            showHostUi = false
            carlinkManager.recoverVideoFromOverlay()
        }
    }
}

// ==================== Dashboard ====================

/** Frosted-glass overlay (live-session): light scrim over the blurred video so the glass lifts. */
private const val OVERLAY_SCRIM_ALPHA = 0.12f

/** Blur radius for the frozen frosted-glass backdrop snapshot. */
private val SNAPSHOT_BLUR_RADIUS = 32.dp

/** Gap between dashboard cards and around the dashboard edge. */
private val DASHBOARD_GAP = 16.dp

/**
 * The window frame the dashboard sits in: the detected safe-area padding (cutout / waterfall /
 * corner arcs, from the session's [com.carlink.util.DisplayProfile]), the active [DisplayMode]
 * and where a new choice goes (MainActivity persists it and rebuilds the session).
 */
@Immutable
private class DashboardFrame(
    val safeAreaInsets: EdgeInsets,
    val displayMode: DisplayMode,
    val onDisplayModeSelected: (DisplayMode) -> Unit,
)

// Snapshot downscale: PixelCopy scales into the destination bitmap, and the result is
// blurred 32dp anyway — a quarter-resolution capture (600x240 vs 2400x960, ~0.55MB vs
// ~8.8MB ARGB_8888) is visually identical after the blur at a fraction of the
// allocation and blur cost.
private const val SNAPSHOT_DOWNSCALE = 4

/**
 * Capture one frame of the video [VideoSurfaceState.surface] via PixelCopy, returning it as an
 * ImageBitmap (or null if the surface isn't ready / the copy fails). Used to freeze a frame for
 * the frosted-glass overlay backdrop. PixelCopy is async; this suspends until it completes.
 * Failed captures recycle their bitmap immediately instead of leaving a multi-MB object for GC.
 */
private suspend fun captureSurfaceBitmap(state: VideoSurfaceState): ImageBitmap? {
    val surface = state.surface ?: return null
    val w = state.width / SNAPSHOT_DOWNSCALE
    val h = state.height / SNAPSHOT_DOWNSCALE
    if (w <= 0 || h <= 0 || !surface.isValid) return null
    val bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
    val ok =
        suspendCancellableCoroutine { cont ->
            try {
                PixelCopy.request(
                    surface,
                    bitmap,
                    { result -> if (cont.isActive) cont.resume(result == PixelCopy.SUCCESS) },
                    Handler(Looper.getMainLooper()),
                )
            } catch (e: IllegalArgumentException) {
                // Surface not backed by a usable buffer (rare race on overlay open) — degrade
                // to the live translucent bleedthrough rather than crashing.
                if (cont.isActive) cont.resume(false)
            }
        }
    if (!ok) {
        bitmap.recycle()
        return null
    }
    return bitmap.asImageBitmap()
}

/**
 * Single-view dashboard shown when not projecting. The arrangement follows the window size class
 * ([DashboardLayout], `ui/adaptive/WindowLayout.kt`): an expanded-width landscape window puts the
 * adapter card in a fixed-width column beside the known-devices card; anything narrower, and any
 * portrait window, stacks them. Both arrangements SCROLL when the window is shorter than their
 * content — nothing is clipped or pinned off-screen at any height. Padding keeps the cards inside
 * the live system-UI insets and the detected safe area (cutout / waterfall / corner arcs). Black
 * background; day/night follows [MaterialTheme] (CarlinkTheme).
 */
@Composable
private fun CarlinkDashboard(
    carlinkManager: CarlinkManager,
    statusText: String,
    frame: DashboardFrame,
    onResetConnection: (() -> Unit)?,
    onReturnToProjection: (() -> Unit)? = null,
) {
    val colorScheme = MaterialTheme.colorScheme
    val context = LocalContext.current
    val appVersion =
        remember {
            try {
                val pi = context.packageManager.getPackageInfo(context.packageName, 0)
                "v${pi.versionName} (${pi.longVersionCode})"
            } catch (e: Exception) {
                ""
            }
        }
    val window = rememberWindowLayoutInfo()
    val arrangement = DashboardLayout.arrangement(window)

    // Overlaying a live CarPlay session (host-UI/"Exit") → frosted glass: a translucent scrim
    // over the blurred video instead of opaque black, and semi-transparent card panels.
    val overlaying = onReturnToProjection != null

    Column(
        modifier =
            Modifier
                .fillMaxSize()
                .background(if (overlaying) Color.Black.copy(alpha = OVERLAY_SCRIM_ALPHA) else Color.Black)
                .windowInsetsPadding(WindowInsets.safeDrawing.union(frame.safeAreaInsets.asWindowInsets()))
                .padding(DASHBOARD_GAP),
    ) {
        // Cards: centred when they fit, scrolling when they do not. The footer below keeps its own
        // row, so the version pill can never sit on top of a card.
        Box(modifier = Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
            when (arrangement) {
                DashboardArrangement.TWO_PANE -> {
                    val contentDp = DashboardLayout.contentWidthDp(window.widthDp.toFloat())
                    val adapterDp = DashboardLayout.adapterWidthDp(contentDp)
                    Column(
                        modifier =
                            Modifier
                                .fillMaxWidth(contentDp / window.widthDp)
                                .verticalScroll(rememberScrollState()),
                    ) {
                        // At least CONTENT_HEIGHT_SHARE of the window tall (the adapter card then
                        // centres its status block in the slack), taller when the content is.
                        Row(
                            modifier =
                                Modifier
                                    .fillMaxWidth()
                                    .heightIn(min = (window.heightDp * DashboardLayout.CONTENT_HEIGHT_SHARE).dp)
                                    .height(IntrinsicSize.Min),
                            horizontalArrangement = Arrangement.spacedBy(DASHBOARD_GAP),
                        ) {
                            AdapterCard(
                                carlinkManager,
                                statusText,
                                onResetConnection,
                                Modifier.width(adapterDp.dp).fillMaxHeight(),
                                stretchStatus = true,
                                onReturnToProjection = onReturnToProjection,
                            )
                            KnownDevicesCard(carlinkManager, Modifier.weight(1f).fillMaxHeight(), fillHeight = true)
                        }
                    }
                }

                DashboardArrangement.STACKED -> {
                    Column(
                        modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()),
                        verticalArrangement = Arrangement.spacedBy(DASHBOARD_GAP),
                    ) {
                        AdapterCard(
                            carlinkManager,
                            statusText,
                            onResetConnection,
                            Modifier.fillMaxWidth(),
                            onReturnToProjection = onReturnToProjection,
                        )
                        KnownDevicesCard(carlinkManager, Modifier.fillMaxWidth(), fillHeight = false)
                    }
                }
            }
        }

        // Footer: display-mode chooser (start) and the version pill (end).
        Row(
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            DisplayModeButton(frame.displayMode, frame.onDisplayModeSelected)
            Surface(
                shape = MaterialTheme.shapes.small,
                color = colorScheme.surfaceVariant,
            ) {
                Text(
                    text = appVersion,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * "Display: Fullscreen" — opens the [DisplayMode] picker. Choosing a mode rebuilds the session
 * (MainActivity), because the bars a mode keeps change the content area the box is told about.
 */
@Composable
private fun DisplayModeButton(
    displayMode: DisplayMode,
    onDisplayModeSelected: (DisplayMode) -> Unit,
) {
    var showDialog by remember { mutableStateOf(false) }
    GlassButton(
        onClick = { showDialog = true },
        contentColor = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.heightIn(min = AutomotiveDimens.FooterMinHeight),
    ) {
        Icon(imageVector = Icons.Default.Fullscreen, contentDescription = null, modifier = Modifier.size(20.dp))
        Spacer(modifier = Modifier.width(8.dp))
        Text(text = "Display: ${displayMode.label}", style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
    if (showDialog) {
        AlertDialog(
            onDismissRequest = { showDialog = false },
            title = { Text("Display mode") },
            text = {
                Column(modifier = Modifier.verticalScroll(rememberScrollState())) {
                    Text(
                        text = "Which system bars stay on screen. CarPlay is re-sized to the area that is left, so the session restarts.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Spacer(modifier = Modifier.height(8.dp))
                    DisplayMode.entries.forEach { mode ->
                        Row(
                            modifier =
                                Modifier
                                    .fillMaxWidth()
                                    .heightIn(min = AutomotiveDimens.ListRowMinHeight)
                                    .clickable {
                                        showDialog = false
                                        onDisplayModeSelected(mode)
                                    }.padding(vertical = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = mode == displayMode, onClick = null)
                            Spacer(modifier = Modifier.width(8.dp))
                            Column {
                                Text(text = mode.label, style = MaterialTheme.typography.titleMedium)
                                Text(
                                    text = mode.summary,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { showDialog = false }) { Text("Close") }
            },
        )
    }
}

/**
 * Combined adapter card: projection status (logo + "Connect Phone to: [name]" + live status
 * text) followed by the adapter control buttons (Reboot Adapter, Reset Connection). No section
 * title; no loading spinner. Every button is a MINIMUM height (touch target), never a fixed one,
 * so the card grows with its content and the dashboard scrolls it rather than overlapping.
 */
@Composable
private fun AdapterCard(
    carlinkManager: CarlinkManager,
    statusText: String,
    onResetConnection: (() -> Unit)?,
    modifier: Modifier = Modifier,
    // When true (two-pane, card stretched to the row height) the status block takes the slack
    // above the controls and centers within it. When false (stacked, wrap-content) it packs
    // at the top.
    stretchStatus: Boolean = false,
    // Non-null only while the dashboard overlays a live CarPlay session (host-UI/"Exit" action).
    // When non-null the top "Return to CarPlay" button is enabled and returns to projection;
    // otherwise the button is shown greyed-out/disabled as a permanent placeholder.
    onReturnToProjection: (() -> Unit)? = null,
) {
    val colorScheme = MaterialTheme.colorScheme
    val scope = rememberCoroutineScope()
    var isProcessing by remember { mutableStateOf(false) }
    var showRebootDialog by remember { mutableStateOf(false) }

    Box(modifier = modifier.frostedGlass(GlassShapes.Card, strong = true)) {
        Column(
            modifier = Modifier.padding(20.dp).fillMaxSize(),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // --- Return to CarPlay (glass pill; greyed out until reachable during a live
            // session via the host-UI overlay) ---
            GlassButton(
                onClick = { onReturnToProjection?.invoke() },
                enabled = onReturnToProjection != null,
                contentColor = colorScheme.primary,
                modifier = Modifier.fillMaxWidth().heightIn(min = AutomotiveDimens.ButtonMinHeight),
            ) {
                Text(
                    text = "Return to CarPlay",
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(modifier = Modifier.height(16.dp))

            // --- Status (centered in the space between the Return button and the first control) ---
            Column(
                modifier =
                    if (stretchStatus) {
                        Modifier.fillMaxWidth().weight(1f)
                    } else {
                        Modifier.fillMaxWidth()
                    },
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Text(
                    text = "Connect Phone to:",
                    style = MaterialTheme.typography.titleMedium,
                    color = colorScheme.onSurface,
                )
                Text(
                    text = carlinkManager.adapterName,
                    style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
                    color = colorScheme.primary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    text = statusText,
                    style = MaterialTheme.typography.titleMedium,
                    color = colorScheme.onSurface,
                    textAlign = TextAlign.Center,
                )
                BoxStatusLine(carlinkManager)
            }

            Spacer(modifier = Modifier.height(16.dp))
            SiriRow(carlinkManager, connected = statusText.isNotEmpty() && carlinkManager.state != CarlinkManager.State.DISCONNECTED)

            // --- Controls: Reboot = glass (warning tint), Reset = solid vibrant accent (destructive) ---
            Spacer(modifier = Modifier.height(12.dp))
            GlassButton(
                onClick = { showRebootDialog = true },
                enabled = !isProcessing,
                contentColor = colorScheme.tertiary,
                modifier = Modifier.fillMaxWidth().heightIn(min = AutomotiveDimens.ButtonMinHeight),
            ) {
                Icon(imageVector = Icons.Default.RestartAlt, contentDescription = null, modifier = Modifier.size(24.dp))
                Spacer(modifier = Modifier.width(8.dp))
                Text(text = "Reboot Adapter", style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }

            Spacer(modifier = Modifier.height(12.dp))

            ControlButton(
                label = "Reset Connection",
                icon = Icons.Default.Usb,
                severity = ButtonSeverity.DESTRUCTIVE,
                enabled = !isProcessing,
                isProcessing = isProcessing,
                onClick = {
                    logWarn("[UI_ACTION] Reset Connection clicked", tag = "UI")
                    isProcessing = true
                    val reset = onResetConnection
                    if (reset != null) {
                        reset()
                        isProcessing = false
                    } else {
                        scope.launch {
                            try {
                                carlinkManager.restart()
                            } finally {
                                isProcessing = false
                            }
                        }
                    }
                },
            )
        }
    }

    if (showRebootDialog) {
        AlertDialog(
            onDismissRequest = { showRebootDialog = false },
            icon = {
                Icon(
                    imageVector = Icons.Default.RestartAlt,
                    contentDescription = null,
                    tint = colorScheme.tertiary,
                )
            },
            title = { Text("Reboot Adapter?") },
            text = { Text("The adapter will restart. It will reconnect automatically in about 15 seconds.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        logWarn("[UI_ACTION] Reboot Adapter confirmed", tag = "UI")
                        showRebootDialog = false
                        scope.launch(Dispatchers.IO) { carlinkManager.rebootAdapter() }
                    },
                ) {
                    Text("Reboot", color = colorScheme.tertiary)
                }
            },
            dismissButton = {
                TextButton(onClick = { showRebootDialog = false }) { Text("Cancel") }
            },
        )
    }
}

/**
 * "WIRED_CP · HCI|SSP|iap2d|airplayd" — which transport owns the box (CT_PROJ_MODE) and which of its
 * subsystems are alive (CT_BOX_HEALTH). Subscribes itself so the card's signature stays put; empty
 * (and laid out as nothing) until the box has said anything.
 */
@Composable
private fun BoxStatusLine(carlinkManager: CarlinkManager) {
    var text by remember(carlinkManager) { mutableStateOf(carlinkManager.boxStatusText()) }
    DisposableEffect(carlinkManager) {
        val l = CarlinkManager.BoxStatusListener { text = it }
        carlinkManager.addBoxStatusListener(l)
        onDispose { carlinkManager.removeBoxStatusListener(l) }
    }
    if (text.isEmpty()) return
    Text(
        text = text,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
    )
}

/**
 * Siri, tier 1: the in-app affordance — a tap sends the `CMD_SIRI_DOWN`/`UP` hold pair. Reachable
 * during a live session through the OEM "Exit" overlay. Beside it, the tier-3 opt-in: the system
 * assistant picker, where choosing Carlink routes the wheel's push-to-talk key to Siri (see `voice/`).
 */
@Composable
private fun SiriRow(
    carlinkManager: CarlinkManager,
    connected: Boolean,
) {
    val colorScheme = MaterialTheme.colorScheme
    val context = LocalContext.current
    Row(modifier = Modifier.fillMaxWidth().height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        GlassButton(
            onClick = {
                val sent = carlinkManager.requestSiri()
                logInfo("[UI_ACTION] Siri tapped -> ${if (sent) "sent" else "no session"}", tag = "UI")
            },
            enabled = connected,
            contentColor = colorScheme.primary,
            modifier = Modifier.weight(1f).heightIn(min = AutomotiveDimens.ButtonMinHeight),
        ) {
            Icon(imageVector = Icons.Default.Mic, contentDescription = null, modifier = Modifier.size(24.dp))
            Spacer(modifier = Modifier.width(8.dp))
            Text(text = "Siri", style = MaterialTheme.typography.titleMedium, maxLines = 1)
        }
        GlassButton(
            onClick = {
                logInfo("[UI_ACTION] Assistant settings opened", tag = "UI")
                runCatching {
                    context.startActivity(Intent(Settings.ACTION_VOICE_INPUT_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                }.onFailure { logWarn("[UI_ACTION] no assistant settings screen: ${it.message}", tag = "UI") }
            },
            contentColor = colorScheme.onSurface,
            modifier = Modifier.fillMaxHeight().heightIn(min = AutomotiveDimens.ButtonMinHeight),
        ) {
            Icon(imageVector = Icons.Default.SettingsVoice, contentDescription = "Assistant settings", modifier = Modifier.size(24.dp))
        }
    }
}

/**
 * Known/paired devices — an instruction line, then the device cards (USB + wireless, from
 * PhonesTab) in an adaptive grid whose column count follows the card's width. [fillHeight]
 * (two-pane) centres the grid in the card's slack; stacked, the card wraps the grid.
 */
@Composable
private fun KnownDevicesCard(
    carlinkManager: CarlinkManager,
    modifier: Modifier = Modifier,
    fillHeight: Boolean,
) {
    Box(modifier = modifier.fillMaxWidth().frostedGlass(GlassShapes.Card, strong = true)) {
        Column(modifier = Modifier.padding(20.dp).then(if (fillHeight) Modifier.fillMaxSize() else Modifier.fillMaxWidth())) {
            Text(
                text = "Tap a known device or remove it",
                style = MaterialTheme.typography.titleLarge,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Box(
                modifier = Modifier.fillMaxWidth().then(if (fillHeight) Modifier.weight(1f) else Modifier),
                contentAlignment = Alignment.Center,
            ) {
                PhonesTabContent(carlinkManager)
            }
        }
    }
}

// ==================== Reusable card + button (moved from the removed SettingsScreen) ====================

private enum class ButtonSeverity { WARNING, DESTRUCTIVE }

/** Action button: swaps its icon for a spinner while [isProcessing]; color by [severity]. */
@Composable
private fun ControlButton(
    label: String,
    icon: ImageVector,
    severity: ButtonSeverity,
    enabled: Boolean,
    isProcessing: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colorScheme = MaterialTheme.colorScheme
    val containerColor: Color
    val contentColor: Color
    when (severity) {
        ButtonSeverity.DESTRUCTIVE -> {
            containerColor = colorScheme.error
            contentColor = colorScheme.onError
        }
        ButtonSeverity.WARNING -> {
            containerColor = colorScheme.tertiaryContainer
            contentColor = colorScheme.onTertiaryContainer
        }
    }

    Button(
        onClick = onClick,
        enabled = enabled && !isProcessing,
        modifier = modifier.fillMaxWidth().heightIn(min = AutomotiveDimens.ButtonMinHeight),
        colors = ButtonDefaults.buttonColors(containerColor = containerColor, contentColor = contentColor),
        contentPadding = PaddingValues(horizontal = 24.dp, vertical = 16.dp),
    ) {
        AnimatedContent(
            targetState = isProcessing,
            transitionSpec = { (fadeIn() + scaleIn()).togetherWith(fadeOut() + scaleOut()) },
            label = "iconTransition",
        ) { processing ->
            if (processing) {
                LoadingSpinner(size = 24.dp, color = contentColor)
            } else {
                Icon(imageVector = icon, contentDescription = label, modifier = Modifier.size(24.dp))
            }
        }
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

// ==================== Touch forwarding ====================

/** In-memory per-pointer touch record (normalized 0..1 coords + last action) for deduping MOVE spam. */
private data class TouchPoint(
    val x: Float,
    val y: Float,
    val action: MultiTouchAction,
)

/**
 * Handle touch on the SurfaceView. CarPlay multitouch (type 0x17): normalize to 0..1 of the
 * SurfaceView. A deadband suppresses sub-pixel MOVE spam.
 */
private fun handleTouchEvent(
    event: MotionEvent,
    activeTouches: MutableMap<Int, TouchPoint>,
    carlinkManager: CarlinkManager,
    surfaceWidth: Int,
    surfaceHeight: Int,
    containerWidth: Int,
    containerHeight: Int,
) {
    if (surfaceWidth == 0 || surfaceHeight == 0 || containerWidth == 0 || containerHeight == 0) return

    // ACTION_CANCEL terminates ALL pointers of the gesture, not just actionIndex (which is
    // always 0 for CANCEL). Marking only one pointer UP left the rest in the map forever —
    // re-sent as phantom held fingers in every later frame until Reset Connection.
    if (event.actionMasked == MotionEvent.ACTION_CANCEL) {
        if (activeTouches.isEmpty()) return
        val touchList =
            activeTouches.entries.map { entry ->
                MessageSerializer.TouchPoint(
                    x = entry.value.x,
                    y = entry.value.y,
                    action = MultiTouchAction.UP,
                    id = entry.key,
                )
            }
        activeTouches.clear()
        carlinkManager.sendMultiTouch(touchList)
        return
    }

    val pointerIndex = event.actionIndex
    val pointerId = event.getPointerId(pointerIndex)

    val action =
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> MultiTouchAction.DOWN
            MotionEvent.ACTION_MOVE -> MultiTouchAction.MOVE
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> MultiTouchAction.UP
            else -> return
        }

    // Clamp: MotionEvent coords legitimately go outside the view during drags that exit
    // the surface; out-of-range values (-0.01, 1.03) must not reach the wire.
    val x = (event.getX(pointerIndex) / surfaceWidth).coerceIn(0f, 1f)
    val y = (event.getY(pointerIndex) / surfaceHeight).coerceIn(0f, 1f)

    var changed = false
    when (action) {
        MultiTouchAction.DOWN -> {
            activeTouches[pointerId] = TouchPoint(x, y, action)
            changed = true
        }

        MultiTouchAction.MOVE -> {
            for (i in 0 until event.pointerCount) {
                val id = event.getPointerId(i)
                val px = (event.getX(i) / surfaceWidth).coerceIn(0f, 1f)
                val py = (event.getY(i) / surfaceHeight).coerceIn(0f, 1f)
                activeTouches[id]?.let { existing ->
                    // Deadband ≈ 0.3% of the normalized surface — suppresses sub-pixel MOVE spam.
                    val dx = kotlin.math.abs(existing.x - px) * 1000
                    val dy = kotlin.math.abs(existing.y - py) * 1000
                    if (dx > 3 || dy > 3) {
                        activeTouches[id] = TouchPoint(px, py, MultiTouchAction.MOVE)
                        changed = true
                    }
                }
            }
        }

        MultiTouchAction.UP -> {
            // Only announce UP for pointers we actually announced DOWN for — a pointer
            // whose DOWN was swallowed (zero-size guard, mid-gesture stream start) would
            // otherwise emit a spurious lone UP for an id the peer never saw.
            if (activeTouches.containsKey(pointerId)) {
                activeTouches[pointerId] = TouchPoint(x, y, action)
                changed = true
            }
        }

        else -> {}
    }

    // Send only when something actually changed. Before, a MOVE where no pointer beat the
    // deadband still re-sent identical coordinates at input rate (the deadband saved
    // nothing on the USB path), and a MOVE with an empty map (DOWN swallowed by the
    // zero-size guard) emitted an empty 0x17 frame.
    if (!changed || activeTouches.isEmpty()) return

    val touchList =
        activeTouches.entries.map { entry ->
            MessageSerializer.TouchPoint(
                x = entry.value.x,
                y = entry.value.y,
                action = entry.value.action,
                id = entry.key,
            )
        }

    carlinkManager.sendMultiTouch(touchList)
    activeTouches.entries.removeIf { it.value.action == MultiTouchAction.UP }
    // DOWN is an edge event: demote to MOVE after its first send so a stationary finger
    // in a multi-touch gesture is not re-announced as a fresh DOWN dozens of times/sec.
    for (entry in activeTouches.entries) {
        if (entry.value.action == MultiTouchAction.DOWN) {
            entry.setValue(entry.value.copy(action = MultiTouchAction.MOVE))
        }
    }
}
