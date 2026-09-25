package com.carlink.ui.settings

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.Settings
import androidx.compose.ui.graphics.vector.ImageVector

/**
 * Tabs in the Settings screen's navigation rail, in display order. The carlink_native lineage
 * also had a Logs tab (file logging + log-level presets); this build's [com.carlink.logging.Logger]
 * has no file sink or level switch — `adb logcat` is the only sink — so there is nothing for that
 * tab to control and it is deliberately absent rather than rendered as a dead panel.
 */
enum class SettingsTab(
    val title: String,
    val icon: ImageVector,
) {
    PHONES("Phones", Icons.Default.PhoneAndroid),
    CONTROL("Control", Icons.Default.Settings),
}
