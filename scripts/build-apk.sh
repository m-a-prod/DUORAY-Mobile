#!/bin/sh
# Builds the APK: xray (latest release), the Rust library for each ABI,
# then the Kotlin shell with Gradle.
#   scripts/build-apk.sh [debug|release]
# Needs ANDROID_HOME, ANDROID_NDK_HOME, cargo-ndk and the Rust Android targets.
# DUORAY must be checked out next to this repository (duoray-core).
set -eu
cd "$(dirname "$0")/.."
PROFILE=${1:-debug}
ABIS=${ABIS:-arm64-v8a x86_64}

: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the NDK}"
# Slint's build script cannot read SDK platform dirs named like android-37.0.
if [ -z "${ANDROID_JAR:-}" ]; then
    ANDROID_JAR=$(ls -d "$ANDROID_HOME"/platforms/android-*/android.jar | sort -V | tail -1)
    export ANDROID_JAR
fi

scripts/fetch-xray.sh

targets=""
for abi in $ABIS; do targets="$targets -t $abi"; done
release=""
[ "$PROFILE" = release ] && release=--release
# shellcheck disable=SC2086
cargo ndk $targets -P 26 -o android/app/src/main/jniLibs build --lib $release

task=assembleDebug
[ "$PROFILE" = release ] && task=assembleRelease
cd android
./gradlew --no-daemon "$task"
ls -1 app/build/outputs/apk/"$PROFILE"/*.apk
