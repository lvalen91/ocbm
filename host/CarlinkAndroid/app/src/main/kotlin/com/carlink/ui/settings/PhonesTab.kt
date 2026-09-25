package com.carlink.ui.settings

import android.view.HapticFeedbackConstants
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Usb
import androidx.compose.material.icons.filled.UsbOff
import androidx.compose.material.icons.filled.Wifi
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.carlink.CarlinkManager
import com.carlink.R
import com.carlink.protocol.PhoneType
import com.carlink.ui.adaptive.AdaptiveGrid
import com.carlink.ui.theme.AutomotiveDimens
import kotlinx.coroutines.delay

/**
 * Device-card width envelope for the adaptive grid. The grid puts as many cards per row as fit
 * at [CARD_MIN_WIDTH] and stretches them up to [CARD_MAX_WIDTH]; the count follows the width of
 * whatever panel this runs on rather than assuming a number. The maximum is the fixed 360 dp card
 * of the carlink_native Phones tab; the minimum is what the widest fixed content (the Remove
 * button, a "Last seen" line) needs on one line at default font scale.
 */
private val CARD_MIN_WIDTH = 240.dp
private val CARD_MAX_WIDTH = 360.dp
private val CARD_GAP = 24.dp

/**
 * Phones tab — the adapter's known device list as an adaptive grid of cards ([AdaptiveGrid]:
 * column count follows the available width, every card the same size, rows centred). It wraps
 * its content; the pane around it scrolls.
 *
 * - USB device card (first): active when a USB phone is connected, greyed out otherwise.
 * - Wireless device cards: the box's bonded MACs merged with the app's own history
 *   ([com.carlink.device.KnownDeviceStore]). INFORMATION-ONLY plus Remove: there is no
 *   targeted-connect verb and no phone-disconnect verb in OCBM, and a tap used to send
 *   `MGMT_RESTART_WIRELESS` in their place (wrong with two bonded phones, a radio bounce for a
 *   wired one). Both verbs are box-side TODOs — docs/ops/04_OPEN_ITEMS.md.
 */
@Composable
fun PhonesTabContent(
    carlinkManager: CarlinkManager,
    modifier: Modifier = Modifier,
) {
    val view = LocalView.current

    // Observe device list and connection state
    var pairedDevices by remember { mutableStateOf(carlinkManager.pairedDevices) }
    var activeBtMac by remember { mutableStateOf(carlinkManager.connectedBtMac) }
    var activeWifi by remember { mutableIntStateOf(carlinkManager.currentWifi ?: -1) }
    var phoneType by remember { mutableStateOf(carlinkManager.currentPhoneType) }
    var managerState by remember { mutableStateOf(carlinkManager.state) }

    // Register device listener for DevList updates (supports multiple listeners)
    DisposableEffect(carlinkManager) {
        val listener =
            CarlinkManager.DeviceListener { devices ->
                pairedDevices = devices
            }
        carlinkManager.addDeviceListener(listener)
        // Request fresh device list when tab opens
        carlinkManager.refreshDeviceList()
        onDispose { carlinkManager.removeDeviceListener(listener) }
    }

    // Poll connection state periodically while tab is visible.
    // Rationale: CarlinkManager.callback is single-slot and already consumed by MainScreen,
    // and DeviceListener only fires on DevList changes — neither surfaces state/phoneType/
    // wifi/btMac transitions to secondary observers. 1 Hz polling is a pragmatic workaround
    // until CarlinkManager grows a multi-observer connection-state listener.
    LaunchedEffect(carlinkManager) {
        while (true) {
            managerState = carlinkManager.state
            activeBtMac = carlinkManager.connectedBtMac
            activeWifi = carlinkManager.currentWifi ?: -1
            phoneType = carlinkManager.currentPhoneType
            delay(1000)
        }
    }

    // Hoisted remove dialog state to prevent stale device references across recompositions.
    var deviceToRemove by remember { mutableStateOf<CarlinkManager.DeviceInfo?>(null) }

    AdaptiveGrid(
        modifier = modifier.fillMaxWidth().padding(vertical = 16.dp),
        minCellWidth = CARD_MIN_WIDTH,
        maxCellWidth = CARD_MAX_WIDTH,
        gap = CARD_GAP,
    ) {
        // === USB Device Card (always present) ===
        // wifi=0 means explicit USB; wifi=-1 (null) with active phoneType means
        // the adapter didn't send the wifi field — treat as USB since wireless
        // always sends wifi=1 explicitly. (connectedBtMac is private backing with a
        // public read-only accessor on CarlinkManager; activeWifi mirrors currentWifi.)
        val isUsbConnected = phoneType != null && activeWifi != 1
        UsbDeviceCard(
            isConnected = isUsbConnected,
            phoneType = if (isUsbConnected) phoneType else null,
        )

        // === Wireless Device Cards ===
        if (pairedDevices.isEmpty()) {
            EmptyDeviceCard()
        } else {
            pairedDevices.forEach { device ->
                // Stable keying by btMac preserves per-card state across list reorderings.
                key(device.btMac) {
                    val isDeviceActive =
                        activeWifi == 1 &&
                            activeBtMac != null &&
                            device.btMac == activeBtMac &&
                            (
                                managerState == CarlinkManager.State.STREAMING ||
                                    managerState == CarlinkManager.State.DEVICE_CONNECTED
                            )

                    WirelessDeviceCard(
                        device = device,
                        isConnected = isDeviceActive,
                        onRemove = {
                            deviceToRemove = device
                        },
                    )
                }
            }
        }
    }

    // Hoisted remove confirmation dialog.
    deviceToRemove?.let { device ->
        RemoveDeviceDialog(
            deviceName = device.name,
            bonded = device.bonded,
            onConfirm = {
                view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
                carlinkManager.forgetDevice(device.btMac)
                deviceToRemove = null
            },
            onDismiss = { deviceToRemove = null },
        )
    }
}

