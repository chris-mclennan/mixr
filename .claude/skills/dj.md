---
name: dj
description: Control mixr as an AI DJ. Use when the user asks to DJ, control playback, manage the queue, monitor mix quality, or interact with the running app.
---

# AI DJ Mode

You control the running mixr app through file-based IPC. The app writes status periodically.

## Read state

| File | What |
|---|---|
| `~/.mixr/status.json` | Live state (track, queue, crossfade) |
| `~/.mixr/screen.txt` | Current TUI frame |
| `~/.mixr/mixr.log` | Engine log |

## Send commands

Write JSON to `~/.mixr/command`. App polls every ~16 ms (atomic
rename-before-read, so no commands are ever lost). Use `>` for a single
command (it overwrites). Use `>>` with a trailing `\n` to queue a
sequence — every appended line is processed in order on the next tick.

### Playback
```bash
echo '{"skip":""}' > ~/.mixr/command
echo '{"pause":""}' > ~/.mixr/command
echo '{"shuffle":""}' > ~/.mixr/command
echo '{"clear":""}' > ~/.mixr/command
```

### Mix control
```bash
echo '{"mixnow":""}' > ~/.mixr/command
echo '{"nudge":-25}' > ~/.mixr/command
echo '{"setrate":1.02}' > ~/.mixr/command
echo '{"crossfader":0.5}' > ~/.mixr/command       # snap crossfader (-1 full A, +1 full B)
```

### Manual mode (Claude DJ Manual)
In Manual mode Claude drives physical decks A and B. The engine does NOT move the crossfader automatically.

Key pattern: load → preview → beatmatch → seek to cue → play_deck + sweep_crossfader once.

```bash
# Load track from current browse position onto deck A
echo '{"key":""}' > ~/.mixr/command   # use load_to_deck tool via Claude

# After starting a crossfade, call sweep_crossfader ONCE — never set_crossfader repeatedly.
# A playing → sweep to +1; B playing → sweep to -1. Direction flips each mix.
# Engine paces the move over bars; calling again while sweep is active restarts it.

echo '{"rate_mix":true}' > ~/.mixr/command     # rate most-recent mix good (+ hotkey)
echo '{"rate_mix":false}' > ~/.mixr/command    # rate most-recent mix bad (- hotkey)
echo '{"claudedj":{"mode":"auto"}}' > ~/.mixr/command  # switch DJ mode live
```

### Navigation
```bash
echo '{"navigate":"up"}' > ~/.mixr/command
echo '{"navigate":"down"}' > ~/.mixr/command
echo '{"navigate":"enter"}' > ~/.mixr/command
echo '{"navigate":"back"}' > ~/.mixr/command
echo '{"browse":"Genres/Melodic House & Techno/Top 100"}' > ~/.mixr/command
echo '{"search":"ARTBAT"}' > ~/.mixr/command
```

## DJ strategy

1. Read status.json to understand current state
2. Find tracks via the BROWSE TREE (Genres > [Genre] > Charts/Top 100), filtering by BPM ±8% and compatible Camelot keys. `search_tracks` is for ARTIST or TITLE lookup only — never for genres or BPM numbers (text search hits track titles, not metadata).
3. Camelot compatibility: same key, ±1 number same letter, or same number A↔B
4. Manage energy: don't jump BPM too drastically, build and release over time
5. Use shuffle after queuing to optimize track order by BPM/key
6. Monitor mix quality via log
