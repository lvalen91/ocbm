plugins {
    id("java-library")
}

// COMPILE-ONLY STUBS. Nothing here is ever packaged — see the note in src/main/java.
java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}
