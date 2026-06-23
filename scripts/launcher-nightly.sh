#!/bin/bash
# mixr-nightly-launcher — the executable inside mixr-nightly.app.
#
# Always launches the LATEST cargo release-build from the source
# tree at $HOME/Projects/mixr/target/release/mixr (no bundled
# binary). The whole point of the nightly icon is "click and get
# whatever I just compiled."
#
# Opens mixr in Ghostty when available, else falls back to
# Terminal.app.
#
# NOTE: do NOT use `set -eu`. Finder strips PATH; if we then
# `source ~/.zshrc` to recover it, any unset-variable reference in
# the user's zshrc trips `set -u` and the launcher exits silently
# with no window opening.

dev_bin="$HOME/Projects/mixr/target/release/mixr"
log_file="${TMPDIR:-/tmp}/mixr-nightly-launcher.log"

{
  echo "----"
  echo "$(date '+%Y-%m-%d %H:%M:%S') mixr-nightly-launcher starting"
  echo "  dev_bin=$dev_bin"
} >> "$log_file" 2>&1

if [ ! -x "$dev_bin" ]; then
    osascript <<EOF
display dialog "mixr-nightly: no build at $dev_bin\n\nRun 'cargo build --release' in ~/Projects/mixr first." buttons {"OK"} default button "OK" with icon caution
EOF
    exit 1
fi

export PATH="$(dirname "$dev_bin"):/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"

# Prefer Ghostty, fall back to Terminal.app.
ghostty_bin=""
if command -v ghostty >/dev/null 2>&1; then
    ghostty_bin="$(command -v ghostty)"
elif [ -x "/Applications/Ghostty.app/Contents/MacOS/ghostty" ]; then
    ghostty_bin="/Applications/Ghostty.app/Contents/MacOS/ghostty"
fi
if [ -n "$ghostty_bin" ]; then
    echo "  found ghostty at $ghostty_bin — exec ghostty -e mixr" >> "$log_file"
    exec "$ghostty_bin" -e "$dev_bin"
fi

echo "  ghostty not found — falling back to Terminal.app" >> "$log_file"
osascript <<EOF
tell application "Terminal"
    activate
    do script "exec '$dev_bin'"
end tell
EOF
