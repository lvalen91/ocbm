package com.carlink.ui.settings

import android.view.Window
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.FullscreenExit
import androidx.compose.material.icons.filled.Layers
import androidx.compose.material.icons.filled.RestartAlt
import androidx.compose.material.icons.filled.WebAsset
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.carlink.ui.components.OptionButton
import com.carlink.ui.components.OptionCard

/** Short button label and icon for each [DisplayMode], in the order the picker shows them. */
private val DisplayMode.pickerLabel: String
    get() =
        when (this) {
            DisplayMode.SYSTEM_UI_VISIBLE -> "System UI"
            DisplayMode.STATUS_BAR_HIDDEN -> "Hide Status"
            DisplayMode.NAV_BAR_HIDDEN -> "Hide Dock"
            DisplayMode.FULLSCREEN_IMMERSIVE -> "Fullscreen"
        }

/** Icon for a [DisplayMode] — shared by the picker and the Control tab's "Display Mode" button. */
internal val DisplayMode.icon: ImageVector
    get() =
        when (this) {
            DisplayMode.SYSTEM_UI_VISIBLE -> Icons.Default.FullscreenExit
            DisplayMode.STATUS_BAR_HIDDEN -> Icons.Default.Layers
            DisplayMode.NAV_BAR_HIDDEN -> Icons.Default.WebAsset
            DisplayMode.FULLSCREEN_IMMERSIVE -> Icons.Default.Fullscreen
        }

private val PICKER_ORDER =
    listOf(
        DisplayMode.SYSTEM_UI_VISIBLE,
        DisplayMode.STATUS_BAR_HIDDEN,
        DisplayMode.NAV_BAR_HIDDEN,
        DisplayMode.FULLSCREEN_IMMERSIVE,
    )

/**
 * Display Mode picker with live preview: selecting a mode shows/hides the system bars at once so
 * the driver sees what they get; Cancel (or a tap outside) restores the [currentMode]; Apply hands
 * the choice to [onApply] — MainActivity persists it and rebuilds the session, because the bars a
 * mode keeps change the content area the box is told about.
 *
 * The preview is NOT restored after Apply: MainActivity has already applied the new mode to the
 * window by the time this dialog leaves composition, and re-asserting the old one here would put
 * the bars back under a session sized for the new content area.
 */
@Composable
internal fun DisplayModeDialog(
    currentMode: DisplayMode,
    onDismiss: () -> Unit,
    onApply: (DisplayMode) -> Unit,
) {
    val colorScheme = MaterialTheme.colorScheme
    val window = (LocalContext.current as? ComponentActivity)?.window
    var selectedMode by remember { mutableStateOf(currentMode) }
    var applied by remember { mutableStateOf(false) }

    LaunchedEffect(selectedMode) { window?.let { previewDisplayMode(it, selectedMode) } }
    DisposableEffect(Unit) {
        onDispose { if (!applied) window?.let { previewDisplayMode(it, currentMode) } }
    }
    val cancel = {
        window?.let { previewDisplayMode(it, currentMode) }
        onDismiss()
    }

    val windowInfo = LocalWindowInfo.current
    val density = LocalDensity.current
    val containerWidthDp = with(density) { windowInfo.containerSize.width.toDp() }
    val dialogMaxWidth = (containerWidthDp * 0.6f).coerceIn(320.dp, 600.dp)

    Dialog(onDismissRequest = cancel) {
        Surface(
            shape = MaterialTheme.shapes.extraLarge,
            color = colorScheme.surfaceContainerHigh,
            tonalElevation = 6.dp,
            modifier = Modifier.widthIn(max = dialogMaxWidth),
        ) {
            Column(modifier = Modifier.padding(24.dp)) {
                DialogHeader()
                Spacer(modifier = Modifier.height(20.dp))
                Column(
                    modifier = Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    ImmersionCard(selectedMode) { selectedMode = it }
                }
                Spacer(modifier = Modifier.height(24.dp))
                Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    TextButton(onClick = cancel, modifier = Modifier.weight(1f)) { Text("Cancel") }
                    Button(
                        onClick = {
                            applied = true
                            onApply(selectedMode)
                        },
                        modifier = Modifier.weight(1.5f),
                        enabled = selectedMode != currentMode,
                    ) {
                        Icon(imageVector = Icons.Default.RestartAlt, contentDescription = null, modifier = Modifier.size(18.dp))
                        Spacer(modifier = Modifier.width(8.dp))
                        Text("Apply & Restart")
                    }
                }
            }
        }
    }
}

@Composable
private fun DialogHeader() {
    val colorScheme = MaterialTheme.colorScheme
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(
            imageVector = Icons.Default.Fullscreen,
            contentDescription = null,
            tint = colorScheme.primary,
            modifier = Modifier.size(28.dp),
        )
        Spacer(modifier = Modifier.width(16.dp))
        Text(text = "Display Mode", style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.Bold))
    }
    Spacer(modifier = Modifier.height(8.dp))
    Text(
        text = "Preview changes instantly • Session restarts to apply",
        style = MaterialTheme.typography.bodyMedium,
        color = colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun ImmersionCard(
    selectedMode: DisplayMode,
    onSelect: (DisplayMode) -> Unit,
) {
    OptionCard(
        title = "App Immersion",
        description = "Control system UI visibility during projection",
        icon = Icons.Default.Layers,
    ) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            PICKER_ORDER.forEach { mode ->
                OptionButton(
                    label = mode.pickerLabel,
                    icon = mode.icon,
                    isSelected = selectedMode == mode,
                    onClick = { onSelect(mode) },
                    modifier = Modifier.weight(1f),
                )
            }
        }
        Spacer(modifier = Modifier.height(12.dp))
        Text(
            text = selectedMode.summary,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.primary,
        )
    }
}

/** Show the bars [mode] keeps and hide the ones it drops — the same calls MainActivity makes when it applies a mode. */
private fun previewDisplayMode(
    window: Window,
    mode: DisplayMode,
) {
    val controller = WindowCompat.getInsetsController(window, window.decorView)
    if (mode.visibleBarTypes != 0) controller.show(mode.visibleBarTypes)
    if (mode.hiddenBarTypes != 0) controller.hide(mode.hiddenBarTypes)
    controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
}
