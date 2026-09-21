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

# GUI binary is the bundle executable. Ship the CLI alongside it, but under a
# name that does NOT collide with "TagTiger" case-insensitively — macOS volumes
# are case-insensitive by default, so a file named "tagtiger" would overwrite
# "TagTiger". Use "tagtiger-cli"; the DMG installer exposes it as `tagtiger`.
cp "$RELEASE_DIR/tagtiger-gui" "$BUNDLE/Contents/MacOS/$APP_NAME"
if [[ -f "$RELEASE_DIR/tagtiger" ]]; then
  cp "$RELEASE_DIR/tagtiger" "$BUNDLE/Contents/MacOS/tagtiger-cli"
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

# Resolve the signing identity:
#   1. TAGTIGER_SIGN_IDENTITY  — a full identity string, used as-is.
#   2. TAGTIGER_SIGN_IDENTITY_NAME — the name after "Developer ID Application: ".
#   3. Auto-detect the first "Developer ID Application" identity in the keychain.
# Auto-detection avoids depending on an exactly-matching secret string.
ENTITLEMENTS="$REPO_ROOT/packaging/entitlements.plist"
SIGN_IDENTITY="${TAGTIGER_SIGN_IDENTITY:-}"
if [[ -z "$SIGN_IDENTITY" && -n "${TAGTIGER_SIGN_IDENTITY_NAME:-}" ]]; then
  # Accept either a bare name or a full "Developer ID Application: ..." value.
  _name="${TAGTIGER_SIGN_IDENTITY_NAME#Developer ID Application: }"
  SIGN_IDENTITY="Developer ID Application: ${_name}"
fi
KEYCHAIN_ARGS=()
if [[ -n "${TAGTIGER_KEYCHAIN:-}" ]]; then
  KEYCHAIN_ARGS=(--keychain "$TAGTIGER_KEYCHAIN")
