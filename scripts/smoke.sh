#!/usr/bin/env bash
# Run every integration smoke script against the currently-running mixr.
# Exits 0 when all pass, 1 otherwise. Prints a per-script summary.
#
# Requires mixr already running (status.json + quick.txt present).
# Individual scripts can still be invoked directly for targeted runs.

set -u
cd "$(dirname "$0")/.."

if [[ ! -f ~/.mixr/status.json ]]; then
    echo "mixr isn't running (no ~/.mixr/status.json)" >&2
    exit 2
fi

FAILED_SCRIPTS=()

run() {
    local name="$1"; shift
    printf '\n\033[1m── %s ──\033[0m\n' "$name"
    if "$@"; then
        printf '\033[32m%s passed\033[0m\n' "$name"
    else
        printf '\033[31m%s failed\033[0m\n' "$name"
        FAILED_SCRIPTS+=("$name")
    fi
}

run "keybind + IPC + mixer + playback + settings + claude" ./scripts/keybind_smoke.sh
run "browse tree"                                          ./scripts/browse_tree.sh
run "click + drag coverage"                                ./scripts/click_drag_smoke.sh

echo
if (( ${#FAILED_SCRIPTS[@]} == 0 )); then
    printf '\033[32mAll smoke scripts passed.\033[0m\n'
    exit 0
else
    printf '\033[31mFailed: %s\033[0m\n' "${FAILED_SCRIPTS[*]}"
    exit 1
fi
