<div align="center">

# mixr

**A terminal DJ app for electronic music, in Rust.**

Beatport streaming, beat-locked crossfades, AI-assisted mixing, and hardware
controller support — a full DJ rig in one terminal binary.

[![Crates.io](https://img.shields.io/crates/v/mixr-rs.svg?logo=rust)](https://crates.io/crates/mixr-rs)
[![CI](https://github.com/chris-mclennan/mixr-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/chris-mclennan/mixr-rs/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

</div>

```
┌─ mixr ─────────────────────────────────────────────────────────────┐
│ Deck A  ARTBAT — Element  126.0 BPM  3:42 ███████░░░               │
│ Deck B  Cassian — Run     128.0 BPM  cued                          │
│                                                                    │
│ ◀ ─ ─ ─ ─ ─ ─ ●─────── ─ ─ ─ ─ ─ ─ ─ ▶   crossfader                │
│ MIX IN 4 bars   transition: BeatMatched   key dist 1   gap 1.6%    │
└────────────────────────────────────────────────────────────────────┘
```

## Highlights

- **Beatport streaming** via OAuth (PKCE) — your account, your subscription, no shared credentials
- **Two-deck engine**: 16-bar phase-locked crossfades with five transition types
- **AI DJ**: Claude picks tracks, beatmatches, and operates the mixer (Auto / Assist / Manual)
- **Library import**: rekordbox.xml, Engine DJ DB (desktop + USB), Serato Database V2, plus a local files browser
- **MIDI controller support**: any controller, any binding — preset for Numark Mixstream Pro Go Plus + a generic 2-channel layout
- **Cart + buy** the track that's playing right now (`&` on the dashboard)
- **File-based IPC** at `~/.mixr/command` — script the whole rig from any language
- **Cross-platform**: macOS primary, Linux + Windows supported

## How It Works

Two-deck engine with beat-locked crossfades and five transition types:

1. Browse or search Beatport's catalog (or import your existing rekordbox / Engine / Serato library)
2. Queue tracks — playback starts automatically
3. Engine analyzes each track: BPM detection, first beat onset, beat grid
4. 16-bar crossfade triggers near the end of each track; transition type auto-selected by BPM gap and Camelot key distance
5. **BeatMatched** (equal-power phase-locked) · **EchoOut** (echo tail + hard cut) · **BassSwap** (EQ lows swap at midpoint) · **FilterSweep** (LP→HP sweep) · **LoopRoll** (4-beat loop on outgoing)
6. Decks swap roles, next track preloads, cycle repeats
7. Virtual mixer (3-band EQ, single-knob filter, delay, beat-aware loop) controllable via keyboard, IPC commands, MIDI controllers, or Claude DJ

## Architecture

```
                    ┌─────────────┐
Queue ──→ Download ──→ Analyze ──→│   MixEngine   │
                                  │               │
       Deck A: symphonia ────────→│  cpal output  │──→ Speakers
                                  │               │
       Deck B: symphonia ────────→│   monitor     │──→ Cue (optional)
                                  └─────────────┘
                                          ▲
                          IPC ────────────┤
                          MIDI ───────────┤
                          Claude DJ ──────┘
```

## Build & Run

```bash
# Build
cargo build --release

# Run
cargo run                                              # interactive TUI
cargo run -- --play "Melodic House & Techno" --shuffle # queue genre + shuffle
cargo run -- --genre "Techno" --dashboard              # set genre, start on dashboard
cargo run -- --search "ARTBAT"                         # jump to search
cargo run -- --browse "Genres/Techno/Top 100"          # navigate to browse path
cargo run -- --quality flac|256k|128k                  # set audio quality
cargo run -- --claude-dj "peak hour"                   # enable Claude DJ
cargo run -- --claude-key sk-ant-...                   # store API key
cargo run -- --logout                                  # clear credentials
cargo run -- --status                                  # print playback status (no TUI)
cargo run -- --command '{"skip":1}'                    # send IPC command to running instance
cargo run -- --export                                  # export play history
cargo run -- --favorites                               # list favorited tracks
```

**Recommended**: use the wrapper scripts — they auto-detect librubberband and implement the restart loop (exit 75):

```bash
./run.sh          # macOS / Linux (bash)
./run.ps1         # Windows (PowerShell)
```

### Beatport login

mixr-rs uses OAuth 2.0 with PKCE against `dj.beatport.com`. On first launch, an embedded WebView opens for you to log in to Beatport directly — credentials never touch mixr. The exchanged token is stored at `~/.mixr/auth.json` and refreshed automatically. Log out with `--logout`.

> Audio quality from this OAuth scope tops out at 256k HLS (AAC). FLAC streams require a higher-tier scope only available to Beatport's own clients.

### Pitch stretching

Three modes (Settings → Pitch Stretch):

| Mode | Description |
|------|-------------|
| **Off** (default) | Varispeed — tempo changes shift pitch. Zero overhead. |
| **Rubberband** | Pitch-invariant via librubberband FFI (`--features rubberband`). |
| **Timestretch** | Pure-Rust hybrid WSOLA + phase vocoder (`--features timestretch`). |

To enable Rubberband: select it in Settings and the app will install the system library and rebuild automatically, or run manually:

```bash
brew install rubberband                          # macOS
apt install librubberband-dev                    # Debian/Ubuntu
dnf install rubberband-devel                     # Fedora
pacman -S rubberband                             # Arch
cargo build --release --features rubberband
```

> **Licensing**: librubberband is GPL v2+ / commercial dual-licensed. mixr-rs does not bundle or redistribute it. Default builds (no feature flag) contain zero GPL code. If you distribute a binary with `--features rubberband` you must comply with GPL v2.

### Monitor Device

Settings → **Monitor Device**. Routes audio to a second output device as a DJ headphone cue. Pick any listed cpal output (e.g. "External Headphones"); empty = disabled. IPC `{"monitor_source":"incoming"|"both"|"a"|"b"}` switches what feeds the cue bus at runtime.

### Master Limiter

Settings → **Master Limiter**. `Soft Knee` (default) is transparent under 0.7 amplitude, smoothly folds peaks to ±1.0 above — no hard clipping on stretcher transients or brief dual-deck peaks. `Off` reverts to the original hard ±1.0 clamp.

### Audio profiler

Off by default — the RT callback pays one atomic load per invocation. Turn on when diagnosing stutter / underruns / stretcher cost:

```bash
echo '{"profile":1}' > ~/.mixr/command   # also 0, "on"/"off"/"toggle"
tail -f ~/.mixr/mixr.log | grep 'INFO audio:'
```

You'll see lines like `audio: avg=312µs ratio_max=0.06 misses=0 | decks=120µs echo=40µs mix=80µs`. Anything with `misses>0` or `ratio_max` approaching 1.0 means the callback blew (or came close to blowing) its buffer deadline.

### Transition Rules

Settings → **Edit Transition Rules** opens an editor for `~/.mixr/transitions.json`. Rules evaluate top-down; first match wins. Each rule picks a transition based on a single condition (BPM gap, Camelot key distance, last-transition, mix-count modulo). Actions:

- **Force** — always pick this transition.
- **Cycle** — rotate through a list (deterministic by mix count).
- **Weighted** — pick by percentage split, e.g. `BassSwap 60%, FilterSweep 30%, BeatMatched 10%`. Seeded by mix count so playthroughs are reproducible.

Keys inside the editor: ↑↓ nav, Enter edit, `i` insert, `D` delete, `{`/`}` reorder; in edit Tab switches When/Then/Choices panes, ←→ cycles field/kind/weight, ↑↓ adjusts values or selects a choice, `+` adds a choice. Esc from the list view saves and reloads the engine live.

## Library Import

mixr can browse files alongside Beatport tracks — pull metadata from your existing DJ software's library:

| Source | Path | Notes |
|--------|------|-------|
| **Local files** | `config.local_library_dir` | Recursive scan, symphonia metadata. |
| **rekordbox.xml** | `config.rekordbox_xml` | File → Export → Collection (XML) in rekordbox. |
| **rekordbox USB** | auto-detected | Sticks with `<mount>/PIONEER/rekordbox/export.pdb` (DeviceSQL binary). |
| **Engine DJ** | `config.engine_dj_db` | `~/Music/Engine Library/Database2/m.db` or USB stick at `<mount>/Engine Library/Database2/m.db`. |
| **Serato** | `config.serato_db` | `~/Music/_Serato_/database V2`. |
| **USB sticks** | auto-detected | Any mounted volume with an Engine Library, Pioneer rekordbox export, or `_Serato_` folder appears in the Browse menu. |

Imports are pure metadata — files stay where they are. Beat grids and tempo come from each library's own analysis.

## Controllers

mixr listens on every connected MIDI input. Bindings live at `~/.mixr/midi-map.json`.

**Presets** in `presets/` — copy to `~/.mixr/midi-map.json` to load:

- `numark-mixstream-pro.midi-map.json` — Numark Mixstream Pro / Pro Go Plus, derived from Mixxx's official mapping
- `generic-2-channel.midi-map.json` — conventional 2-channel layout (Hercules, Reloop, generic Pioneer)

**MIDI learn** — press `K` on the dashboard. Move any control on your hardware; pick the action; binding is saved. Any IPC-reachable mixr operation is also MIDI-bindable: crossfader, channel faders, EQ (3-band per deck), filter, tempo, jump bars, play/pause, skip, mix now, hot cues (jump + set), loop beats, nudge, grid shift, transition select.

## Controls

| Key | Action |
|-----|--------|
| ↑↓ | Navigate |
| Enter / → | Select / drill in / next column |
| ← | Previous column (track list) |
| Esc | Back |
| Space | Preview track (toggle) |
| Enter | Queue track |
| a | Queue all tracks |
| p | Pause / resume |
| n | Skip current track |
| t | Teleport to mix point |
| m | Mix now |
| Shift+A | AI analyze mix alignment (during crossfade) |
| < / > | Jump ±N bars (cycle 4/8/16/32) |
| [ / ] | Nudge incoming deck |
| ; / ' | Shift grid ±2 ms |
| ( / ) | Shift grid ±1 beat |
| S | Split cue on/off |
| M | Metronome on/off |
| u/U/i/I/O | Loop 1 / 2 / 4 / 8 / 16 beats |
| / or s | Search |
| q | View queue |
| { / } | Grab / drop queue item (reorder) |
| X | Clear queue (Y/N confirm) |
| h | View history |
| d | Dashboard |
| Tab | Cycle dashboard focus (Controller → Queue → History → Browse → Log) |
| b | Browse library |
| f or * | Toggle favorite on selected track |
| & | Add playing track to Beatport cart |
| + | Add track to playlist |
| o | Open in browser (column-aware) |
| L | Load more (next page) |
| c | Open Claude DJ screen |
| C | Toggle Claude DJ on/off |
| K | MIDI learn (next event → action picker) |
| z / Z | Virtual Mixer overlay (Tab deck, ↑↓ row, ←→ adjust, r reset row, `0` reset all w/ Y/N confirm) |
| + / = | Rate most-recent mix good (dashboard) |
| - / _ | Rate most-recent mix bad (dashboard) |
| 1..4 | Jump to hot cue 1–4 on the playing deck |
| Shift+1..4 (`!@#$`) | Set hot cue 1–4 at current position |
| v | Compact / full view |
| w | Waveform mode (phrase / audio / off) |
| , | Settings |
| : | Command prompt (e.g. `:skip 1`, `:transition echoout`) |
| ? | Help |
| Ctrl+C | Quit |

## Claude DJ

AI-powered DJ using the Anthropic API. Browses Beatport, queues compatible tracks, monitors phase alignment, adjusts mixes, and controls EQ/filter/transitions.

```bash
# Store API key (one-time)
cargo run -- --claude-key sk-ant-...

# Enable with style direction
cargo run -- --claude-dj "melodic techno, build energy"
```

**Modes** (Settings → DJ Mode):
- **Auto** — engine drives crossfades, Claude picks tracks
- **Assist** — Claude comments only, you stay in control
- **Manual** — Claude operates physical decks A/B (loads, previews, beatmatches, sweeps the crossfader)

**Memory**: `+` rates the most-recent mix good, `-` rates it bad. Stored at `~/.mixr/dj_memory.json` and folded into Claude's prompt next session — it learns what works for your style.

## IPC

Write JSON to `~/.mixr/command` from any language; mixr drains the file each tick.

```bash
echo '{"skip":1}'                     >  ~/.mixr/command   # one-shot overwrite
echo '{"transition":"echoout"}'       >> ~/.mixr/command   # queue (append)
echo '{"crossfader":-1}'              >> ~/.mixr/command
```

Status files (read-only):
- `~/.mixr/status.json` — full engine state (every 2s)
- `~/.mixr/screen.txt` — current screen content
- `~/.mixr/quick.txt` — compact key=value status (every tick)
- `~/.mixr/events.jsonl` — append-only event log (queue/play/mix-start/mix-complete)

See `CLAUDE.md` for the full IPC command reference.

## Stack

- Rust 2024, cross-platform (macOS primary, Linux, Windows)
- cpal (audio output), symphonia (decode), ratatui + crossterm (TUI)
- reqwest (HTTP), aes/cbc (HLS decrypt), tokio + futures (async)
- midir (MIDI input), hidapi (HID input), quick-xml (rekordbox.xml), rusqlite (Engine DJ DB)
- chrono, tracing, url, dirs, anyhow, thiserror
- wry + tao (embedded WebView for OAuth login)
- Optional: librubberband (`--features rubberband`), timestretch (`--features timestretch`), stratum-dsp (`--features stratum`)

## The tmnl family

mixr is one of a small family of terminal-native Rust tools:

| Project | What it is | |
|---------|-----------|--|
| [**tmnl**](https://github.com/chris-mclennan/tmnl-rs) | A GPU-accelerated terminal | hosts mixr as a native tab |
| [**mnml**](https://github.com/chris-mclennan/mnml-rs) | A terminal IDE | runs as a native tmnl tab |
| **mixr** | A terminal DJ app | ← you are here |
| [**tmnl-protocol**](https://github.com/chris-mclennan/tmnl-protocol) | The binary wire protocol | mixr's `--blit` integration |
| [**fim-engine**](https://github.com/chris-mclennan/fim-engine) | Embedded code completion | local FIM, used by tmnl & mnml |

mixr runs standalone in any terminal, and integrates as a native pane when
hosted inside `tmnl` (`mixr --blit`).

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for the
workflow and conventions. The roadmap lives in [`.local/PLAN.md`](.local/PLAN.md)
and the release history in [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Third-party licensing notes — in particular Rubberband's GPL boundary (the
optional `rubberband` feature) and Beatport ToS considerations — live in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md). Default builds contain no
GPL code.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
