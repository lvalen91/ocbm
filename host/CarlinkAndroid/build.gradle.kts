// Top-level build file for Carlink Native Android App

plugins {
    // AGP 9.2: lifecycle 2.11.0 formally requires AGP >= 9.2.0 (this was the one
    // out-of-spec pairing in the build); Gradle 9.6.1 satisfies 9.2's floor.
    id("com.android.application") version "9.2.1" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.3.10" apply false
    // ktlint-gradle 14.x: 12.x predates AGP 9 built-in-Kotlin support (added 14.1.0) —
    // the old plugin may not even have been scanning src/main/kotlin.
    id("org.jlleitschuh.gradle.ktlint") version "14.2.0" apply false
    id("io.gitlab.arturbosch.detekt") version "1.23.8" apply false
    // NOTE: com.autonomousapps.dependency-analysis (tried 3.16.0) is NOT yet compatible with
    // AGP 9.1.0 — it registers analyze* tasks but never wires projectHealth/buildHealth, so
    // buildHealth reports projectCount=0. Re-add once the plugin supports AGP 9.x.
}

// Build outputs live OUTSIDE the source tree, deliberately.
//
// This project sits under ~/Documents, which macOS "Desktop & Documents" iCloud sync covers
// (`brctl status` → `Desktop & Documents: current=YES`). iCloud resolves its own races by writing
// conflict copies next to the original — "BuildConfig 2.java", "classes 2.jar",
// "CarlinkManager$Callback$DefaultImpls 2.dex" — and Gradle then feeds those duplicates straight
// back into javac and D8:
//
//     error: class BuildConfig is public, should be declared in a file named BuildConfig.java
//     Type com.carlink.CarlinkManager$Callback$DefaultImpls is defined multiple times
//
// It is not deterministic: a build can pass, then fail minutes later with hundreds of conflict
// copies, and `./gradlew clean` only helps until sync catches up again. Redirecting the build
// directory out of the synced tree removes the input entirely. Same reasoning as this repo's
// Rust `target` symlink into ~/.cache.
//
// Override with -Pcarlink.buildRoot=/some/path if ~/.cache is not writable.
val buildRoot =
    (findProperty("carlink.buildRoot") as String?)
        ?: "${System.getProperty("user.home")}/.cache/gradle-builds/carlink-android-ocbm"

allprojects {
    layout.buildDirectory.set(file("$buildRoot/${project.path.replace(':', '_').trim('_').ifEmpty { "root" }}"))
}
