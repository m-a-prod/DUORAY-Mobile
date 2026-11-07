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

    buildTypes {
        release {
            isMinifyEnabled = false
            // Until a release key exists: installable, not publishable.
            signingConfig = signingConfigs.getByName("debug")
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
