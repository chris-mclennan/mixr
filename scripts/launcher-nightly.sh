#!/bin/bash
# mixr-nightly-launcher — the executable inside mixr-nightly.app.
#
# Always launches the LATEST cargo release-build from the source
# tree at $HOME/Projects/mixr/target/release/mixr (no bundled
# binary). The whole point of the nightly icon is "click and get
# whatever I just compiled."
#
# Dispatch: same shape as the stable launcher — go through tmnl
# when available, fall back to Terminal.app. Prepends the dev
# binary's directory to PATH so `tmnl --mixr` resolves the nightly
# mixr (not whatever's globally installed).
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

# Resolve tmnl, in order: $PATH → /Applications/tmnl-nightly.app
# (prefer nightly tmnl) → /Applications/tmnl.app (stable).
tmnl_bin=""
if [ -x "/Applications/tmnl-nightly.app/Contents/MacOS/tmnl" ]; then
    tmnl_bin="/Applications/tmnl-nightly.app/Contents/MacOS/tmnl"
elif command -v tmnl >/dev/null 2>&1; then
    tmnl_bin="$(command -v tmnl)"
elif [ -x "/Applications/tmnl.app/Contents/MacOS/tmnl" ]; then
    tmnl_bin="/Applications/tmnl.app/Contents/MacOS/tmnl"
fi

if [ -n "$tmnl_bin" ]; then
    echo "  found tmnl at $tmnl_bin — exec tmnl --mixr" >> "$log_file"
    exec "$tmnl_bin" --mixr
fi

echo "  tmnl not found — falling back to Terminal.app" >> "$log_file"
osascript <<EOF
tell application "Terminal"
    activate
    do script "exec '$dev_bin'"
end tell
EOF
