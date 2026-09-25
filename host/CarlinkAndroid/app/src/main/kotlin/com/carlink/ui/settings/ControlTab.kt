package com.carlink.ui.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.DisplaySettings
import androidx.compose.material.icons.filled.PhoneDisabled
import androidx.compose.material.icons.filled.PowerOff
import androidx.compose.material.icons.filled.RestartAlt
import androidx.compose.material.icons.filled.SettingsInputComponent
import androidx.compose.material.icons.filled.Usb
import androidx.compose.material.icons.filled.VideoSettings
import androidx.compose.material.icons.filled.Wifi
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.carlink.CarlinkManager
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn
import com.carlink.ui.DisplayModeOutcome
import com.carlink.ui.adaptive.DashboardArrangement
import com.carlink.ui.adaptive.DashboardLayout
import com.carlink.ui.adaptive.rememberWindowLayoutInfo
import com.carlink.ui.components.BoxStatusLine
import com.carlink.ui.components.ButtonSeverity
import com.carlink.ui.components.ControlAction
import com.carlink.ui.components.ControlButton
import com.carlink.ui.components.ControlCard
import com.carlink.ui.theme.AutomotiveDimens
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

private val DISCONNECT_PHONE = ControlAction("Disconnect Phone", Icons.Default.PhoneDisabled, ButtonSeverity.WARNING)
private val RESTART_WIRELESS = ControlAction("Restart Wireless", Icons.Default.Wifi, ButtonSeverity.WARNING)
private val REBOOT_ADAPTER = ControlAction("Reboot Adapter", Icons.Default.RestartAlt, ButtonSeverity.WARNING)
private val DISCONNECT_ADAPTER = ControlAction("Disconnect Adapter", Icons.Default.PowerOff, ButtonSeverity.DESTRUCTIVE)
private val RESET_DECODER = ControlAction("Reset Decoder", Icons.Default.VideoSettings, ButtonSeverity.WARNING)
private val RESET_CONNECTION = ControlAction("Reset Connection", Icons.Default.Usb, ButtonSeverity.DESTRUCTIVE)

/** Why Disconnect Phone is disabled on every box today — see [CarlinkManager.phoneDisconnectSupported]. */
private const val DISCONNECT_PHONE_UNSUPPORTED = "Requires adapter firmware support"

/**
 * Control tab: the "Adapter" card (box status, Disconnect Phone, Restart Wireless, Reboot Adapter,
 * Disconnect Adapter) beside the "App Control" card (Display Mode, Reset Decoder, Reset
 * Connection). Side by side on an expanded landscape window, stacked otherwise — the same
 * breakpoint the previous dashboard used ([DashboardLayout]), so a portrait or narrow panel
 * never squeezes two cards into one row.
 *
 * What each control sends (Settings → OCBM mapping, corrected 2026-09-25; OCBMANDROID.md):
 *  - Disconnect Phone: NOTHING today — disabled with a reason until the box grows a
 *    phone-disconnect verb and advertises it. It used to send `MGMT_RESTART_WIRELESS` in disguise.
 *  - Restart Wireless: `MGMT_RESTART_WIRELESS`, confirmed; does not touch a wired phone.
 *  - Reboot Adapter: `MGMT_REBOOT`, confirmed; ~50 s absence and a USB permission prompt after.
 *  - Disconnect Adapter: CT_STOP + release USB, confirmed, off the main thread.
 *  - Reset Connection: the clean-slate reset ([CarlinkManager.cleanSlateReset]), confirmed.
 */
@Composable
internal fun ControlTabContent(
    carlinkManager: CarlinkManager,
    connectionState: CarlinkManager.State,
    displayMode: DisplayMode,
    onDisplayModeSelected: (DisplayMode) -> DisplayModeOutcome,
) {
    val window = rememberWindowLayoutInfo()
    val twoPane = DashboardLayout.arrangement(window) == DashboardArrangement.TWO_PANE
    // Sized like carlink_native: 75 % of the window, clamped to a legible range.
    val maxContentWidth = (window.widthDp * 0.75f).dp.coerceIn(400.dp, 1200.dp)
    val connected = connectionState != CarlinkManager.State.DISCONNECTED

    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
        Column(
            modifier =
                Modifier
                    .widthIn(max = maxContentWidth)
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState())
                    .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp),
        ) {
            if (twoPane) {
                Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                    AdapterCard(carlinkManager, connected, Modifier.weight(1f))
                    AppControlCard(carlinkManager, connected, displayMode, onDisplayModeSelected, Modifier.weight(1f))
                }
            } else {
                AdapterCard(carlinkManager, connected, Modifier.fillMaxWidth())
                AppControlCard(carlinkManager, connected, displayMode, onDisplayModeSelected, Modifier.fillMaxWidth())
            }
        }
    }
}

