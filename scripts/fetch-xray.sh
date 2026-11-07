#!/bin/sh
# Puts the newest stable Xray-core into the APK: the official Android
# builds as lib/<abi>/libxray.so (Android runs executables only from an
# app's native library directory) and their geo databases as assets.
# XRAY_TAG=v26.10.31 pins a release instead of the latest one.
set -eu
cd "$(dirname "$0")/.."

# latest_tag OWNER/REPO: newest stable release, from GitHub's releases/latest
# redirect (no API rate limit). Same as DUORAY's packaging/latest.sh.
latest_tag() {
    tag=$(curl -fsSI --retry 3 --retry-delay 5 --connect-timeout 20 "https://github.com/$1/releases/latest" |
        sed -n 's|^[Ll]ocation: .*/tag/\([^[:space:]]*\).*|\1|p')
    [ -n "$tag" ] || { echo "cannot find the latest release of $1" >&2; return 1; }
    echo "$tag"
}

# xray_sha256 TAG ZIP: SHA-256 of an Xray-core archive, from its .dgst file.
xray_sha256() {
    sha=$(curl -fsSL --retry 3 --retry-delay 5 --connect-timeout 20 "https://github.com/XTLS/Xray-core/releases/download/$1/$2.dgst" |
        sed -n 's/^SHA2-256= *//p')
    [ -n "$sha" ] || { echo "no SHA-256 published for xray $1 $2" >&2; return 1; }
    echo "$sha"
}

TAG=${XRAY_TAG:-$(latest_tag XTLS/Xray-core)}
JNI=android/app/src/main/jniLibs
ASSETS=android/app/src/main/assets/xray
if [ -f "$JNI/.xray-$TAG" ]; then
    echo "xray $TAG already in place"
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for pair in arm64-v8a:arm64-v8a x86_64:amd64; do
    abi=${pair%%:*}
    zip=Xray-android-${pair#*:}.zip
    echo "xray $TAG: $zip"
    curl -fsSL --retry 3 --retry-delay 5 -o "$tmp/$zip" "https://github.com/XTLS/Xray-core/releases/download/$TAG/$zip"
    echo "$(xray_sha256 "$TAG" "$zip")  $tmp/$zip" | sha256sum -c - >/dev/null
    mkdir -p "$tmp/$abi" "$JNI/$abi"
    unzip -q -o "$tmp/$zip" -d "$tmp/$abi"
    install -m 755 "$tmp/$abi/xray" "$JNI/$abi/libxray.so"
done
mkdir -p "$ASSETS"
cp "$tmp/arm64-v8a/geoip.dat" "$tmp/arm64-v8a/geosite.dat" "$ASSETS/"
rm -f "$JNI"/.xray-*
touch "$JNI/.xray-$TAG"
echo "xray $TAG in place"