// ==================== USB Device Card ====================

// Active card background tints
private val CarPlayActiveColor = Color(0xFF1B3A1F) // Dark green tint
private val AndroidAutoActiveColor = Color(0xFF1A2A3D) // Dark blue tint

/**
 * Card representing the wired USB slot.
 *
 * Always rendered as the first card; shows a branded CarPlay / Android Auto icon when a
 * USB phone is connected, greyed-out UsbOff icon otherwise. Non-interactive — the adapter
 * owns USB session lifecycle.
 */
@Composable
private fun UsbDeviceCard(
    isConnected: Boolean,
    phoneType: PhoneType?,
    modifier: Modifier = Modifier,
) {
    val colorScheme = MaterialTheme.colorScheme
    val alpha = if (isConnected) 1f else 0.38f

    val containerColor =
        if (isConnected && phoneType != null) {
            activeCardColor(phoneType)
        } else {
            colorScheme.surfaceContainerLow
        }
    val textColor = if (isConnected) Color.White else colorScheme.onSurface.copy(alpha = alpha)

    Card(
        modifier = modifier,
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
        colors = CardDefaults.cardColors(containerColor = containerColor),
    ) {
        Column(
            modifier = Modifier.padding(24.dp).fillMaxWidth().fillMaxHeight(),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Icon(
                imageVector = if (isConnected) Icons.Default.Usb else Icons.Default.UsbOff,
                contentDescription = null,
                tint = textColor,
                modifier = Modifier.size(32.dp),
            )
            Spacer(modifier = Modifier.height(12.dp))
            Text(
                text = "USB",
                style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
                color = textColor,
            )
            Spacer(modifier = Modifier.height(12.dp))
            if (isConnected && phoneType != null) {
                Text(
                    text = "Connected",
                    style = MaterialTheme.typography.bodySmall,
                    color = Color.White.copy(alpha = 0.85f),
                )
                // Push icon to bottom
                Spacer(modifier = Modifier.weight(1f))
                Image(
                    painter = painterResource(id = phoneTypeIcon(phoneType)),
                    contentDescription = phoneType.name,
                    modifier = Modifier.size(48.dp),
                )
            } else {
                Text(
                    text = "No device",
                    style = MaterialTheme.typography.bodyLarge,
                    color = colorScheme.onSurface.copy(alpha = 0.38f),
                )
            }
        }
    }
}

// ==================== Wireless Device Card ====================

/**
 * Card representing a single known wireless device. Information-only (no tap action — see the
 * tab KDoc) plus Remove. A device the app remembers but the box no longer holds a link key for
 * ([CarlinkManager.DeviceInfo.bonded] false) says so on its status line, and its Remove is a
 * local history delete. Under OCBM `type` is always "CarPlay" (the box's link-key store carries
 * no type), so the branded icon is effectively fixed; the string match is kept for the history
 * records that may carry another value.
 */
@Composable
private fun WirelessDeviceCard(
    device: CarlinkManager.DeviceInfo,
    isConnected: Boolean,
    onRemove: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colorScheme = MaterialTheme.colorScheme
    val containerColor = if (isConnected) activeCardColor(device.type) else colorScheme.surfaceContainer

    Card(
        modifier = modifier,
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
        colors = CardDefaults.cardColors(containerColor = containerColor),
    ) {
        Column(
            modifier = Modifier.padding(24.dp).fillMaxWidth().fillMaxHeight(),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Image(
                painter = painterResource(id = deviceTypeIcon(device.type)),
                contentDescription = device.type,
                modifier = Modifier.size(48.dp),
            )

            Spacer(modifier = Modifier.height(12.dp))

            // Device name — white on active colored cards, theme-adaptive otherwise
            val cardTextColor = if (isConnected) Color.White else colorScheme.onSurface
            Text(
                text = device.name,
                style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
                color = cardTextColor,
                textAlign = TextAlign.Center,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )

            Spacer(modifier = Modifier.height(16.dp))

            // Status line — "Connected", "Not paired with adapter", "Last seen: ..." or "Disconnected"
            Text(
                text = deviceStatusText(device, isConnected),
                style = MaterialTheme.typography.bodySmall,
                color = if (isConnected) Color.White.copy(alpha = 0.85f) else colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )

            Spacer(modifier = Modifier.height(16.dp))

            // Push remove button to bottom
            Spacer(modifier = Modifier.weight(1f))

            // Remove button — matches "Disconnect Adapter" style (filled error)
            Button(
                onClick = onRemove,
                modifier = Modifier.heightIn(min = AutomotiveDimens.ButtonMinHeight),
                colors =
                    ButtonDefaults.buttonColors(
                        containerColor = colorScheme.error,
                        contentColor = colorScheme.onError,
                    ),
            ) {
                Icon(
                    imageVector = Icons.Default.Delete,
                    contentDescription = null,
                    modifier = Modifier.size(20.dp),
                )
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = "Remove",
                    style = MaterialTheme.typography.titleMedium,
                )
            }
        }
    }
}

