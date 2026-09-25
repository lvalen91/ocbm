package com.carlink.ui.components

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.carlink.ui.theme.AutomotiveDimens

/** Visual severity of a [ControlButton]: WARNING = tertiary/amber tonal, DESTRUCTIVE = solid error red. */
enum class ButtonSeverity { WARNING, DESTRUCTIVE }

/** What a [ControlButton] shows: label, leading icon and its colour class. */
@Immutable
data class ControlAction(
    val label: String,
    val icon: ImageVector,
    val severity: ButtonSeverity,
)

/**
 * Titled Material 3 card used as the container for a group of control actions on the Settings
 * Control tab ("Adapter", "App Control"): a leading icon + title header row, then [content].
 */
@Composable
fun ControlCard(
    title: String,
    icon: ImageVector,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val colorScheme = MaterialTheme.colorScheme
    Card(
        modifier = modifier.fillMaxWidth(),
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
    ) {
        Column(modifier = Modifier.padding(24.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    tint = colorScheme.primary,
                    modifier = Modifier.size(24.dp),
                )
                Spacer(modifier = Modifier.width(12.dp))
                Text(
                    text = title,
                    style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
            }
            Spacer(modifier = Modifier.height(20.dp))
            content()
        }
    }
}

/**
 * Action button inside a [ControlCard]. Swaps its leading icon for a spinner while [isProcessing];
 * colours come from [ControlAction.severity]. Minimum height is the automotive touch target — the
 * button grows with its text rather than clipping it.
 */
@Composable
fun ControlButton(
    action: ControlAction,
    enabled: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    isProcessing: Boolean = false,
) {
    val colorScheme = MaterialTheme.colorScheme
    val containerColor: Color
    val contentColor: Color
    when (action.severity) {
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
                Icon(imageVector = action.icon, contentDescription = action.label, modifier = Modifier.size(24.dp))
            }
        }
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = action.label,
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}
