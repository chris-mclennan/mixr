#!/bin/bash
# mixr-launcher — the executable inside mixr.app.
#
# Opens mixr in Ghostty when available, else falls back to
# Terminal.app.

set -eu

bundle_root="$(cd "$(dirname "$0")/../.." && pwd)"
mixr_bin="$bundle_root/Contents/Resources/bin/mixr"

# Finder/LaunchServices strips $PATH down to a system minimum.
export PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin"

# Prefer Ghostty (better Nerd Font rendering, native macOS feel),
# fall back to Terminal.app.
ghostty_bin=""
if command -v ghostty >/dev/null 2>&1; then
    ghostty_bin="$(command -v ghostty)"
elif [ -x "/Applications/Ghostty.app/Contents/MacOS/ghostty" ]; then
    ghostty_bin="/Applications/Ghostty.app/Contents/MacOS/ghostty"
fi
if [ -n "$ghostty_bin" ]; then
    exec "$ghostty_bin" -e "$mixr_bin"
fi

osascript <<EOF
tell application "Terminal"
    activate
    do script "exec '$mixr_bin'"
end tell
EOF
