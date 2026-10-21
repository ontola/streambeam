plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// Store bundles must never silently fall back to the preview/debug identity.
val signingVariables = listOf(
    "STREAMBEAM_KEYSTORE_FILE", "STREAMBEAM_KEYSTORE_PASSWORD",
    "STREAMBEAM_KEY_ALIAS", "STREAMBEAM_KEY_PASSWORD",
)
val signingValues = signingVariables.associateWith { System.getenv(it).orEmpty() }
val hasStoreSigning = signingValues.values.all { it.isNotBlank() }
require(signingValues.values.all { it.isBlank() } || hasStoreSigning) {
    "Incomplete StreamBeam signing configuration: set all four STREAMBEAM_KEYSTORE/KEY variables."
}
gradle.taskGraph.whenReady {
    if (allTasks.any { it.path == ":app:bundleRelease" } && !hasStoreSigning) {
        throw GradleException("Release App Bundles require the StreamBeam upload signing key.")
    }
}

android {
    namespace = "io.streambeam.streambeam"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "io.streambeam.streambeam"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        // NDK `libnativewindow` (MediaCodec → Surface) is only linked from API 26+.
        minSdk = maxOf(flutter.minSdkVersion, 26)
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (hasStoreSigning) {
            create("store") {
                storeFile = file(signingValues.getValue("STREAMBEAM_KEYSTORE_FILE"))
                storePassword = signingValues.getValue("STREAMBEAM_KEYSTORE_PASSWORD")
                keyAlias = signingValues.getValue("STREAMBEAM_KEY_ALIAS")
                keyPassword = signingValues.getValue("STREAMBEAM_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            // APK previews retain their existing identity. Store bundles require
            // the explicit upload identity, enforced above.
            signingConfig = signingConfigs.getByName(if (hasStoreSigning) "store" else "debug")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
