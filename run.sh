#!/bin/bash
# Restart loop — app exits with code 75 to trigger rebuild + restart.
# Builds release mode and auto-enables the `rubberband` feature when
# librubberband is installed (brew install rubberband on macOS).
cd "$(dirname "$0")"

FEATURES=""
if [ -f /opt/homebrew/lib/librubberband.dylib ] || \
   [ -f /usr/local/lib/librubberband.dylib ] || \
   [ -f /usr/lib/x86_64-linux-gnu/librubberband.so ]; then
    FEATURES="--features rubberband"
fi

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
