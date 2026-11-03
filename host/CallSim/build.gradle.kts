// CallSim — self-managed Telecom fake-call test app. Toolchain mirrors ../CarlinkAndroid
// (same AGP / Gradle wrapper, `gradlew` and `gradle/` are symlinks into it).
plugins {
    id("com.android.application") version "9.2.1" apply false
}

// Build outputs live OUTSIDE the source tree: the tree may sit in a cloud-synced folder (iCloud
// "Desktop & Documents" on the maintainer's Mac), and sync writes "classes 2.jar"-style conflict copies
// that D8 then picks up. Same fix as ../CarlinkAndroid. Override with -Pcallsim.buildRoot=.
// Root: $OCBM_CACHE, else ${TMPDIR:-/tmp}/ocbm-cache (same rule as ../CarlinkAndroid).
val ocbmCache =
    System.getenv("OCBM_CACHE")?.takeIf { it.isNotEmpty() }
        ?: "${(System.getenv("TMPDIR")?.takeIf { it.isNotEmpty() } ?: "/tmp").trimEnd('/')}/ocbm-cache"
val buildRoot =
    (findProperty("callsim.buildRoot") as String?)
        ?: "${ocbmCache.trimEnd('/')}/gradle-builds/callsim"

allprojects {
    layout.buildDirectory.set(file("$buildRoot/${project.path.replace(':', '_').trim('_').ifEmpty { "root" }}"))
}
