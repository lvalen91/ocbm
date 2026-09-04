// CallSim — self-managed Telecom fake-call test app. Toolchain mirrors ../CarlinkAndroid
// (same AGP / Gradle wrapper, `gradlew` and `gradle/` are symlinks into it).
plugins {
    id("com.android.application") version "9.2.1" apply false
}

// Build outputs live OUTSIDE the source tree: this path is under ~/Documents, which iCloud
// "Desktop & Documents" sync covers, and sync writes "classes 2.jar"-style conflict copies
// that D8 then picks up. Same fix as ../CarlinkAndroid. Override with -Pcallsim.buildRoot=.
val buildRoot =
    (findProperty("callsim.buildRoot") as String?)
        ?: "${System.getProperty("user.home")}/.cache/gradle-builds/callsim"

allprojects {
    layout.buildDirectory.set(file("$buildRoot/${project.path.replace(':', '_').trim('_').ifEmpty { "root" }}"))
}
