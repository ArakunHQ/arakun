import com.android.build.api.variant.ApplicationAndroidComponentsExtension

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "com.arakun.android"
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }

    defaultConfig {
        applicationId = "com.arakun.app"
        minSdk = 26
        targetSdk = 36
        // android-release.yml sets both: the version is Arakun's, the code a
        // number that only goes up, as Google Play wants.
        versionCode = providers.gradleProperty("versionCode").map(String::toInt).getOrElse(1)
        versionName = providers.gradleProperty("versionName").getOrElse("0.19.0")
        // Crash reports go to Arakun's PostHog project, as the desktop's do
        // (src/telemetry.rs). The token is write-only, so it is safe in the
        // app. `-PposthogKey=` (empty) builds an app that sends nothing.
        val posthogKey = providers.gradleProperty("posthogKey").getOrElse("phc_kXKrLfaWumKa3mfXcFVBCRpyKa6DZiUv7hCkQW2epLQZ")
        buildConfigField("String", "POSTHOG_KEY", "\"$posthogKey\"")
        buildConfigField("String", "POSTHOG_HOST", "\"https://us.i.posthog.com\"")
    }

    // The upload key Google Play knows the app by, for a bundle that goes
    // there (android-release.yml passes it as ORG_GRADLE_PROJECT_* variables).
    // Without it a release build is unsigned; CI signs with a throwaway key.
    val uploadStore = providers.gradleProperty("uploadStoreFile").orNull
    signingConfigs {
        if (uploadStore != null) {
            create("upload") {
                // From android/, where ./gradlew runs, not from app/.
                storeFile = rootProject.file(uploadStore)
                storePassword = providers.gradleProperty("uploadStorePassword").get()
                keyAlias = providers.gradleProperty("uploadKeyAlias").get()
                keyPassword = providers.gradleProperty("uploadKeyPassword").get()
            }
        }
    }

    buildTypes {
        release {
            if (uploadStore != null) signingConfig = signingConfigs.getByName("upload")
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
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

    testOptions {
        unitTests.all { it.useJUnitPlatform() }
    }

    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }
}

kotlin {
    jvmToolchain(17)
}

/**
 * Takes the brand's fonts and the mascot's drawing from assets/ at build time,
 * rather than keeping copies, so the app and the desktop never drift apart.
 * Android wants resource names in lower case with underscores.
 */
abstract class BrandResources : DefaultTask() {
    @get:InputDirectory
    abstract val fonts: DirectoryProperty

    @get:OutputDirectory
    abstract val output: DirectoryProperty

    @TaskAction
    fun copy() {
        val dir = output.get().asFile.resolve("font")
        dir.deleteRecursively()
        dir.mkdirs()
        fonts.get().asFile.listFiles { f -> f.extension == "ttf" }!!.forEach { f ->
            val name = f.nameWithoutExtension
                .replace(Regex("([a-z])([A-Z])"), "$1_$2")
                .replace('-', '_')
                .lowercase()
            f.copyTo(dir.resolve("$name.ttf"), overwrite = true)
        }
    }
}

abstract class BrandAssets : DefaultTask() {
    @get:InputFile
    abstract val mascot: RegularFileProperty

    @get:OutputDirectory
    abstract val output: DirectoryProperty

    @TaskAction
    fun copy() {
        val dir = output.get().asFile
        dir.mkdirs()
        mascot.get().asFile.copyTo(dir.resolve("mascot.json"), overwrite = true)
    }
}

val repo = rootProject.layout.projectDirectory.dir("..")
val brandResources = tasks.register<BrandResources>("brandResources") {
    fonts.set(repo.dir("assets/fonts"))
}
val brandAssets = tasks.register<BrandAssets>("brandAssets") {
    mascot.set(repo.file("assets/brand/mascot.json"))
}

extensions.getByType<ApplicationAndroidComponentsExtension>().onVariants { variant ->
    variant.sources.res?.addGeneratedSourceDirectory(brandResources, BrandResources::output)
    variant.sources.assets?.addGeneratedSourceDirectory(brandAssets, BrandAssets::output)
}

dependencies {
    implementation(project(":kit"))
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)
    implementation(libs.activity.compose)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.core.ktx)
    implementation(libs.coroutines.android)
    implementation(libs.posthog.android)
    testImplementation(libs.kotlin.test.junit5)
    testRuntimeOnly(libs.junit.launcher)
}
