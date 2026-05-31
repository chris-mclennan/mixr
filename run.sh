#!/usr/bin/env bash
# mixr-rs wrapper — restart-on-75 loop + family-common dev subcommands.
# Family convention: `build`/`release`/`test`/`check`/`watch`/`help` are
# shared across mnml + tmnl + mixr-rs.
#
# Usage:
#   ./run.sh                      Run mixr (release profile, restart loop).
#                                 Auto-enables --features rubberband when
#                                 librubberband.{dylib,so} is on the system.
#
# Common dev subcommands (family-wide):
#   ./run.sh build [args]         cargo build [args]
#   ./run.sh release [args]       cargo build --release [args]
#   ./run.sh test [args]          cargo test [args]
#   ./run.sh check                cargo fmt --check + cargo clippy
#                                 --all-targets -- -D warnings
#                                 (matches CI's hard-gated checks)
#   ./run.sh watch                cargo watch -x build  (needs cargo-watch)
#   ./run.sh help                 show this
#
# mixr-specific modes:
#   ./run.sh blit SOCKET          Run mixr as a tmnl/mnml native client
#                                 (`mixr --blit <socket>`). Renders into the
#                                 parent host's grid, not the terminal.
#   ./run.sh logout               Pass --logout to clear OAuth tokens + the
#                                 WebView's persistent cookie store.
#
# Env:
#   No special env vars — release profile is the default since mixr's audio
#   pipeline is timing-sensitive and the debug build's slower mix loop
#   audibly stutters.
set -o pipefail
cd "$(dirname "$0")"

# Auto-detect rubberband — same logic as the original 23-line version.
FEATURES=""
if [ -f /opt/homebrew/lib/librubberband.dylib ] || \
   [ -f /usr/local/lib/librubberband.dylib ] || \
   [ -f /usr/lib/x86_64-linux-gnu/librubberband.so ]; then
    FEATURES="--features rubberband"
fi

case "${1:-default}" in
  # ── Family-wide dev subcommands ─────────────────────────────────
  build)   shift; exec cargo build $FEATURES "$@" ;;
  release) shift; exec cargo build --release $FEATURES "$@" ;;
  test)    shift; exec cargo test $FEATURES "$@" ;;
  check)
    cargo fmt --check || exit 1
    exec cargo clippy --all-targets -- -D warnings
    ;;
  watch)
    if ! command -v cargo-watch >/dev/null 2>&1; then
      echo "[run.sh] cargo-watch not installed — \`cargo install cargo-watch\`" >&2
      exit 1
    fi
    exec cargo watch -x "build $FEATURES"
    ;;
  # ── mixr-specific modes ─────────────────────────────────────────
  blit)
    shift
    socket="${1:-}"
    if [ -z "$socket" ]; then
      echo "[run.sh] blit needs a socket path: ./run.sh blit <socket>" >&2
      exit 2
    fi
    cargo build --release $FEATURES --quiet
    exec ./target/release/mixr --blit "$socket"
    ;;
  logout)
    cargo build --release $FEATURES --quiet
    exec ./target/release/mixr --logout
    ;;
  -h|--help|help) grep -E '^# ' "$0" | sed 's/^# \?//'; exit 0 ;;
  # ── Default ─────────────────────────────────────────────────────
  default) ;;
  # Unknown — let cargo run handle it (e.g. passing through unrecognized
  # positional args / flags for one-off testing).
  *)
    exec ~/.cargo/bin/cargo run --release $FEATURES -- "$@"
    ;;
esac

# Default: restart-on-75 loop.
while true; do
    ~/.cargo/bin/cargo run --release $FEATURES
    EXIT_CODE=$?
    if [ $EXIT_CODE -eq 75 ]; then
        echo "Rebuilding and restarting..."
        sleep 1
    else
        break
    fi
done
