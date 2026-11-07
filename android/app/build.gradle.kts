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
        // 37 turns on Android 17's local network protection: xray's direct
        // connections to the LAN («Локальная сеть» напрямую) and the hotspot
        // proxy would need a runtime permission. MaterialXray stays on 36 too.
        targetSdk = 36
        versionCode = (findProperty("versionCode") as String?)?.toInt() ?: 1
        versionName = (findProperty("versionName") as String?) ?: "0.1.0"
        // With -PabiSplit=true the split below picks the ABIs instead.
        if (findProperty("abiSplit") != "true") {
            ndk {
                abiFilters += listOf("arm64-v8a", "x86_64")
            }
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
            // DUORAY_UNSIGNED=1 (the release workflow): left unsigned, for
            // scripts/release.sh --from-github to sign on the release machine.
            signingConfig = when {
                System.getenv("DUORAY_UNSIGNED") == "1" -> null
                keystore != null -> signingConfigs.getByName("release")
                else -> signingConfigs.getByName("debug")
            }
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
    // Notifications (Live Updates), FileProvider, insets.
    implementation("androidx.core:core:1.19.1")
    // ComponentActivity for the scanner: edge to edge, permissions, the photo picker.
    implementation("androidx.activity:activity:1.13.0")
    // The QR scanner: CameraX for the camera, zxing-cpp (Apache-2.0) to decode.
    implementation("androidx.camera:camera-camera2:1.6.2")
    implementation("androidx.camera:camera-lifecycle:1.6.2")
    implementation("androidx.camera:camera-view:1.6.2")
    implementation("io.github.zxing-cpp:android:3.1.1")
}
