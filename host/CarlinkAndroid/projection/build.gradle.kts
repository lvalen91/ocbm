plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jlleitschuh.gradle.ktlint")
    id("io.gitlab.arturbosch.detekt")
}

android {
    namespace = "com.carlink.projection"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.carlink.projection"

        // AAOS 16 on the Pi. Higher than :app's 32 on purpose: this module is NOT the GM
        // Android-12L app, it is a system app for one known platform, so nothing is gained by
        // carrying 12L compatibility and CarProjectionManager's modern surface is assumed
        // present rather than reflected around.
        minSdk = 34
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-pi"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            // Deliberately NOT minified. This is a sideloaded priv-app whose entry points are
            // reached by the platform (Settings injection, boot receiver, CarProjectionManager
            // reverse-binding) rather than from our own code, so R8's reachability analysis has
            // no way to see them. A keep-rules file would have to enumerate the same set and
            // would rot; the APK is installed by hand on one device and size is irrelevant.
            isMinifyEnabled = false
        }
    }

    // Source sharing with :app. See the note in settings.gradle.kts for why these are shared
    // rather than copied. Kept to the exact transitive closure of the seam + decoders:
    //
    //   ProbeLog          — the only logging dependency any of the rest has
    //   SeamCrypto        — ChaCha20-Poly1305 open, confirmed byte-for-byte vs CarPlaySDK
    //   SeamPipe          — the bounded hand-off to a decoder thread
    //   VideoSeam         — fwd-enc "SEAV" framing -> Annex-B, incl. hvcC/avcC unwrapping
    //   AudioSeam         — fwd-enc audio framing -> ADTS / tagged ELD
    //   HevcRenderer      — MediaCodec video, consumes [u32 BE len][Annex-B]
    //   AacPlayer         — MediaCodec + AudioTrack
    //
    // MetadataSeam is deliberately absent: it depends on `com.carlink.ocbm.Ocbm`, and the Pi
    // gets metadata off the iAP2 DataStream rather than an OCBM channel.
    sourceSets {
        getByName("main") {
            kotlin.srcDir("shared")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    // CarProjectionManager, CarUxRestrictionsManager, CarOccupantZoneManager all live here.
    // Compile-only stubs — the platform provides the implementation.
    useLibrary("android.car")

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            isReturnDefaultValues = true
        }
    }

    lint {
        // The privileged permissions this app declares are granted by the priv-app allowlist
        // installed alongside it, not by the manifest alone. Lint cannot see that file and
        // reports every one of them as a permission the app can never hold.
        disable += "ProtectedPermissions"
        abortOnError = false
    }
}

ktlint {
    android.set(true)
    outputToConsole.set(true)
    ignoreFailures.set(false)
    // The shared sources are :app's to format; formatting them from here would fight that
    // module's own ktlint task over the same files.
    filter {
        exclude("**/shared/**")
    }
}

detekt {
    buildUponDefaultConfig = true
    allRules = false
    config.setFrom(files("$rootDir/detekt.yml"))
    baseline = file("$rootDir/detekt-baseline.xml")
    ignoreFailures = true
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("androidx.core:core:1.19.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.11.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.activity:activity-compose:1.13.0")

    implementation(platform("androidx.compose:compose-bom:2026.06.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.material3:material3")

    // @SystemApi surface of android.car that the public stub omits. compileOnly is load-bearing:
    // packaging a second android.car.CarProjectionManager would shadow the platform's real class.
    compileOnly(project(":car-system-stubs"))

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20250107")
    testImplementation("org.robolectric:robolectric:4.16.1")
    testImplementation("androidx.test:core:1.7.0")
    testImplementation("androidx.test.ext:junit:1.3.0")
}
