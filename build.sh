#!/bin/bash
set -e

SIGN_ID="Developer ID Application: Sergei Sharov (Z499WGKJW6)"
APPLE_ID="${APPLE_ID:-thesergesha@gmail.com}"
APPLE_PASSWORD="${APPLE_PASSWORD:-zgrr-vsxc-ahhk-oaqh}"
APPLE_TEAM_ID="Z499WGKJW6"

NAME=$(grep '"productName":' src-tauri/tauri.conf.json | cut -d'"' -f4)
VERSION=$(grep '"version":' src-tauri/tauri.conf.json | cut -d'"' -f4)

echo "==> Signing sidecar binary..."
SIDECAR="src-tauri/binaries/fluidaudio-sidecar-aarch64-apple-darwin"
if [ -f "$SIDECAR" ]; then
  codesign --force --options runtime --sign "$SIGN_ID" \
    --entitlements src-tauri/entitlements.plist "$SIDECAR"
  echo "    Sidecar signed."
fi

echo "==> Bundling JS..."
bun esbuild.mjs

echo "==> Building app (no DMG)..."
bun tauri build --bundles app

BUNDLE_DIR="src-tauri/target/release/bundle/macos"
APP_PATH="$BUNDLE_DIR/$NAME.app"

if [ ! -d "$APP_PATH" ]; then
  echo "ERROR: $APP_PATH not found"
  exit 1
fi

echo "==> Verifying signature..."
codesign -dvvv "$APP_PATH" 2>&1 | grep -E "(Authority|Identifier|TeamIdentifier)"

echo "==> Creating DMG..."
mkdir -p builds
DMG_PATH="builds/${NAME}_v${VERSION}.dmg"

# Detach any mounted DMG for this volume and remove old file
hdiutil detach "/Volumes/$NAME" >/dev/null 2>&1 || true
rm -f "$DMG_PATH"

create-dmg \
  --volname "$NAME" \
  --background "src-tauri/dmg-assets/background.png" \
  --window-pos 200 120 \
  --window-size 820 520 \
  --icon-size 160 \
  --text-size 14 \
  --icon "$NAME.app" 220 230 \
  --app-drop-link 600 230 \
  --hide-extension "$NAME.app" \
  --no-internet-enable \
  "$DMG_PATH" \
  "$APP_PATH"
echo "    DMG created: $DMG_PATH"

echo "==> Notarizing DMG..."
xcrun notarytool submit \
  --apple-id "$APPLE_ID" \
  --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" \
  --wait \
  "$DMG_PATH"

echo "==> Stapling..."
xcrun stapler staple "$DMG_PATH"

echo "==> Gatekeeper check..."
spctl -a -vvv "$APP_PATH" 2>&1 || true

echo "==> Uploading to GitHub Releases..."
TAG="v${VERSION}"
gh release create "$TAG" "$DMG_PATH" \
  --title "$NAME $TAG" \
  --notes "Release $TAG" \
  --latest \
  2>/dev/null \
|| gh release upload "$TAG" "$DMG_PATH" --clobber

RELEASE_URL=$(gh release view "$TAG" --json url -q .url)

echo ""
echo "=================================================="
echo "  BUILD COMPLETE: $DMG_PATH"
echo "  GitHub Release: $RELEASE_URL"
echo "=================================================="