fi
if [[ -z "$SIGN_IDENTITY" && -n "${TAGTIGER_KEYCHAIN:-}" ]]; then
  # Pull the exact identity name from the keychain (robust to typos in secrets).
  AUTO_ID="$(security find-identity -v -p codesigning "$TAGTIGER_KEYCHAIN" 2>/dev/null \
    | grep -o '"Developer ID Application:[^"]*"' | head -1 | tr -d '"' || true)"
  if [[ -n "$AUTO_ID" ]]; then
    SIGN_IDENTITY="$AUTO_ID"
  fi
fi

# Diagnostics: show what codesigning identities are visible.
if [[ -n "${TAGTIGER_KEYCHAIN:-}" ]]; then
  echo "==> Available codesigning identities in $TAGTIGER_KEYCHAIN:"
  security find-identity -v -p codesigning "$TAGTIGER_KEYCHAIN" || true
fi
echo "==> Resolved signing identity: '${SIGN_IDENTITY:-<none>}'"

# Sign the .app in place. Inner code (the nested CLI) is signed first, then the
# main executable, then the bundle itself — hardened runtime + secure timestamp
# + entitlements throughout. Verified before use. When no identity is resolved
# (local builds) the bundle is left unsigned.
sign_app() {
  local app="$1"
  [[ -n "$SIGN_IDENTITY" ]] || { echo "==> No identity; leaving $app unsigned"; return 0; }
  echo "==> Codesigning $app as: $SIGN_IDENTITY"
  # Expand KEYCHAIN_ARGS safely even when empty (bash 3.2 + set -u).
  local kc=(${KEYCHAIN_ARGS[@]+"${KEYCHAIN_ARGS[@]}"})
  if [[ -f "$app/Contents/MacOS/tagtiger-cli" ]]; then
    codesign --force --options runtime --timestamp \
      --entitlements "$ENTITLEMENTS" \
      --sign "$SIGN_IDENTITY" ${kc[@]+"${kc[@]}"} \
      "$app/Contents/MacOS/tagtiger-cli"
  fi
  codesign --force --options runtime --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGN_IDENTITY" ${kc[@]+"${kc[@]}"} \
    "$app/Contents/MacOS/$APP_NAME"
  codesign --force --options runtime --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGN_IDENTITY" ${kc[@]+"${kc[@]}"} \
    "$app"
  # Strict verification, and confirm the main executable carries the hardened
  # runtime + a secure timestamp (the exact things notarization checks).
  codesign --verify --deep --strict --verbose=2 "$app"
  echo "==> Signature details for the main executable:"
  codesign --display --verbose=4 "$app/Contents/MacOS/$APP_NAME" 2>&1 \
    | grep -Ei 'flags|Timestamp|Authority' || true
  echo "==> $app signed and verified"
}

# Sign the bundle that ships in the .tar.gz.
sign_app "$BUNDLE"

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
# Use `ditto` (not `cp -R`), which preserves code-signature metadata and
# extended attributes — a plain copy corrupts the signature and fails
# notarization. Re-sign the staged copy afterwards to guarantee the sealed
# signature matches the exact bytes placed in the image.
ditto "$BUNDLE" "$STAGING/$APP_NAME.app"
sign_app "$STAGING/$APP_NAME.app"
ln -s /Applications "$STAGING/Applications"
cp "$ICNS" "$STAGING/.VolumeIcon.icns"

# CLI installer: a double-clickable script that symlinks the `tagtiger` CLI
# (which ships inside the notarized app at Contents/MacOS/tagtiger-cli) onto the
# user's PATH at /usr/local/bin/tagtiger. Because the symlink target lives
# inside the notarized .app, the CLI runs without Gatekeeper quarantine issues.
INSTALLER="$STAGING/Install tagtiger CLI.command"
cat >"$INSTALLER" <<'CMD'
#!/bin/bash
# Symlink the TagTiger CLI onto your PATH. Run this after copying TagTiger.app
# to your Applications folder.
set -e
APP="/Applications/TagTiger.app/Contents/MacOS/tagtiger-cli"
DEST="/usr/local/bin/tagtiger"

if [ ! -x "$APP" ]; then
  echo "TagTiger.app was not found in /Applications."
  echo "Drag TagTiger.app to Applications first, then run this again."
  read -n 1 -s -r -p "Press any key to close..."
  exit 1
fi

echo "Linking $DEST -> $APP"
if [ -w "/usr/local/bin" ] || mkdir -p /usr/local/bin 2>/dev/null; then
  ln -sf "$APP" "$DEST"
else
  echo "Administrator access is required to write to /usr/local/bin."
  sudo mkdir -p /usr/local/bin
  sudo ln -sf "$APP" "$DEST"
fi

echo "Done. You can now run: tagtiger --help"
read -n 1 -s -r -p "Press any key to close..."
CMD
chmod +x "$INSTALLER"
# Sign the installer script too when a signing identity is available (loose
# scripts are otherwise unsigned; signing keeps Gatekeeper happy).
if [[ -n "$SIGN_IDENTITY" ]]; then
  installer_kc=(${KEYCHAIN_ARGS[@]+"${KEYCHAIN_ARGS[@]}"})
  codesign --force --timestamp --sign "$SIGN_IDENTITY" \
    ${installer_kc[@]+"${installer_kc[@]}"} "$INSTALLER" || true
fi

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

# Safety net: verify the app signature inside the *finished* compressed image.
# Fail the build here (with diagnostics) rather than at notarization if the
# signature didn't survive being placed into the DMG.
if [[ -n "$SIGN_IDENTITY" ]]; then
  echo "==> Verifying signature inside the finished DMG"
  VERIFY_MNT="$(mktemp -d)"
  hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$VERIFY_MNT" >/dev/null
  if codesign --verify --deep --strict --verbose=2 "$VERIFY_MNT/$APP_NAME.app"; then
    echo "==> DMG app signature verified"
  else
    echo "ERROR: app signature invalid inside the finished DMG" >&2
    codesign --display --verbose=4 "$VERIFY_MNT/$APP_NAME.app/Contents/MacOS/$APP_NAME" 2>&1 || true
    hdiutil detach "$VERIFY_MNT" >/dev/null 2>&1 || true
    rmdir "$VERIFY_MNT" 2>/dev/null || true
    exit 1
  fi
  hdiutil detach "$VERIFY_MNT" >/dev/null
  rmdir "$VERIFY_MNT" 2>/dev/null || true
fi

echo "==> Done: $DMG"
