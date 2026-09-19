package com.carlink.ui.settings

import android.content.Context
import androidx.core.view.WindowInsetsCompat

/**
 * How much of the system UI the projection keeps. A USER CHOICE, persisted, not an assumption
 * about the head unit: the bars a mode leaves visible shrink the content area, and that content
 * area — not the panel — is what goes to the box (`DisplayProfile.detect`), what the video surface
 * occupies, and what touch is normalised over. Changing the mode rebuilds the session.
 *
 * PERSISTENCE CONTRACT: [value] is written to SharedPreferences. Never reassign or reuse a number —
 * a silent renumber corrupts every existing install's stored choice. New modes append fresh
 * integers. The numbers match the older carlink_native app's `DisplayMode` so a stored value from
 * that lineage means the same thing here.
 */
enum class DisplayMode(
    val value: Int,
    /** `WindowInsetsCompat.Type` mask of the bars this mode hides. */
    val hiddenBarTypes: Int,
    val label: String,
    val summary: String,
) {
    /** Both bars visible; CarPlay gets the area between them. */
    SYSTEM_UI_VISIBLE(
        0,
        0,
        "System bars visible",
        "Status and navigation bars stay on screen; CarPlay fills the area between them.",
    ),

    /** Status bar hidden, navigation bar kept. */
    STATUS_BAR_HIDDEN(
        1,
        WindowInsetsCompat.Type.statusBars(),
        "Status bar hidden",
        "More vertical room; the navigation bar stays reachable.",
    ),

    /** Both bars hidden; swipe from an edge to reveal them transiently. The app's original hard-coded behaviour. */
    FULLSCREEN_IMMERSIVE(
        2,
        WindowInsetsCompat.Type.systemBars(),
        "Fullscreen",
        "Both bars hidden; CarPlay uses the whole panel. Swipe from an edge to peek at the bars.",
    ),

    /** Navigation bar hidden, status bar kept. */
    NAV_BAR_HIDDEN(
        3,
        WindowInsetsCompat.Type.navigationBars(),
        "Navigation bar hidden",
        "Status indicators stay; the navigation bar is hidden.",
    ),
    ;

    /** The bars this mode leaves VISIBLE — their stable insets come off the window to give the content area. */
    val visibleBarTypes: Int get() = WindowInsetsCompat.Type.systemBars() and hiddenBarTypes.inv()

    companion object {
        /**
         * Fullscreen everywhere: exactly what the app did before the mode existed, so no panel changes
         * behaviour on upgrade. A per-head-unit default would be an identity check, which this generic
         * AAOS app does not make; the user picks otherwise from the dashboard.
         */
        val DEFAULT = FULLSCREEN_IMMERSIVE

        /** Unknown or corrupt stored values fall back to [DEFAULT]. */
        fun fromValue(value: Int): DisplayMode = entries.firstOrNull { it.value == value } ?: DEFAULT
    }
}

/** The persisted [DisplayMode]. SharedPreferences: tiny, synchronous, read once in `onCreate`. */
class DisplayModeStore(
    context: Context,
) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    fun get(): DisplayMode = DisplayMode.fromValue(prefs.getInt(KEY_MODE, DisplayMode.DEFAULT.value))

    fun set(mode: DisplayMode) {
        prefs.edit().putInt(KEY_MODE, mode.value).apply()
    }

    private companion object {
        // PERSISTENCE CONTRACT: renaming either orphans every stored choice.
        const val PREFS_NAME = "carlink_display_mode"
        const val KEY_MODE = "display_mode"
    }
}
