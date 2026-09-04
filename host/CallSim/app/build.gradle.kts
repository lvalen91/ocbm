plugins {
    id("com.android.application") // AGP 9.x: Kotlin support is built in, no kotlin-android plugin
}

android {
    namespace = "com.carlink.callsim"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.carlink.callsim"
        minSdk = 31 // Notification.CallStyle, ForegroundServiceStartNotAllowedException
        targetSdk = 36
        versionCode = 1
        versionName = "0.1"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        buildConfig = false
        aidl = false
    }
}

// Framework-only app for calls: Telecom, AudioTrack/AudioRecord, Notification.CallStyle are all in
// android.jar at API 31+. The ONE AndroidX dependency is for the NOTIFY test: Android Auto's
// messaging pipeline reads NotificationCompat's action flags (showsUserInterface), which the
// platform Notification.Action API cannot set (device-observed 2026-09-04).
dependencies {
    implementation("androidx.core:core:1.15.0")   // 1.19 needs compileSdk 37; this app compiles against 36
}
