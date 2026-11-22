// The protocol, apart from Android so it tests on any JVM: the end-to-end
// encryption, the relay link and what the computer sends.
plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    api(libs.serialization.json)
    api(libs.coroutines.core)
    api(libs.okhttp)
    testImplementation(kotlin("test"))
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
}

tasks.test {
    useJUnitPlatform()
    // The vectors core/src/phone/e2e.rs writes, which phone/e2e.js and the iOS
    // app are checked against too.
    systemProperty("vectors", rootProject.file("../phone/test/vectors").absolutePath)
    inputs.dir(rootProject.file("../phone/test/vectors"))
}