/** Which of the Adapter card's confirmations is open, if any. */
private enum class AdapterConfirm { RESTART_WIRELESS, REBOOT, DISCONNECT_ADAPTER }

/** Box-side status and the adapter-level actions. */
@Composable
private fun AdapterCard(
    carlinkManager: CarlinkManager,
    connected: Boolean,
    modifier: Modifier = Modifier,
) {
    var confirm by remember { mutableStateOf<AdapterConfirm?>(null) }
    val scope = rememberCoroutineScope()
    // Read once per composition; the box cannot gain the capability mid-session (it is a
    // HELLO_ACK caps bit), and `connected` recomposes this card on every session edge anyway.
    val phoneDisconnectSupported = carlinkManager.phoneDisconnectSupported

    ControlCard(title = "Adapter", icon = Icons.Default.SettingsInputComponent, modifier = modifier) {
        Text(
            text = "Connect phone to: ${carlinkManager.adapterName}",
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSurface,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        BoxStatusLine(carlinkManager, Modifier.padding(top = 4.dp))
        Spacer(modifier = Modifier.height(16.dp))

        // Shown DISABLED rather than hidden: the control belongs on this card (carlink_native
        // had it, and the driver expects it here), and a visible reason beats a missing button
        // nobody can explain. Enabling it is the manager's capability flag flipping — no UI edit.
        ControlButton(DISCONNECT_PHONE, enabled = connected && phoneDisconnectSupported, onClick = {
            logWarn("[UI_ACTION] Disconnect Phone button clicked", tag = "UI")
            carlinkManager.disconnectPhone()
        })
        if (!phoneDisconnectSupported) {
            Text(
                text = DISCONNECT_PHONE_UNSUPPORTED,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
        Spacer(modifier = Modifier.height(12.dp))
        ControlButton(RESTART_WIRELESS, enabled = connected, onClick = { confirm = AdapterConfirm.RESTART_WIRELESS })
        Spacer(modifier = Modifier.height(12.dp))
        ControlButton(REBOOT_ADAPTER, enabled = true, onClick = { confirm = AdapterConfirm.REBOOT })
        Spacer(modifier = Modifier.height(12.dp))
        ControlButton(DISCONNECT_ADAPTER, enabled = connected, onClick = { confirm = AdapterConfirm.DISCONNECT_ADAPTER })
    }

    AdapterConfirmDialog(confirm, carlinkManager, scope, onDone = { confirm = null })
}

/** The confirmation for whichever Adapter action is pending, and the action it guards. */
@Composable
private fun AdapterConfirmDialog(
    confirm: AdapterConfirm?,
    carlinkManager: CarlinkManager,
    scope: CoroutineScope,
    onDone: () -> Unit,
) {
    when (confirm) {
        AdapterConfirm.RESTART_WIRELESS ->
            ConfirmDialog(RESTART_WIRELESS_CONFIRM, onDismiss = onDone, onConfirm = {
                logWarn("[UI_ACTION] Restart Wireless confirmed", tag = "UI")
                onDone()
                carlinkManager.restartWireless()
            })
        AdapterConfirm.REBOOT ->
            ConfirmDialog(REBOOT_CONFIRM, onDismiss = onDone, onConfirm = {
                logWarn("[UI_ACTION] Reboot Adapter confirmed", tag = "UI")
                onDone()
                scope.launch(Dispatchers.IO) { carlinkManager.rebootAdapter() }
            })
        AdapterConfirm.DISCONNECT_ADAPTER ->
            ConfirmDialog(DISCONNECT_ADAPTER_CONFIRM, onDismiss = onDone, onConfirm = {
                logWarn("[UI_ACTION] Disconnect Adapter confirmed", tag = "UI")
                onDone()
                scope.launch { carlinkManager.disconnect() }
            })
        null -> Unit
    }
}

/** Text of one confirmation dialog. */
private class ConfirmSpec(
    val icon: ImageVector,
    val title: String,
    val text: String,
    val confirmLabel: String,
)

private val RESTART_WIRELESS_CONFIRM =
    ConfirmSpec(
        icon = Icons.Default.Wifi,
        title = "Restart wireless stack?",
        text =
            "The adapter restarts its Bluetooth and Wi-Fi and advertises again. " +
                "A wirelessly connected phone will drop and reconnect; a phone on the USB cable is not affected.",
        confirmLabel = "Restart Wireless",
    )

private val REBOOT_CONFIRM =
    ConfirmSpec(
        icon = Icons.Default.RestartAlt,
        title = "Reboot Adapter?",
        text =
            "The adapter will reboot and any live projection session will drop. " +
                "It is gone for about 50 seconds while it restarts and re-enumerates on USB; " +
                "Android may then ask for USB permission again before the app reconnects.",
        confirmLabel = "Reboot",
    )

private val DISCONNECT_ADAPTER_CONFIRM =
    ConfirmSpec(
        icon = Icons.Default.PowerOff,
        title = "Disconnect Adapter?",
        text =
            "Ends the session with the adapter and releases it. Any live projection drops; " +
                "the app stays open and reconnects when you use Reset Connection or replug the adapter.",
        confirmLabel = "Disconnect",
    )

private val RESET_CONNECTION_CONFIRM =
    ConfirmSpec(
        icon = Icons.Default.Usb,
        title = "Reset Connection?",
        text =
            "Ends the current phone session and the link to the adapter, waits for the adapter to " +
                "finish shutting the session down, then reconnects and starts a new session. " +
                "The phone reconnects on its own; expect about 10 seconds without projection.",
        confirmLabel = "Reset",
    )

/** One confirmation shape for every destructive adapter action. */
@Composable
private fun ConfirmDialog(
    spec: ConfirmSpec,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val colorScheme = MaterialTheme.colorScheme
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(imageVector = spec.icon, contentDescription = null, tint = colorScheme.tertiary) },
        title = { Text(spec.title) },
        text = { Text(spec.text) },
        confirmButton = { TextButton(onClick = onConfirm) { Text(spec.confirmLabel, color = colorScheme.error) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Display mode, decoder and session resets. */
@Composable
private fun AppControlCard(
    carlinkManager: CarlinkManager,
    connected: Boolean,
    displayMode: DisplayMode,
    onDisplayModeSelected: (DisplayMode) -> DisplayModeOutcome,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    var showDeferredNotice by remember { mutableStateOf(false) }
    var showDisplayModeDialog by remember { mutableStateOf(false) }
    var showResetConfirm by remember { mutableStateOf(false) }
    var isRestarting by remember(carlinkManager) { mutableStateOf(carlinkManager.resetInFlight) }

    ControlCard(title = "App Control", icon = Icons.Default.DisplaySettings, modifier = modifier) {
        TonalActionButton(label = "Display Mode", icon = displayMode.icon, onClick = { showDisplayModeDialog = true })
        Spacer(modifier = Modifier.height(12.dp))
        ControlButton(RESET_DECODER, enabled = !isRestarting, onClick = {
            logWarn("[UI_ACTION] Reset Decoder button clicked", tag = "UI")
            carlinkManager.resetVideoDecoder()
        })
        Spacer(modifier = Modifier.height(12.dp))
        ControlButton(
            RESET_CONNECTION,
            enabled = connected,
            isProcessing = isRestarting,
            onClick = { showResetConfirm = true },
        )
    }

    if (showResetConfirm) {
        ResetConnectionDialog(
            onConfirm = {
                logWarn("[UI_ACTION] Reset Connection confirmed", tag = "UI")
                showResetConfirm = false
                isRestarting = true
                scope.launch {
                    try {
                        carlinkManager.cleanSlateReset()
                    } finally {
                        isRestarting = false
                    }
                }
            },
            onDismiss = { showResetConfirm = false },
        )
    }

    if (showDisplayModeDialog) {
        DisplayModeDialog(
            currentMode = displayMode,
            onDismiss = { showDisplayModeDialog = false },
            onApply = { mode ->
                showDisplayModeDialog = false
                logInfo("[DISPLAY] User applied display mode: ${mode.name}", tag = "UI")
                if (onDisplayModeSelected(mode) == DisplayModeOutcome.DEFERRED) showDeferredNotice = true
            },
        )
    }

    if (showDeferredNotice) DisplayModeDeferredNotice(onDismiss = { showDeferredNotice = false })
}

/** Shown when a display-mode choice was persisted but held back by a live phone session. */
@Composable
private fun DisplayModeDeferredNotice(onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(imageVector = Icons.Default.DisplaySettings, contentDescription = null) },
        title = { Text("Display mode saved") },
        text = {
            Text(
                "A phone session is active, so the new display mode applies at the next connection. " +
                    "The current session keeps its layout.",
            )
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("OK") } },
    )
}

/**
 * Confirmation for the clean-slate reset — shared with the loading overlay's Reset Device
 * (`MainScreen`), which runs the same [CarlinkManager.cleanSlateReset].
 */
@Composable
internal fun ResetConnectionDialog(
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    ConfirmDialog(RESET_CONNECTION_CONFIRM, onConfirm = onConfirm, onDismiss = onDismiss)
}

/** Neutral (non-destructive) tonal action: icon + label, automotive touch height. */
@Composable
private fun TonalActionButton(
    label: String,
    icon: ImageVector,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    FilledTonalButton(
        onClick = onClick,
        modifier = modifier.fillMaxWidth().heightIn(min = AutomotiveDimens.ButtonMinHeight),
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
    ) {
        Icon(imageVector = icon, contentDescription = label, modifier = Modifier.size(AutomotiveDimens.IconSize))
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.Medium),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}
