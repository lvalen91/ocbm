plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jlleitschuh.gradle.ktlint")
    id("io.gitlab.arturbosch.detekt")
}

android {
    namespace = "com.carlink"
    compileSdk = 37

// ###############################################
// ###############################################
// ###############################################

    defaultConfig {
        // DISTINCT from the riddleBox app's `zeno.carlink` ON PURPOSE. carlink_native_personal is
        // the working, favored app against stock firmware and is the fallback while this one is
        // built; sharing an applicationId would install over it and leave no way back on a head
        // unit that is also the daily driver. Both can be installed side by side and A/B'd.
        applicationId = "zeno.carlink.ocbm"
        minSdk = 32 // GM gminfo = Android 12L / API 32
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-ocbm"

// ###############################################
// ###############################################
// ###############################################

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        vectorDrawables {
            useSupportLibrary = true
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    // Sideload-only (cp-stripped): a single APK installed directly on the head unit, never
    // uploaded to Play. The cluster-icon ContentProvider authority machinery + the `play`
    // flavor were removed along with cluster navigation. The single flavor is retained so the
    // build variant stays `sideloadDebug`/`sideloadRelease`.
    flavorDimensions += "distribution"

    productFlavors {
        create("sideload") {
            dimension = "distribution"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true // Enable BuildConfig generation for debug checks
        aidl = false
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    // android.car is an OPTIONAL shared library, present only on automotive builds. useLibrary puts
    // the stubs on the COMPILE classpath only — it does not bundle them, so the app still installs
    // and runs on a phone. DriveStateMonitor guards every touch behind FEATURE_AUTOMOTIVE.
    useLibrary("android.car")

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
        }
    }

    lint {
        // Baseline mirrors detekt-baseline.xml: records pre-existing findings (incl. the
        // opinionated compose-lints checks) so the build stays green and only NEW issues
        // gate. Regenerate with `./gradlew updateLintBaseline` after fixing items.
        baseline = file("lint-baseline.xml")

        // (Tier 4) Former module-global disables scoped down:
        // - DiscouragedApi (Timer.scheduleAtFixedRate for the heartbeat) → @SuppressLint
        //   at the call site in AdapterDriver. A global disable also silenced every
        //   FUTURE discouraged-API use anywhere in the app.
        // - UnsafeOptInUsageError (Media3 @UnstableApi tool disagreement — compiler
        //   treats it as non-opt-in, lint demands the opt-in) → path-scoped ignore in
        //   lint.xml covering only the media package. Re-test after AGP/Media3 bumps.
        // - Instantiatable / InvalidUsesTagAttribute — their justifications
        //   (CarAppActivity AAR, the "navigation" uses-tag) died with the cluster /
        //   Car App Library removal; re-enabled.
    }
}

// Kotlin compiler: report deprecations and unchecked casts as warnings
tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach {
    compilerOptions {
        freeCompilerArgs.addAll("-opt-in=kotlin.RequiresOptIn")
        allWarningsAsErrors.set(false) // report but don't fail — tighten later
    }
}

ktlint {
    android.set(true)
    outputToConsole.set(true)
    // Enforcing (Tier 4): a ktlintFormat pass ran against the 14.x engine, so only NEW
    // violations gate the build from here on.
    ignoreFailures.set(false)
}

detekt {
    buildUponDefaultConfig = true
    allRules = false
    config.setFrom(files("$rootDir/detekt.yml"))
    baseline = file("$rootDir/detekt-baseline.xml")
    // Enforcing (Tier 4): baseline regenerated 2026-07-07, so only NEW findings gate.
    ignoreFailures = false
}

dependencies {
    // (Removed: detekt-formatting. Its embedded ktlint (~0.50) disagrees with the
    // standalone ktlint-gradle 14.x engine that now formats AND gates the build —
    // running both baselined ~70 entries of pure engine disagreement. One formatter,
    // one gate: ktlint owns formatting; detekt owns code smells.)

    // Compose-specific Android lint rules (Slack): catches Modifier-param omissions,
    // unstable-collection params, remember/state-hoisting mistakes that base lint misses.
    lintChecks("com.slack.lint.compose:compose-lint-checks:1.5.2")

    // Kotlin
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")

    // AndroidX Core (core-ktx is an empty compatibility artifact as of 1.19.0 — the
    // Kotlin extensions merged into androidx.core:core)
    implementation("androidx.core:core:1.19.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.11.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.activity:activity-compose:1.13.0")

    // (Removed: androidx.datastore — AdapterConfigPreference's DataStore layer was
    // write-only dead I/O; SharedPreferences was always the effective store and is now
    // the only one. Removed: androidx.documentfile — its "transitive via legacy-support"
    // justification died with the Media3 migration; nothing pulls it.)

    // Compose BOM
    implementation(platform("androidx.compose:compose-bom:2026.06.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")

    // MediaSession for AAOS integration — Media3 1.10.1.
    // media3-session supersedes legacy androidx.media:media (MediaSessionCompat, deprecated
    // in androidx.media 1.8.0-alpha01). GM AAOS observers use platform android.media.session.*
    // APIs which Media3 auto-registers under the hood for backwards compatibility, so the
    // GMCarMediaService → ClusterService → cluster pipeline keeps working.
    // media3-common provides Player / SimpleBasePlayer / MediaItem / MediaMetadata.
    // media3-exoplayer is intentionally NOT included: this app does not decode/play audio
    // locally — the connected phone plays over USB; we only mirror metadata + forward commands.
    val media3Version = "1.10.1"
    implementation("androidx.media3:media3-session:$media3Version")
    implementation("androidx.media3:media3-common:$media3Version")

    // Testing
    testImplementation("junit:junit:4.13.2")
    // Real org.json for unit tests. The android.jar the unit-test classpath uses ships STUBS whose
    // methods throw ("not mocked"), so anything parsing JSON off the device — the known-device
    // store's document, MGMT_INFO — is untestable without this. Same coordinates AOSP itself uses.
    testImplementation("org.json:json:20250107")
    testImplementation("org.robolectric:robolectric:4.16.1")
    testImplementation("androidx.test:core:1.7.0")
    testImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.7.0")
    androidTestImplementation(platform("androidx.compose:compose-bom:2026.06.00"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}
