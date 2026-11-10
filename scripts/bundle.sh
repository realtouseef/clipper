#!/bin/bash
# Builds a shareable Clipper.dmg (and Clipper.app) into dist/.
#
#   scripts/bundle.sh            # build, bundle ffmpeg, sign, make the .dmg
#   scripts/bundle.sh --gpu      # speech to text on the GPU (Metal)
#
# Signing (optional, for a .dmg that opens without warnings on other Macs):
#   CLIPPER_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
#   CLIPPER_NOTARY_PROFILE=clipper   # from: xcrun notarytool store-credentials clipper
# Without them the app is signed ad hoc: it works, but people have to allow it once in
# System Settings → Privacy & Security ("Open Anyway").
#
# The app runs on Apple Silicon Macs with macOS 15 or later.
set -euo pipefail
cd "$(dirname "$0")/.."

FEATURES=()
[[ "${1:-}" == "--gpu" ]] && FEATURES=(--features gpu)
IDENTITY="${CLIPPER_SIGN_IDENTITY:-}"
NOTARY="${CLIPPER_NOTARY_PROFILE:-}"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=dist/Clipper.app
DMG=dist/Clipper-$VERSION.dmg

echo "Building Clipper $VERSION (release)..."
cargo build --release ${FEATURES[@]+"${FEATURES[@]}"}
# ffmpeg + ffprobe that ship inside the app (pinned, checksum-verified download).
scripts/fetch-ffmpeg.sh

rm -rf "$APP" dist/*.dmg dist/dmg
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/clipper "$APP/Contents/MacOS/Clipper"

# Helpers next to the executable (Clipper looks there first): the ScreenCaptureKit programs
# and ffmpeg/ffprobe.
HELPERS=$(dirname "$(ls -t target/release/build/clipper-*/out/clipper-shot | head -1)")
cp "$HELPERS/clipper-capture" "$HELPERS/clipper-shot" vendor/ffmpeg/ffmpeg vendor/ffmpeg/ffprobe "$APP/Contents/MacOS/"
cp assets/licenses/FFmpeg.txt "$APP/Contents/Resources/FFmpeg-LICENSE.txt"

# App icon from the mascot.
ICONSET=$(mktemp -d)/Clipper.iconset
"$APP/Contents/MacOS/Clipper" --write-iconset "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Clipper</string>
  <key>CFBundleDisplayName</key><string>Clipper</string>
  <key>CFBundleIdentifier</key><string>app.clipper.Clipper</string>
  <key>CFBundleExecutable</key><string>Clipper</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Clipper records your microphone with your screen recordings.</string>
  <key>NSCameraUsageDescription</key><string>Clipper records your webcam as its own track in screen recordings.</string>
</dict>
</plist>
PLIST

# Sign the helpers first, then the app. A Developer ID signature uses the hardened runtime
# (required for notarization) with the camera and microphone entitlements.
NESTED=("$APP/Contents/MacOS/clipper-capture" "$APP/Contents/MacOS/clipper-shot" "$APP/Contents/MacOS/ffmpeg" "$APP/Contents/MacOS/ffprobe")
if [[ -n "$IDENTITY" ]]; then
  echo "Signing with ${IDENTITY}..."
  codesign --force --timestamp --options runtime --sign "$IDENTITY" "${NESTED[@]}"
  codesign --force --timestamp --options runtime --entitlements scripts/Clipper.entitlements --sign "$IDENTITY" "$APP"
else
  echo "No CLIPPER_SIGN_IDENTITY: signing ad hoc."
  codesign --force --sign - "${NESTED[@]}"
  codesign --force --sign - "$APP"
fi
codesign --verify --deep --strict "$APP"

# The disk image: the app and an Applications shortcut, for drag-to-install.
mkdir -p dist/dmg
cp -R "$APP" dist/dmg/
ln -s /Applications dist/dmg/Applications
hdiutil create -quiet -volname "Clipper $VERSION" -srcfolder dist/dmg -ov -format UDZO "$DMG"
rm -rf dist/dmg
if [[ -n "$IDENTITY" ]]; then
  codesign --force --timestamp --sign "$IDENTITY" "$DMG"
fi

# Notarize and staple, so Gatekeeper accepts the download without a warning.
if [[ -n "$IDENTITY" && -n "$NOTARY" ]]; then
  echo "Notarizing (a few minutes)..."
  xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY" --wait
  xcrun stapler staple "$DMG"
  spctl -a -t open --context context:primary-signature -v "$DMG"
fi

echo
echo "Built $DMG ($(du -h "$DMG" | cut -f1))"
if [[ -z "$IDENTITY" || -z "$NOTARY" ]]; then
  echo "Not notarized: people who download it allow it once in System Settings → Privacy & Security → Open Anyway."
fi
