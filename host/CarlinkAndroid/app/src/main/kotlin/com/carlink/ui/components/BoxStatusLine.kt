package com.carlink.ui.components

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import com.carlink.CarlinkManager

/**
 * "WIRED_CP · HCI|SSP|iap2d|airplayd" — which transport owns the box (CT_PROJ_MODE) and which of its
 * subsystems are alive (CT_BOX_HEALTH). Subscribes itself via [CarlinkManager.BoxStatusListener];
 * lays out as nothing until the box has said anything.
 */
@Composable
fun BoxStatusLine(
    carlinkManager: CarlinkManager,
    modifier: Modifier = Modifier,
) {
    var text by remember(carlinkManager) { mutableStateOf(carlinkManager.boxStatusText()) }
    DisposableEffect(carlinkManager) {
        val l = CarlinkManager.BoxStatusListener { text = it }
        carlinkManager.addBoxStatusListener(l)
        onDispose { carlinkManager.removeBoxStatusListener(l) }
    }
    if (text.isEmpty()) return
    Text(
        text = text,
        modifier = modifier,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
    )
}
