plugins {
    id("com.android.application")
}

// ─────────────────────────────────────────────────────────────────────────────
// The GM AAOS USB-permission "handler squat" proof app. See docs/59.
//
// GM's Y181 image ships `config_UsbDeviceConnectionHandling_component =
// android.car.usb.handler/android.car.usb.handler.UsbHostManagementActivity` but STRIPS the
// package, so `deviceAttachedForFixedHandler` throws NameNotFoundException and the third-party
// implicit-grant path dead-ends → the permission dialog reappears on every attach.
//
// This module installs a /data app UNDER that exact package name. When present in the foreground
// user (user 10), the framework resolves THIS app as the fixed handler and calls
// `grantDevicePermission(device, ourUid)` SILENTLY on every attach — no dialog, no root.
//
// It is a PROOF, not the product: its whole job is to attach, confirm it already holds permission
// (hasPermission == true with NO requestPermission call), open the device, and log it. It stays a
// SEPARATE module so the daily-driver `:app` (zeno.carlink.ocbm) is never touched.
//
// Structure mirrors AOSP `packages/services/Car/car-usb-handler`; divergences (NoDisplay instead of
// the Dialog picker, no MANAGE_USB) are called out in the manifest — a /data untrusted_app cannot
// hold MANAGE_USB and does not need it here.
// ─────────────────────────────────────────────────────────────────────────────

android {
    // MUST equal the stripped package GM points its fixed-handler config at. This is the entire
    // mechanism — a different applicationId is just an ordinary app and gets the dialog.
    namespace = "android.car.usb.handler"
    compileSdk = 37

    defaultConfig {
        applicationId = "android.car.usb.handler"
        minSdk = 32 // GM gminfo37 = Android 12L / API 32
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-squatproof"
    }

    buildTypes {
        // No minify: this is a debug proof exercised by logcat; keep class/method names readable.
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        // No Compose, no BuildConfig, no AndroidX — the proof uses only platform classes
        // (android.app.Activity, android.hardware.usb.*, android.util.Log) so the APK is tiny and
        // the build has nothing to break.
        buildConfig = false
    }
}
