#!/usr/bin/env bash
#
# Build a macOS .app bundle for the TagTiger GUI and wrap it in a .dmg whose
# mounted volume shows the application icon.
#
# Usage:
#   packaging/macos_bundle.sh <target-triple> <output-dir>
#
# Example:
#   packaging/macos_bundle.sh aarch64-apple-darwin dist
#
# Produces:
#   <output-dir>/TagTiger.app          (bundle, also archived by CI)
#   <output-dir>/TagTiger-<target>.dmg (disk image with a volume icon)
#
# Requires the release binaries to already be built at
# target/<target>/release/{tagtiger,tagtiger-gui}.

set -euo pipefail

TARGET="${1:?usage: macos_bundle.sh <target-triple> <output-dir>}"
OUT_DIR="${2:?usage: macos_bundle.sh <target-triple> <output-dir>}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_DIR="$REPO_ROOT/target/$TARGET/release"
ICNS="$REPO_ROOT/gui/src/resources/app_icon.icns"

APP_NAME="TagTiger"
BUNDLE="$OUT_DIR/$APP_NAME.app"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
VERSION="${VERSION:-0.0.0}"

echo "==> Building $APP_NAME.app ($TARGET, v$VERSION)"

# ---- Assemble the .app bundle ------------------------------------------------
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"

# GUI binary is the bundle executable; include the CLI alongside for convenience.
cp "$RELEASE_DIR/tagtiger-gui" "$BUNDLE/Contents/MacOS/$APP_NAME"
if [[ -f "$RELEASE_DIR/tagtiger" ]]; then
  cp "$RELEASE_DIR/tagtiger" "$BUNDLE/Contents/MacOS/tagtiger"
fi
chmod +x "$BUNDLE/Contents/MacOS/$APP_NAME"

cp "$ICNS" "$BUNDLE/Contents/Resources/app_icon.icns"

cat >"$BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>     <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>      <string>com.tagtiger.gui</string>
    <key>CFBundleVersion</key>         <string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleExecutable</key>      <string>$APP_NAME</string>
    <key>CFBundleIconFile</key>        <string>app_icon</string>
    <key>LSMinimumSystemVersion</key>  <string>10.13</string>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

echo "==> Bundle assembled at $BUNDLE"

# ---- Optionally codesign the bundle -----------------------------------------
# When TAGTIGER_SIGN_IDENTITY is set (CI with a Developer ID cert imported into
# the keychain), sign the nested executables and the .app with the hardened
# runtime and our entitlements. Signing the DMG and notarizing happen in the
# workflow after this script returns. When the var is unset (local builds), the
# bundle is left unsigned.
ENTITLEMENTS="$REPO_ROOT/packaging/entitlements.plist"
# Resolve the signing identity. Prefer a full identity in TAGTIGER_SIGN_IDENTITY;
# otherwise build one from TAGTIGER_SIGN_IDENTITY_NAME (the bare team/name), to
# keep the CI YAML free of the colon in "Developer ID Application:".
SIGN_IDENTITY="${TAGTIGER_SIGN_IDENTITY:-}"
if [[ -z "$SIGN_IDENTITY" && -n "${TAGTIGER_SIGN_IDENTITY_NAME:-}" ]]; then
  SIGN_IDENTITY="Developer ID Application: ${TAGTIGER_SIGN_IDENTITY_NAME}"
fi
if [[ -n "$SIGN_IDENTITY" ]]; then
  echo "==> Codesigning bundle as: $SIGN_IDENTITY"
  KEYCHAIN_ARGS=()
  if [[ -n "${TAGTIGER_KEYCHAIN:-}" ]]; then
    KEYCHAIN_ARGS=(--keychain "$TAGTIGER_KEYCHAIN")
  fi
  # Sign the nested CLI binary first (inner-to-outer signing order).
  if [[ -f "$BUNDLE/Contents/MacOS/tagtiger" ]]; then
    codesign --force --options runtime --timestamp \
      --entitlements "$ENTITLEMENTS" \
      --sign "$SIGN_IDENTITY" "${KEYCHAIN_ARGS[@]}" \
      "$BUNDLE/Contents/MacOS/tagtiger"
  fi
  # Sign the main executable, then the bundle as a whole.
  codesign --force --options runtime --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGN_IDENTITY" "${KEYCHAIN_ARGS[@]}" \
    "$BUNDLE/Contents/MacOS/$APP_NAME"
  codesign --force --options runtime --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGN_IDENTITY" "${KEYCHAIN_ARGS[@]}" \
    "$BUNDLE"
  # Verify before we wrap it in a DMG.
  codesign --verify --deep --strict --verbose=2 "$BUNDLE"
  echo "==> Bundle signed and verified"
fi

# ---- Build a .dmg with a volume icon ----------------------------------------
# Approach: create a writable DMG from the staged contents, mount it, drop the
# volume icon at the root and set the volume's custom-icon attribute, then
# convert to a compressed read-only image. Setting the attribute on the mounted
# HFS volume root is what makes Finder show the icon for the mounted disk.
DMG="$OUT_DIR/$APP_NAME-$TARGET.dmg"
DMG_RW="$(mktemp -u).dmg"
STAGING="$(mktemp -d)"
cleanup() {
  [[ -n "${MOUNT_DIR:-}" && -d "${MOUNT_DIR:-}" ]] && hdiutil detach "$MOUNT_DIR" >/dev/null 2>&1 || true
  rm -rf "$STAGING" "$DMG_RW"
}
trap cleanup EXIT

# Lay out what the mounted volume will contain: the app + an Applications link.
cp -R "$BUNDLE" "$STAGING/$APP_NAME.app"
ln -s /Applications "$STAGING/Applications"
cp "$ICNS" "$STAGING/.VolumeIcon.icns"

echo "==> Creating writable image"
hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$STAGING" \
  -fs HFS+ \
  -format UDRW \
  -ov \
  "$DMG_RW"

echo "==> Setting volume icon attribute"
MOUNT_DIR="$(mktemp -d)"
hdiutil attach "$DMG_RW" -nobrowse -mountpoint "$MOUNT_DIR" >/dev/null
if command -v SetFile >/dev/null 2>&1; then
  SetFile -a C "$MOUNT_DIR"                    # volume has a custom icon
  SetFile -a V "$MOUNT_DIR/.VolumeIcon.icns"   # keep the icns hidden
fi
sync
hdiutil detach "$MOUNT_DIR" >/dev/null
MOUNT_DIR=""

echo "==> Converting to compressed image: $DMG"
rm -f "$DMG"
hdiutil convert "$DMG_RW" -format UDZO -o "$DMG" >/dev/null

echo "==> Done: $DMG"
