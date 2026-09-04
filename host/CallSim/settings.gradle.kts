pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
plugins {
    // Needed so the JDK 21 daemon toolchain pinned in gradle/gradle-daemon-jvm.properties
    // (shared with ../CarlinkAndroid via the `gradle` symlink) can be auto-provisioned.
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "callsim"
include(":app")
