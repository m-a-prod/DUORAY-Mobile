#!/bin/sh
# Builds the APK: xray (latest release), the Rust library for each ABI,
# then the Kotlin shell with Gradle.
#   scripts/build-apk.sh [debug|release]
# GRADLE_ARGS go to Gradle (scripts/release.sh passes the version).
# Needs ANDROID_HOME, ANDROID_NDK_HOME, cargo-ndk and the Rust Android targets.
# DUORAY must be checked out next to this repository (duoray-core).
set -eu
cd "$(dirname "$0")/.."
PROFILE=${1:-debug}
ABIS=${ABIS:-arm64-v8a armeabi-v7a x86_64}

: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the NDK}"
ANDROID_NDK="$ANDROID_NDK_HOME"
export ANDROID_NDK
# Slint's build script cannot read SDK platform dirs named like android-37.0.
if [ -z "${ANDROID_JAR:-}" ]; then
    ANDROID_JAR=$(ls -d "$ANDROID_HOME"/platforms/android-*/android.jar | sort -V | tail -1)
    export ANDROID_JAR
fi

scripts/fetch-xray.sh

release=""
[ "$PROFILE" = release ] && release=--release
for abi in $ABIS; do
    (
        if [ "$abi" = armeabi-v7a ]; then
            # Skia is built from source for this ABI. Its unversioned target
            # triple is rejected by newer NDK headers.
            # Use the parser shipped with the NDK for its C++ headers.
            for lib in "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/lib/libclang.so; do
                if [ -f "$lib" ]; then
                    export LIBCLANG_PATH="$lib"
                fi
            done
            export SKIA_GN_ARGS='extra_cflags+=["--target=armv7-linux-androideabi26"]'
            export CXXFLAGS_armv7_linux_androideabi="--target=armv7-linux-androideabi26"
            set -- env "BINDGEN_EXTRA_CLANG_ARGS_armv7-linux-androideabi=--target=armv7-linux-androideabi26 -D__ANDROID_MIN_SDK_VERSION__=26"
        else
            set --
        fi
        "$@" cargo ndk -t "$abi" -P 26 -o android/app/src/main/jniLibs build --lib $release
    )
done

task=assembleDebug
[ "$PROFILE" = release ] && task=assembleRelease
cd android
# shellcheck disable=SC2086
./gradlew --no-daemon "$task" ${GRADLE_ARGS:-}
ls -1 app/build/outputs/apk/"$PROFILE"/*.apk
