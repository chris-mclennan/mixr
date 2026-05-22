---
name: mix
description: Start a DJ mix session. Use when the user asks to play a mix, start DJing, or queue music.
---

# Mix Session

The user wants to start a DJ mix. Gather what you need, then set it up.

## What you need

1. **Genre(s)** — which Beatport genre(s)? (e.g. "Melodic House & Techno", "Afro House", "Organic House / Downtempo")
2. **Duration** — how long? (e.g. "2 hours", "30 minutes", "until I say stop")
3. **Vibe/energy** — any preference? (e.g. "high energy", "deep and chill", "build up over time")
4. **Shuffle** — smart shuffle by BPM/key? (default: yes)
5. **Quality** — FLAC, 256k, 128k? (default: FLAC)

If the user provides details (e.g. "/mix 2 hours of Techno"), extract what you can and only ask about what's missing.

## How to set it up

```bash
# 1. Check if mixr is running
pgrep -f mixr 2>/dev/null && echo "RUNNING" || echo "NOT RUNNING"

# 2. If not running, build and launch
cd ~/Projects/mixr-rs && cargo build --release 2>&1
osascript -e "tell application \"Terminal\" to do script \"~/Projects/mixr-rs/target/release/mixr\""

# 3. Send commands via ~/.mixr/command (when IPC is wired up)
echo '{"browse":"Genres/GENRE_NAME/Top 100"}' > ~/.mixr/command
```

## Duration management

- Tracks average ~6-7 minutes each
- 1 hour ≈ 9-10 tracks
- 2 hours ≈ 18-20 tracks
- Queue enough tracks for the requested duration

## After setup

Tell the user:
- What you queued and from where
- How many tracks / estimated duration
- Current BPM range and keys
