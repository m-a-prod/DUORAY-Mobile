#!/bin/sh
# Publishes a DUORAY for Android release to the update hub.
#
#   scripts/release.sh ["Что нового: ..."]
#
# Builds one APK per ABI (arm64-v8a, x86_64), signed with the release key,
# and adds them as the `android` section of the hub's signed manifest, next
# to the desktop builds (../DUORAY/packaging/release.sh keeps the section).
# The manifest is re-signed with the desktop's Ed25519 update key; both keys
# stay on this machine:
#   ~/.config/duoray-release/update-signing.pem   (the desktop's, see its release.sh)
#   ~/.config/duoray-release/android-release.jks  (APK signing; updates must keep it)
# and ~/.config/duoray-release/env sets DUORAY_HUB_SSH, DUORAY_KEYSTORE_PASSWORD.
set -eu
cd "$(dirname "$0")/.."

NOTES=${1:-}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD_ID=$(git rev-parse --short=12 HEAD)
# The build id in the library must be a real commit: updates compare it.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "uncommitted changes: commit first, the release build id is the commit" >&2
    exit 1
fi
CONF=$HOME/.config/duoray-release/env
[ -f "$CONF" ] && . "$CONF"
KEY=${DUORAY_SIGNING_KEY:-$HOME/.config/duoray-release/update-signing.pem}
HUB=${DUORAY_HUB_SSH:?set DUORAY_HUB_SSH (user@host of the update hub) in $CONF}
REMOTE=/var/lib/private/duoray-hub/updates
DUORAY_KEYSTORE=${DUORAY_KEYSTORE:-$HOME/.config/duoray-release/android-release.jks}
export DUORAY_KEYSTORE
[ -f "$KEY" ] || { echo "update signing key not found: $KEY" >&2; exit 1; }
if [ ! -f "$DUORAY_KEYSTORE" ]; then
    cat >&2 <<EOF
APK signing key not found: $DUORAY_KEYSTORE
Create it once and keep a copy: phones accept updates only with the same key.
  keytool -genkeypair -keystore $DUORAY_KEYSTORE -alias duoray -keyalg RSA -keysize 4096 -validity 36500 -dname CN=DUORAY
and put DUORAY_KEYSTORE_PASSWORD=... into $CONF.
EOF
    exit 1
fi
: "${DUORAY_KEYSTORE_PASSWORD:?set DUORAY_KEYSTORE_PASSWORD in $CONF}"
export DUORAY_KEYSTORE_PASSWORD

# versionCode grows with the version: 1.2.3 -> 10203.
CODE=$(echo "$VERSION" | awk -F'[.-]' '{ print $1 * 10000 + $2 * 100 + $3 }')
GRADLE_ARGS="-PversionName=$VERSION -PversionCode=$CODE -PabiSplit=true" scripts/build-apk.sh release

OUT=dist/release-$VERSION
rm -rf "$OUT" && mkdir -p "$OUT"
for abi in arm64-v8a x86_64; do
    cp "android/app/build/outputs/apk/release/app-$abi-release.apk" "$OUT/DUORAY-$VERSION-$abi.apk"
done

# The manifest as the hub serves it now, checked before it is signed again.
openssl pkey -in "$KEY" -pubout > "$OUT/.pub.pem"
ssh "$HUB" "cat $REMOTE/manifest.json" > "$OUT/current.json"
ssh "$HUB" "cat $REMOTE/manifest.sig" | xxd -r -p > "$OUT/.current.sig"
openssl pkeyutl -verify -pubin -inkey "$OUT/.pub.pem" -rawin -in "$OUT/current.json" -sigfile "$OUT/.current.sig" >/dev/null || {
    echo "the hub's manifest does not match its signature: not touching it" >&2; exit 1; }

python3 - "$VERSION" "$NOTES" "$OUT" "$BUILD_ID" <<'EOF'
import hashlib, json, os, sys, time
version, notes, out, build = sys.argv[1:5]
with open(os.path.join(out, "current.json"), encoding="utf-8") as f:
    manifest = json.load(f)
assets = {}
for abi in ("arm64-v8a", "x86_64"):
    name = f"DUORAY-{version}-{abi}.apk"
    path = os.path.join(out, name)
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    assets[f"android-{abi}-apk"] = {"file": name, "sha256": h.hexdigest(), "size": os.path.getsize(path)}
    print(f"  android-{abi:<12} {name}  {os.path.getsize(path) // 1024 // 1024} MB")
manifest["android"] = {
    "version": version,
    "build": build,
    "notes": notes,
    "published": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "assets": assets,
}
with open(os.path.join(out, "manifest.json"), "w", encoding="utf-8") as f:
    json.dump(manifest, f, ensure_ascii=False, indent=2)
os.remove(os.path.join(out, "current.json"))
EOF

openssl pkeyutl -sign -inkey "$KEY" -rawin -in "$OUT/manifest.json" | xxd -p | tr -d '\n' > "$OUT/manifest.sig"
xxd -r -p "$OUT/manifest.sig" > "$OUT/.sig.bin"
openssl pkeyutl -verify -pubin -inkey "$OUT/.pub.pem" -rawin -in "$OUT/manifest.json" -sigfile "$OUT/.sig.bin" >/dev/null
rm -f "$OUT/.sig.bin" "$OUT/.pub.pem" "$OUT/.current.sig"

# Files first, the manifest last: clients never see a manifest whose files
# are still uploading.
ssh "$HUB" "mkdir -p $REMOTE/files"
for f in "$OUT"/DUORAY-*.apk; do
    scp -q "$f" "$HUB:$REMOTE/files/"
done
scp -q "$OUT/manifest.json" "$HUB:$REMOTE/manifest.json.new"
scp -q "$OUT/manifest.sig" "$HUB:$REMOTE/manifest.sig.new"
ssh "$HUB" "cd $REMOTE && chmod -R a+rX files; chmod 644 manifest.*.new && mv manifest.sig.new manifest.sig && mv manifest.json.new manifest.json"
echo "published Android $VERSION ($BUILD_ID)"
