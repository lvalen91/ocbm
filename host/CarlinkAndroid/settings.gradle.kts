pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "carlink-android-ocbm"
include(":app")

// The Raspberry Pi / AAOS projection system app (pi/docs/01_PROJECTION_APP_DESIGN.md).
//
// It lives in THIS Gradle build rather than under `pi/` as a standalone project for one reason:
// it shares the forward-encrypted A/V seam and the decoders with :app by source directory, so
// there is exactly ONE copy of SeamCrypto/VideoSeam/AudioSeam/HevcRenderer/AacPlayer. Those files
// carry byte-exact framing and ChaCha20-Poly1305 details that were confirmed against Apple's
// CarPlaySDK; a fork of them would drift silently and fail as "decrypt failed" months later.
//
// :app talks OCBM to a CCPA over USB. :projection consumes the same seam bytes straight off
// localhost on the Pi. Different transport, same seam — see ProjectionSeamServer.
include(":projection")

// Compile-only @SystemApi declarations for android.car. The public SDK stub strips every
// projection API; see car-system-stubs/src/main/java/android/car/CarProjectionManager.java.
include(":car-system-stubs")

// GM AAOS USB-permission "handler squat" PROOF app (docs/59). Package android.car.usb.handler —
// it impersonates the stock component GM's framework-res points at but stripped, so the framework
// grants USB permission to it silently on every attach. Deliberately a SEPARATE module from :app:
// it is a throwaway proof exercised by logcat and must never share identity with, or install over,
// the daily-driver OCBM host app (zeno.carlink.ocbm). Standalone — depends on nothing else here.
include(":usbhandler")
