#!/bin/bash
# Builds a distributable ClipVault.app: universal binary, signed, zipped, and
# optionally notarized.
#
#   ./scripts/release.sh                       # universal + best available signature + zip
#   ./scripts/release.sh --notarize <profile>  # …then notarize and staple
#
# The notarize step needs a Developer ID Application certificate, which requires
# a paid Apple Developer Program membership. An "Apple Development" certificate
# is NOT sufficient — Apple rejects it for distribution.
#
# One-time setup for --notarize (an app-specific password comes from
# appleid.apple.com, not your Apple ID password):
#
#   xcrun notarytool store-credentials clipvault \
#       --apple-id you@example.com --team-id ABCDE12345 --password xxxx-xxxx-xxxx-xxxx
set -euo pipefail

cd "$(dirname "$0")/.."

APP="dist/ClipVault.app"
ZIP="dist/ClipVault.zip"
NOTARIZE_PROFILE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --notarize) NOTARIZE_PROFILE="${2:?--notarize needs a keychain profile name}"; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
done

# Pick the strongest identity available rather than guessing one.
IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep "Developer ID Application" | head -1 | sed -E 's/.*"(.*)"/\1/' || true)"

# Build unsigned; this script does the signing so it can apply the hardened
# runtime, which notarization requires and bundle.sh's ad-hoc pass does not.
echo "==> building universal bundle"
CLIPVAULT_UNIVERSAL=1 CLIPVAULT_SKIP_SIGN=1 ./scripts/bundle.sh >/dev/null
echo "    $(lipo -info "$APP/Contents/MacOS/clipvault-menubar" | sed 's/.*are: //')"

if [ -n "$IDENTITY" ]; then
    echo "==> signing with: $IDENTITY"
    SIGN_ARGS=(--force --options runtime --timestamp --sign "$IDENTITY")
else
    echo "==> no Developer ID certificate found; falling back to an ad-hoc signature"
    echo "    (runs locally, but recipients will hit Gatekeeper — see README)"
    SIGN_ARGS=(--force --sign -)
fi

# Inside-out: nested executables before the bundle that contains them.
codesign "${SIGN_ARGS[@]}" "$APP/Contents/MacOS/clipvault"
codesign "${SIGN_ARGS[@]}" "$APP/Contents/MacOS/clipvault-menubar"
codesign "${SIGN_ARGS[@]}" "$APP"
codesign --verify --strict --verbose=2 "$APP"

# ditto, not zip: it preserves the bundle structure and extended attributes that
# a plain zip flattens.
echo "==> packaging"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

if [ -n "$NOTARIZE_PROFILE" ]; then
    if [ -z "$IDENTITY" ]; then
        echo "cannot notarize an ad-hoc signed app — a Developer ID certificate is required" >&2
        exit 1
    fi
    echo "==> notarizing (a few minutes)"
    xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARIZE_PROFILE" --wait

    # Staple the ticket into the app so it opens offline, then rezip: the zip
    # made above predates the ticket.
    echo "==> stapling"
    xcrun stapler staple "$APP"
    rm -f "$ZIP"
    ditto -c -k --keepParent "$APP" "$ZIP"
    spctl --assess --type execute --verbose=2 "$APP"
fi

echo
echo "built $ZIP ($(du -h "$ZIP" | cut -f1))"
if [ -z "$NOTARIZE_PROFILE" ]; then
    echo "not notarized — recipients must approve it in System Settings › Privacy & Security"
fi
