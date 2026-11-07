// The Android shell around the Rust app. scripts/build-apk.sh puts the
// native parts in place before Gradle runs: libduoray_mobile.so (cargo-ndk)
// and libxray.so plus the geo databases (scripts/fetch-xray.sh).

plugins {
    id("com.android.application")
}

android {
    namespace = "space.dualizm.duoray"
    compileSdk = 37

    defaultConfig {
        applicationId = "space.dualizm.duoray"
        // The Slint Android backend needs API 26.
        minSdk = 26
        targetSdk = 37
        versionCode = (findProperty("versionCode") as String?)?.toInt() ?: 1
        versionName = (findProperty("versionName") as String?) ?: "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    // Releases (scripts/release.sh) are signed with the key in
    // DUORAY_KEYSTORE; updates install only over an APK with the same key.
    // Without it a release build is signed for debugging: installable, not
    // publishable.
    val keystore = System.getenv("DUORAY_KEYSTORE")
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("DUORAY_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("DUORAY_KEY_ALIAS") ?: "duoray"
                keyPassword = System.getenv("DUORAY_KEY_PASSWORD") ?: System.getenv("DUORAY_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName(if (keystore != null) "release" else "debug")
        }
    }

    // -PabiSplit=true: one APK per ABI (the hub serves each phone its own).
    splits {
        abi {
            isEnable = findProperty("abiSplit") == "true"
            reset()
            include("arm64-v8a", "x86_64")
            isUniversalApk = false
        }
    }

    packaging {
        jniLibs {
            // Extract native libraries to disk: Android only runs executables
            // (libxray.so) from the app's native library directory.
            useLegacyPackaging = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    // The QR scanner (camera screen and decoder).
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
    // The scanner asks for the camera through it but does not bring it along.
    implementation("androidx.core:core:1.19.1")
}
