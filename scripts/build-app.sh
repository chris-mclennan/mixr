#!/usr/bin/env bash
#
# Build mixr.app — a hand-rolled macOS app bundle.
#
#   ./scripts/build-app.sh          # debug profile, builds target/mixr.app
#   ./scripts/build-app.sh release  # release profile
#
# Launch with:  open target/mixr.app
#
# Bundle layout:
#   target/mixr.app/Contents/
#     Info.plist
#     MacOS/mixr-launcher       (small dispatch script — Contents/Resources/bin/mixr)
#     Resources/AppIcon.icns
#     Resources/bin/mixr        (the actual TUI binary)
#
# Launcher dispatch: if `tmnl` is on PATH the launcher opens mixr as
# a native tmnl tab; otherwise it falls back to Terminal.app.

set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="${1:-debug}"
case "$PROFILE" in
    debug)
        cargo build --bin mixr
        ;;
    release)
        cargo build --release --bin mixr
        ;;
    *)
        echo "usage: $0 [debug|release]" >&2
        exit 2
        ;;
esac

APP="target/mixr.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"
cp scripts/launcher.sh "$APP/Contents/MacOS/mixr-launcher"
chmod +x "$APP/Contents/MacOS/mixr-launcher"
cp "target/$PROFILE/mixr" "$APP/Contents/Resources/bin/mixr"
cp scripts/Info.plist "$APP/Contents/Info.plist"

# App icon — built on demand if missing (no external image-tool deps;
# scripts/icon/gen_icon.swift draws from scratch in AppKit).
if [ ! -f scripts/icon/AppIcon.icns ]; then
    echo "building app icon…"
    (cd scripts/icon && ./build.sh) >/dev/null
fi
cp scripts/icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

# Strip the quarantine bit so Finder doesn't Gatekeeper-block the
# first launch. Best-effort.
xattr -d com.apple.quarantine "$APP" 2>/dev/null || true

echo "built $APP"
echo "launch: open $APP"
