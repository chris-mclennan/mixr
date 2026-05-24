#!/usr/bin/env bash
# mixr interactive launcher — pick a mode from a menu.
# Companion to ./run.sh (which takes static subcommands). Run this when
# you want to be prompted; run ./run.sh <mode> directly when you know
# what you want.
set -u
cd "$(dirname "$0")"

TEAL=$'\033[38;2;83;192;188m'
GREEN=$'\033[38;2;152;195;121m'
GREY=$'\033[38;2;92;99;112m'
BOLD=$'\033[1m'
RST=$'\033[0m'

printf '\n%s%s┌─ mixr launcher ──────────────────────────────────────┐%s\n' \
    "$BOLD" "$TEAL" "$RST"
printf '%s%s│%s  Pick a mode:                                        %s%s│%s\n' \
    "$BOLD" "$TEAL" "$RST" "$BOLD" "$TEAL" "$RST"
printf '%s%s└──────────────────────────────────────────────────────┘%s\n\n' \
    "$BOLD" "$TEAL" "$RST"

PS3=$'\n'"  ${GREEN}→${RST} pick a number: "
COLUMNS=1
options=(
    "mixr — standalone TUI"
    "mixr — inside tmnl as a native tab (tmnl --mixr)"
    "mixr --logout — clear OAuth tokens + WebView cookies"
    "build — debug build"
    "release — release build"
    "test — run cargo test"
    "check — fmt + clippy (matches CI)"
    "quit"
)
# Note: `mixr --blit <socket>` is intentionally NOT in this menu —
# that mode needs a HOST process (tmnl or mnml) to have already bound
# the socket. Pick option 2 to launch mixr THROUGH tmnl, which mints
# the socket for you. Drop to `./run.sh blit <socket>` if you really
# need to attach by hand (e.g. debugging).
select choice in "${options[@]}"; do
    case "$REPLY" in
        1) exec ./run.sh ;;
        2)
            # Defer to tmnl's run.sh — the sibling repo. Resolve relative
            # to mixr-rs's parent dir; fail clearly if tmnl isn't checked
            # out next to mixr-rs.
            tmnl_dir="../tmnl"
            if [ ! -x "$tmnl_dir/run.sh" ]; then
                printf '  %ssibling repo `../tmnl/` not found at %s — clone tmnl-rs alongside mixr-rs first%s\n' \
                    "$GREY" "$tmnl_dir" "$RST"
                continue
            fi
            exec "$tmnl_dir/run.sh" mixr
            ;;
        3) exec ./run.sh logout ;;
        4) exec ./run.sh build ;;
        5) exec ./run.sh release ;;
        6) exec ./run.sh test ;;
        7) exec ./run.sh check ;;
        8) echo "bye"; exit 0 ;;
        *) printf '  %sunknown choice %q — try again%s\n' "$GREY" "$REPLY" "$RST" ;;
    esac
done
