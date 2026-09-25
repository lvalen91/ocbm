package com.carlink.ui

import androidx.compose.runtime.Immutable
import com.carlink.ui.settings.DisplayMode

/**
 * What the Settings screen can ask of its host (MainActivity via [CarlinkApp]).
 *
 * @property onNavigateBack close the overlay and return to the projection.
 * @property onDisplayModeSelected persist a new [DisplayMode] and rebuild the session for it — or,
 *   while a phone session owns the box, only persist it ([DisplayModeOutcome.DEFERRED]).
 */
@Immutable
class SettingsActions(
    val onNavigateBack: () -> Unit,
    val onDisplayModeSelected: (DisplayMode) -> DisplayModeOutcome,
)

/** What happened to a display-mode choice (MacHost's mid-session rule, `OCBMClient.swift`). */
enum class DisplayModeOutcome {
    /** Applied now: the session was rebuilt for the new content area. */
    APPLIED,

    /** Persisted only; applies at the next connection because a phone session is active. */
    DEFERRED,
}
