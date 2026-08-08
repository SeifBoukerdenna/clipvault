#!/bin/bash
# Builds ClipVault.app — the menu bar app — into dist/.
# Pass --install to also copy it into /Applications.
set -euo pipefail

cd "$(dirname "$0")/.."

APP="dist/ClipVault.app"
BIN="clipvault-menubar"

# Version comes from Cargo.toml so the two can't drift.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUILD="${CLIPVAULT_BUILD:-1}"

# CLIPVAULT_UNIVERSAL=1 produces one binary that runs on both Apple Silicon and
# Intel. Off by default because building twice is slower and a local install
# only needs the host arch.
if [ "${CLIPVAULT_UNIVERSAL:-0}" = "1" ]; then
    echo "==> building release binaries (universal)"
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        if ! rustup target list --installed | grep -q "^$target$"; then
            echo "missing rust target $target — run: rustup target add $target" >&2
            exit 1
        fi
        cargo build --release --target "$target"
    done
    BIN_DIR="target/universal"
    mkdir -p "$BIN_DIR"
    for exe in clipvault clipvault-menubar; do
        lipo -create -output "$BIN_DIR/$exe" \
            "target/aarch64-apple-darwin/release/$exe" \
            "target/x86_64-apple-darwin/release/$exe"
    done
else
    echo "==> building release binaries (host arch only)"
    cargo build --release
    BIN_DIR="target/release"
fi

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN_DIR/$BIN" "$APP/Contents/MacOS/$BIN"
# The CLI rides along so `list`/`search` are available inside the bundle too.
cp "$BIN_DIR/clipvault" "$APP/Contents/MacOS/clipvault"

# Regenerate the icon only if it's missing, so a normal build stays fast.
if [ ! -f assets/ClipVault.icns ]; then
    ./scripts/make-icon.sh
fi
cp assets/ClipVault.icns "$APP/Contents/Resources/ClipVault.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>                 <string>ClipVault</string>
    <key>CFBundleDisplayName</key>          <string>ClipVault</string>
    <key>CFBundleIdentifier</key>           <string>com.seifboukerdenna.clipvault</string>
    <key>CFBundleExecutable</key>           <string>clipvault-menubar</string>
    <key>CFBundleIconFile</key>             <string>ClipVault</string>
    <key>CFBundlePackageType</key>          <string>APPL</string>
    <key>LSApplicationCategoryType</key>     <string>public.app-category.utilities</string>
    <key>CFBundleShortVersionString</key>   <string>__VERSION__</string>
    <key>CFBundleVersion</key>              <string>__BUILD__</string>
    <key>LSMinimumSystemVersion</key>       <string>11.0</string>
    <!-- Menu bar only: no Dock icon, no app switcher entry. -->
    <key>LSUIElement</key>                  <true/>
    <key>NSHighResolutionCapable</key>      <true/>
</dict>
</plist>
PLIST

# The heredoc is quoted so the plist stays literal; substitute afterwards.
/usr/bin/sed -i '' "s/__VERSION__/$VERSION/; s/__BUILD__/$BUILD/" "$APP/Contents/Info.plist"

# Strip extended attributes before signing. Anything that touched a browser or
# an archive carries com.apple.quarantine, and signing over it bakes the
# attribute into the bundle recipients download.
xattr -cr "$APP"

# Ad-hoc signature. Fine for running on this machine; scripts/release.sh
# re-signs with a Developer ID when one is available.
# Signing runs inside-out: nested executables first, then the bundle. Signing
# the bundle alone fails with "code object is not signed at all" on the extra
# CLI binary, and an unsigned arm64 binary won't execute at all.
if [ "${CLIPVAULT_SKIP_SIGN:-0}" != "1" ]; then
    echo "==> ad-hoc signing"
    codesign --force --sign - "$APP/Contents/MacOS/clipvault"
    codesign --force --sign - "$APP/Contents/MacOS/$BIN"
    codesign --force --sign - "$APP"
    codesign --verify --strict "$APP"
fi

echo
echo "built $APP"
if [ "${1:-}" = "--install" ]; then
    echo "==> installing to /Applications"
    rm -rf "/Applications/ClipVault.app"
    cp -R "$APP" /Applications/
    echo "installed /Applications/ClipVault.app"
fi
