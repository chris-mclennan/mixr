#!/bin/bash
# mixr-launcher — the executable inside mixr.app.
#
# Dispatch logic:
# - If `tmnl` is on PATH, open mixr inside a tmnl native tab
#   (GPU-rendered, family-aware). This is the "all three installed"
#   path — DJs get the proper visuals without thinking about it.
# - Otherwise fall back to opening mixr standalone in macOS's
#   Terminal.app. Works without tmnl; mixr's TUI runs anywhere.

set -eu

bundle_root="$(cd "$(dirname "$0")/../.." && pwd)"
mixr_bin="$bundle_root/Contents/Resources/bin/mixr"

# Finder/LaunchServices strips $PATH down to a system minimum, so a
# Homebrew-installed `tmnl` isn't visible unless we source profile.
if [ -f "$HOME/.zshrc" ]; then
    # shellcheck disable=SC1091
    source "$HOME/.zshrc" 2>/dev/null || true
fi
if [ -f "$HOME/.bash_profile" ]; then
    # shellcheck disable=SC1091
    source "$HOME/.bash_profile" 2>/dev/null || true
fi
export PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin"

if command -v tmnl >/dev/null 2>&1; then
    exec tmnl --mixr --editor "$mixr_bin"
fi

osascript <<EOF
tell application "Terminal"
    activate
    do script "exec '$mixr_bin'"
end tell
EOF
