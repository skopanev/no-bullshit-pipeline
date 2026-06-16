#!/bin/bash
set -e

# Notarization credentials are secrets — never hardcode them. They live in
# an untracked .env.release file (gitignored) or the environment. Copy
# .env.release.example → .env.release and fill in your own values.
if [ -f .env.release ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env.release
  set +a
fi

SIGN_ID="Developer ID Application: Sergei Sharov (Z499WGKJW6)"
APPLE_ID="${APPLE_ID:?set APPLE_ID in .env.release or the environment}"
APPLE_PASSWORD="${APPLE_PASSWORD:?set APPLE_PASSWORD (app-specific password) in .env.release or the environment}"
APPLE_TEAM_ID="${APPLE_TEAM_ID:-Z499WGKJW6}"

NAME=$(grep '"productName":' src-tauri/tauri.conf.json | cut -d'"' -f4)
VERSION=$(grep '"version":' src-tauri/tauri.conf.json | cut -d'"' -f4)

echo "==> Building sidecars (release)..."
bun run build:sidecar:force

echo "==> Signing sidecar binaries..."
for SIDECAR in \
  "src-tauri/binaries/fluidaudio-sidecar-aarch64-apple-darwin" \
  "src-tauri/binaries/apple-speech-sidecar-aarch64-apple-darwin"
do
  if [ -f "$SIDECAR" ]; then
    codesign --force --options runtime --sign "$SIGN_ID" \
      --entitlements src-tauri/entitlements.plist "$SIDECAR"
    echo "    Signed: $SIDECAR"
  fi
done

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

# dmgbuild, not create-dmg: create-dmg drives Finder via AppleScript to set the
# window layout, which on recent macOS intermittently fails to apply the icon
# size + background before the volume unmounts (→ tiny icons, white background —
# the "waited 1 seconds for .DS_STORE" path). dmgbuild writes the .DS_Store
# directly, so the layout is deterministic. Self-bootstraps a venv so a fresh
# checkout just works; layout lives in src-tauri/dmg-assets/dmg-settings.py.
if [ ! -x .venv-build/bin/dmgbuild ]; then
  echo "    Bootstrapping dmgbuild venv..."
  python3 -m venv .venv-build
  .venv-build/bin/pip install --quiet dmgbuild
fi
.venv-build/bin/dmgbuild \
  -s src-tauri/dmg-assets/dmg-settings.py \
  -D app="$APP_PATH" \
  "$NAME" \
  "$DMG_PATH"
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