/** Active (connected) card tint for a known-device `type` string; unknown types fall back to neutral. */
@Composable
private fun activeCardColor(deviceType: String): Color =
    when (deviceType) {
        "CarPlay" -> CarPlayActiveColor
        "AndroidAuto" -> AndroidAutoActiveColor
        else -> MaterialTheme.colorScheme.surfaceContainerHighest
    }

/** Branded drawable for a known-device `type` string; unknown types render the generic projection icon. */
private fun deviceTypeIcon(deviceType: String): Int =
    when (deviceType) {
        "CarPlay" -> R.drawable.ic_carplay
        "AndroidAuto" -> R.drawable.ic_android_auto
        else -> R.drawable.ic_phone_projection
    }

/** "Connected", "Not paired with adapter", "Last seen: ..." or "Disconnected". */
private fun deviceStatusText(
    device: CarlinkManager.DeviceInfo,
    isConnected: Boolean,
): String =
    when {
        isConnected -> "Connected"
        !device.bonded -> "Not paired with adapter"
        else -> device.lastConnected?.let { "Last seen: $it" } ?: "Disconnected"
    }

// ==================== Empty State ====================

/**
 * Placeholder card shown when there are no known wireless devices.
 * Prompts the user to pair a phone with the adapter; no interactive actions.
 */
@Composable
private fun EmptyDeviceCard(modifier: Modifier = Modifier) {
    val colorScheme = MaterialTheme.colorScheme

    Card(
        modifier = modifier,
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
        colors = CardDefaults.cardColors(containerColor = colorScheme.surfaceContainerLow),
    ) {
        Column(
            modifier = Modifier.padding(24.dp).fillMaxWidth(),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Icon(
                imageVector = Icons.Default.Wifi,
                contentDescription = null,
                tint = colorScheme.onSurface.copy(alpha = 0.38f),
                modifier = Modifier.size(48.dp),
            )
            Spacer(modifier = Modifier.height(16.dp))
            Text(
                text = "No paired wireless devices",
                style = MaterialTheme.typography.titleMedium,
                color = colorScheme.onSurface.copy(alpha = 0.6f),
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "Connect a phone to the adapter to get started",
                style = MaterialTheme.typography.bodyMedium,
                color = colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
            )
        }
    }
}

/** Returns the active card background color based on phone type. */
@Composable
private fun activeCardColor(phoneType: PhoneType): Color =
    when (phoneType) {
        PhoneType.CARPLAY, PhoneType.CARPLAY_WIRELESS -> CarPlayActiveColor
        PhoneType.ANDROID_AUTO -> AndroidAutoActiveColor
        else -> MaterialTheme.colorScheme.surfaceContainerHighest
    }

/** Branded drawable for a connected USB phone. */
private fun phoneTypeIcon(phoneType: PhoneType): Int =
    when (phoneType) {
        PhoneType.CARPLAY, PhoneType.CARPLAY_WIRELESS -> R.drawable.ic_carplay
        else -> R.drawable.ic_android_auto
    }

// ==================== Dialogs ====================

/**
 * Confirmation dialog for forgetting a known wireless device. For a [bonded] record Remove clears
 * the phone from the app's history AND the box (link key + AirPlay peer store), so the next
 * connection from that phone is a fresh pairing. For an unbonded one the box holds nothing, so
 * only the app's history entry goes — nothing is sent (`CarlinkManager.forgetDevice`).
 */
@Composable
private fun RemoveDeviceDialog(
    deviceName: String,
    bonded: Boolean,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        // Back press / scrim tap both clear deviceToRemove via onDismiss.
        onDismissRequest = onDismiss,
        title = { Text("Remove Device") },
        text = {
            Text(
                if (bonded) {
                    "Remove \"$deviceName\" from the known device list? " +
                        "The adapter will forget it and the phone will need to be paired again."
                } else {
                    "Remove \"$deviceName\" from this app's history? " +
                        "The adapter is not paired with it, so nothing is sent to the adapter."
                },
            )
        },
        confirmButton = {
            TextButton(
                onClick = onConfirm,
                colors =
                    ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
            ) {
                Text("Remove")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}
