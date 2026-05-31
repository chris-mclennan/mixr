---
title: First run
description: Launch mixr, load a track, mix.
---

## Launch

```sh
mixr
```

First launch opens a WebView to log into Beatport. Sign in — mixr never sees your password. After auth, you land on the dashboard.

Useful launch flags:

```sh
mixr --play "Melodic House & Techno" --shuffle  # queue a genre + smart-shuffle
mixr --genre "Techno" --dashboard               # set default genre, jump to dashboard
mixr --search "ARTBAT"                          # jump to search
mixr --browse "Genres/Techno/Top 100"           # navigate browse-tree by path
mixr --quality flac|256k|128k                   # set audio quality
mixr --claude-dj "peak hour"                    # enable Claude DJ with a vibe prompt
mixr --status                                   # print state, no TUI
```

## The dashboard

The default view is the dashboard. Two decks side by side, crossfader between them, status line below, log scrollback at the bottom. `Tab` cycles focus across the 13 sections (Controller / Queue / History / Browse / Log / etc.); `↑↓` move within a section, `←→` adjust.

Switch screens any time:

| Key | Screen |
|---|---|
| `d` | Dashboard (default) |
| `b` | Browse Beatport's catalog |
| `q` | Current queue |
| `h` | Play history |
| `/` or `s` | Search |
| `c` | Full Claude DJ screen (log scrollback) |
| `,` | Settings |
| `?` | Help |

## Transport + mixing

| Key | Action |
|---|---|
| `p` | Play / pause focused deck |
| `n` | Skip to next queued track |
| `t` | Teleport to mix-in point |
| `T` | Rewind last mix |
| `m` | Trigger crossfade immediately |
| `<` / `>` | Jump ±N bars (cycle 4/8/16/32 on dashboard) |
| `[` / `]` | Nudge incoming deck (hold) |
| `;` / `'` | Shift beat grid ±2 ms |
| `(` / `)` | Shift beat grid ±1 beat |
| `:` | Command prompt (e.g. `:transition echoout`) |
| `M` | Metronome overlay |
| `G` | Re-analyze + regrid current deck |
| `R` | Start / stop recording |

## Queue

| Key | Action |
|---|---|
| `Enter` | Queue focused track |
| `a` | Queue everything on current screen |
| `X` | Clear queue (Y/N confirm) |
| `x` | Smart shuffle (BPM + Camelot key compatible) |
| `{` / `}` | Reorder queue item up / down |
| `L` | Load next page of results |
| `Space` | Preview the focused track |

## Hot cues, loops, EQ

| Key | Action |
|---|---|
| `1`–`4` | Jump to hot cue |
| `!@#$` | Set hot cue (shift-1..4) |
| `u` / `U` / `i` / `I` / `O` | Loop 1 / 2 / 4 / 8 / 16 beats (press again to release) |
| `z` / `Z` | Open Virtual Mixer overlay (EQ / filter / volume / loop) |

## Claude DJ

```sh
mixr --claude-key sk-ant-...   # one-time, stores at ~/.mixr/claude.toml
```

Then:

| Key | Action |
|---|---|
| `C` | Toggle Claude DJ on/off |
| `c` | Full Claude DJ screen (conversation log) |
| `/` | Ask DJ a one-off prompt (from dashboard) |
| `+` / `=` | Rate last mix good (teaches DJ) |
| `-` / `_` | Rate last mix bad |

Modes (Settings → DJ Mode):
- **Auto** — Claude runs the whole set unattended
- **Assist** — Claude suggests, you confirm
- **Manual** — Claude operates virtual mixer tools (load/play/EQ/crossfader) while you steer

## MIDI controllers

Plug a controller in. If it matches a bundled preset (Numark Mixstream Pro Go Plus, or the generic 2-channel layout), it auto-binds.

For unsupported controllers:
- Press `K` from the dashboard → MIDI Learn mode
- Waggle each control to bind it
- Saves to `~/.mixr/midi/<device>.toml`

Or write the TOML mapping by hand — see `docs/midi-controllers.md` (in progress) for the format.

## IPC — script the rig

mixr reads JSON commands from `~/.mixr/command`. Useful from any language:

```sh
echo '{"skip":1}'                > ~/.mixr/command  # next track
echo '{"crossfader":-0.5}'       > ~/.mixr/command  # fader toward deck A
echo '{"transition":"echoout"}'  > ~/.mixr/command  # force the next mix type
echo '{"eq":{"deck":"a","low":-12}}' > ~/.mixr/command  # cut deck A bass
echo '{"record":"toggle"}'       > ~/.mixr/command  # start/stop recording
```

Every key binding above has an IPC equivalent — full list in `FEATURES.md`.

## Recording your set

Press `R` at any time. mixr writes WAV (default) to `~/.mixr/recordings/<session>.wav`. Optional `.cue` sheet alongside (Settings → `record_cue_sheet`). Optional auto-save as Beatport playlist (Settings → `record_save_as_playlist`).

## Cart + buy

If a track moves you mid-set, hit `&` on the dashboard — adds the currently-playing track to your Beatport cart. Buy them later from beatport.com.
