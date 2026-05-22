# CLAUDE.md - mixr-rs

## Project Overview

mixr-rs is a lean terminal DJ app for electronic music, written in Rust. Beatport streaming for track source. Cross-platform (macOS primary, Linux future).

- **Playing deck** and **incoming deck** — no A/B paradigm; decks swap roles automatically after each mix
- 16-bar crossfade: bars calculated from BPM, aligned to beat 1 (downbeat); 5 transition types
- **Beatport** streaming — search uses `catalog/search/` endpoint; config dir `~/.mixr/`

## Architecture

Single cpal output stream — two DeckPlayers mixed in the audio callback. State machine: `idle → playing → preparingCrossfade → crossfading → swap → playing → ...`

### Module Layout
```
src/
├── audio/          # Engine, decks, beat grid, crossfade, analyzer, transitions, mixer, rules
├── beatport/       # API client, OAuth2 PKCE auth, models, HLS stream downloader, catalog, WebView host
├── claude/         # Anthropic API client, Claude DJ agent
├── tui/            # ratatui app, dashboard, navigator, screens, toast, rules_editor, claude_screen, download, midi_learn
├── config.rs       # AppConfig (serde, ~/.mixr/config.json)
├── favorites.rs    # Favorites store (~/.mixr/favorites.json) + audio sync
├── ipc.rs          # IPC command parser (JSON lines from ~/.mixr/command) + shorthand_to_json (`:` prompt)
├── library_import.rs  # rekordbox XML, rekordbox export.pdb (DeviceSQL), Engine DJ DB, Serato Database V2
├── local_library.rs   # Local audio dir recursive scan + metadata extraction
├── usb_libraries.rs   # USB-stick auto-detection; inserts entries in Browse root menu
├── midi.rs         # midir listener + ~/.mixr/midi-map.json routing → IPC actions
├── hid.rs          # HID scaffold — discovery + listener (decoders pending hardware)
├── log.rs          # tracing to ~/.mixr/mixr.log
├── platform.rs     # OS-specific subprocess helpers (rubberband install + pkg managers)
└── main.rs         # CLI args, terminal setup, event loop
```

### Components
- **MixEngine**: State machine (idle → playing → crossfading → swap), owns cpal stream + both decks, 60Hz tick loop
- **DeckPlayer**: Per-deck audio buffer + playback state, linear interpolation for fractional positions; holds EQ, filter, delay, loop state
- **BeatGrid**: Beat math — phase, bar phase, beat/bar index. Arithmetic grid (constant BPM)
- **TrackAnalyzer**: Offline analysis via symphonia — BPM detection, first beat onset, grid construction
- **CrossfadeController**: Volume curves (equal-power), phase alignment, sync correction. Piecewise EMA rate controller (once per beat); one-shot ±3% kick when offset > 20 ms, then ±1%-clamped proportional path.
- **Mixer**: Virtual mixer control surface — transport, tempo, volume, 3-band EQ, single-knob filter, delay FX, loop, beat-aware jump, grid shift; used by both transitions and Claude DJ
- **TransitionType**: 5 transition types with per-sample volume curves and per-tick automation
- **RuleEngine**: Ordered rule list from `~/.mixr/transitions.json`; first-match selects transition type
- **BeatportAPI**: REST client with reqwest — search, genres, charts, streaming URLs, playlist create/delete. In-memory response cache with 60-min TTL (`BROWSE_CACHE_TTL`) keyed by path+sorted-params — wraps the shared `request()` chokepoint so every API method benefits. Cleared on logout.
- **BeatportAuth**: OAuth2 login, token + credentials in ~/.mixr/auth.json
- **StreamDownloader**: FLAC direct download or HLS (AES-128 CBC decrypt)
- **ClaudeDJ**: AI DJ — Prep/Performance mode split, rate-limited Anthropic API calls with prompt caching (system + tools only), tool use loop, persistent training memory (`~/.mixr/dj_memory.json`)
- **ClaudeAPI**: Anthropic API client — `ask_with_tools()` with prompt caching (`cache_control: ephemeral` on system prompt and last tool). Messages array is not cached (retro-compaction between rounds invalidates message-level cache prefixes). Logs `input/cache_read/cache_write` tokens per call for diagnostics.
- **Quantize / PendingJump / PendingLoopOp**: CDJ-style quantize (on/off + `quantize_beats`). `PendingJump` defers bar-jumps; `PendingLoopOp` defers loop activate/release — both fire at the next beat boundary. **MixSnapshot / RewindOutcome**: captures decks + transition at crossfade start; `request_rewind()` → `InPlace` or `NeedLoad(track)`; blocked mid-crossfade.
- **SessionSnapshot** (`src/session.rs`): persists queue + deck positions to `~/.mixr/session.json`. `ResumeBehavior` (Never/Ask/Always) controls restore on launch. **AnalyzerEngine**: `Builtin` (default) or `Stratum` (`--features stratum`). **`split_ramp`**: per-sample 0..1 slew for split-cue stereo image. **`crossfade_bars_auto`**: genre-aware per-mix length.
- **LocalLibrary** (`src/local_library.rs`): recursive scan of `config.local_library_dir`; symphonia tag extraction; tracks appear in Browse root menu.
- **LibraryImport** (`src/library_import.rs`): rekordbox.xml (Pioneer desktop export), rekordbox export.pdb (DeviceSQL binary — USB sticks / CDJs), Engine DJ `m.db` (SQLite, desktop + USB), Serato Database V2. Imports metadata only — files stay in place.
- **UsbLibraries** (`src/usb_libraries.rs`): scans mounted volumes for Pioneer, Engine DJ, and Serato layouts; auto-inserts entries in the Browse root menu.
- **MidiController** (`src/midi.rs`): midir listener on all inputs; binding map at `~/.mixr/midi-map.json`; routes CC/note-on/pitch-bend to IPC actions. `K` opens MIDI learn overlay (touch control → pick action → saved). Presets in `presets/`: `numark-mixstream-pro.midi-map.json`, `generic-2-channel.midi-map.json`.
- **HidController** (`src/hid.rs`): HID discovery + listener scaffold; vendor decoders pending hardware arrival.
- **BeatportCart** (`src/beatport/api.rs::add_to_cart`): `&` key on dashboard adds the playing (or highlighted) track to the user's Beatport cart. Toast confirms success/failure.
- **OAuthWebView** (`src/beatport/webview_host.rs`): PKCE flow with embedded wry+tao WebView against `dj.beatport.com`. Token stored at `~/.mixr/auth.json`; audio quality tops out at 256k HLS from this scope.
- **CommandPrompt**: `:` opens an inline shorthand prompt (vim-style). Input dispatched via `ipc::shorthand_to_json()` then written to `~/.mixr/command`. `(` / `)` are now grid ±1 beat (freed from `:`/`"`).
- **MIT License**: `LICENSE` + `THIRD_PARTY_LICENSES.md` (Rubberband GPL boundary, Beatport ToS notes).

## Audio Pipeline & Transitions

- **Decode**: symphonia — FLAC, AAC, MP3, OGG; **Output**: cpal; **Mixing**: manual buffer mix in audio callback
- **Pitch stretch** (`src/audio/pitch_stretch.rs`): `Off` (varispeed), `Rubberband` (`--features rubberband`, GPL v2+/commercial, auto-installer), `Timestretch` (`--features timestretch`, pure-Rust WSOLA + phase vocoder, MIT). A/B all three from Settings → Pitch Stretch.
- **Stretcher accounting**: reports source-equivalent of delivered output only, keeping `deck.position` aligned with audible playback (fixes 20–80 ms phantom phase offset).
- **Phrase detection**: novelty curve from self-similarity matrix; **Mix-in points**: FirstBeat, Drop, Middle
- **Pause + crossfade**: `transition.apply()` respects `deck.paused`; `crossfade_progress` tracks playing deck source time — pause freezes needle, resume continues exactly.
- **Phase-align math**: `BeatGrid::phase_align_advance()` — signed shortest-path delta; `seek_forward_safe()` fallback when backward seek would walk past buffer start. Eliminates ~13 ms BPM-ratio residual.
- **Downbeat alignment**: `BeatGrid::bar_offset_beats()` detects beat-aligned but bar-offset decks ("1s don't land together"); `bar_aligned_seek_offset()` corrects via `seek_forward_safe()`. Cleanup re-check after both seeks, final nudge if residual > 2 ms.
- **`AlignmentReadout`** struct (`src/audio/engine.rs`): `beat_phase_ms`, `beat_in_bar_a/b` (0–3), `bar_in_phrase_a/b` (0–15). Returned by `engine.read_alignment()` and the `read_alignment` Claude DJ tool.
- **`AlignmentSamples`** struct (`src/audio/engine.rs`): raw PCM windows from each deck plus `sample_rate`, `playing_bpm`, `incoming_bpm`. Returned by `engine.alignment_samples()` and fed to `audio::ai_beat::analyze_mix_alignment()`.
- **Manual-mix mode** (`engine.manual_mix` flag): `transition.apply()` curves skipped; `crossfade_progress` from `crossfader_pos`. Gated by `claude_dj_enabled`. **30s stall detector** falls back to auto if crossfader idle. **EchoOut override**: forced to `BeatMatched` when manual is active. **CrossfaderSweep**: engine-paced interpolation to `target` over N bars via `engine.sweep_crossfader(target, bars)`; manual drag/direct set cancels.
- **Quick-mix mode** (`DEFAULT_QUICK_MIX_BARS = 16`): auto-fires crossfade after N bars (8..=64). IPC `{"claudedj":{"quickMixBars":N}}`. For iteration, not live sets. Controlled by `ClaudeDjSettings.quick_mix`.
- **Master limiter** (`LimiterMode::SoftKnee` default): `|x| < 0.7` passthrough; above, `sign(x) * (0.7 + 0.3 * tanh((|x|-0.7) * 3))` — transparent at normal levels, folds peaks to ±1.0 smoothly. `Off` = original hard clamp.
- **Monitor Device** (`config.monitor_device`): optional second cpal output as DJ headphone cue bus. Settings picker lists devices. IPC `{"monitor_device":"name"}` persists (stream rebuild on next launch). Runtime source: `{"monitor_source":"incoming"|"both"|"a"|"b"}`.
- **Audio-callback profiler** (`PROFILER_ENABLED` atomic, off by default): samples per-section timings (decks, echo, mix) in `fill_output` and emits a 10-second `INFO audio: avg=... ratio_max=... misses=...` line. Turn on when diagnosing RT underruns, stutter, or stretcher cost — IPC `{"profile":1}`. Off-path cost is 1 atomic load per callback (~1 ns).

5 transition types (`src/audio/transition.rs`): `BeatMatched` (cos/sin equal-power), `EchoOut` (~8.25 bars, hard cut + echo tail), `BassSwap` (both decks full, EQ lows swap at midpoint), `FilterSweep` (LP→HP sweep), `LoopRoll` (4-beat loop on playing, released at progress 0.85).

**Train-wreck handling** (`config.train_wreck_mode`): `Off` / `Detect` (toast only) / `AutoBail` (default — switches transition to EchoOut). Manual `B` key calls `bail_crossfade()`. Fires at most once per crossfade.

Auto-selection: BPM gap > 8% → EchoOut; key dist 0–1 → BassSwap; dist 2 → FilterSweep; else → BeatMatched. Engine post-check forces EchoOut if `bpm_gap_pct > 14` (stretcher) or `> 8` (varispeed) for phase-sync transitions. Rule engine (`~/.mixr/transitions.json`): ordered `{when, then}`, first match wins. Conditions: `bpm_gap_pct_gt/lt`, `key_dist_eq/lte/gte`, `last_transition_eq/in`, `mix_count_mod`. Actions: `force`, `cycle`, `weighted`, `skip`.

## Virtual Mixer

`src/audio/mixer.rs` — single control surface for both transitions and Claude DJ: transport (`play/pause/stop/seek`), tempo (`set_rate`, `match_bpm`), volume (`set_volume` 0–1), 3-band EQ (`set_eq_low/mid/high`, −24..+12 dB), filter (`set_filter` −1 LP..+1 HP, 0 bypass), delay FX (`set_delay_wet/feedback/samples/sync`), loop (`loop_in/out/beats/release`), beat-aware jump (`jump_bars`), tap nudge (`nudge_rate`), grid shift (`shift_grid` ms). Engine-wide state: `crossfader_pos` (−1..+1), `channel_fader_a/b` (0..1).

## TUI (ratatui)

- Declarative widget-based rendering via ratatui + crossterm
- Now playing bar: track name, BPM, time remaining, progress
- Browse: menu navigation with track lists; track list has column navigation (→ cycles title/artist/remixer/label/genre/date, Enter drills into entity, ← goes back, `o` opens that column's entity in browser)
- Dashboard: Deck A always on left, Deck B always on right (physical layout regardless of playing role); Controller box (side-by-side decks, tempo faders, VU meters, beat dots), crossfader needle moves naturally with each mix direction, phase meter, stacked waveform/sparkline; MIXER readout panel below Controller (transition type, crossfader pos, per-deck EQ/filter/fader/loop state); Tab cycles focus Controller→Queue→History→Browse; mini browse panel in dashboard (↑↓ navigate, Enter/→ drill in, Left go up); `b` switches full browse panel or focus
- Dashboard MIXER panel rows show per-deck hot-cue state: `CUE ●1 ○2 ○3 ●4` (filled = set) and the `[loop]` tag when a beat-aligned loop is active.
- Cue countdown: above the crossfader the dashboard shows `MIX IN N bars` before a crossfade fires, counting down in source-domain bars until the mix-in point.
- Overlays: queue (with reorder via {/}), history, help, search, settings, playlist picker, genre/favorites pickers, Virtual Mixer (`z`/`Z`), Transition Rules (Settings → Edit Transition Rules)
- **Transition Rules editor** (`src/tui/rules_editor.rs`): list/edit views over `~/.mixr/transitions.json`. List: ↑↓ nav, Enter edit, `i` insert, `D` delete, `{`/`}` reorder. Edit: Tab cycles When/Then/Choices panes; ←→ cycles fields, kinds, or weights; ↑↓ adjusts values or selects a choice; `+` adds a transition to cycle/weighted; `D` removes. Weighted actions display auto-normalized percentages. Edit view also renders an ASCII preview of the selected transition's playing/incoming volume curves across the crossfade. Esc from list saves and reloads the engine live.
- Toast notifications for user actions
- 60Hz render loop with async event handling
- **Mouse**: full TUI is mouse-driveable. Scroll wheel = ↑↓ in any view. Click + drag the crossfader bar to set position (constant-power equal-power taper); click PLAY/JUMP/NUDGE/CUE labels and EQ/Filter/Tempo/Volume knobs on the dashboard; click hot-cue dots `●1..●4` to jump (shift-click to set); click rows in browse/queue/history/settings (second click activates); `[← back]` button top-right of every non-dashboard view dispatches Esc; mini-browse panel is fully clickable (load/queue tracks without leaving dashboard). LOG panel is in the Tab focus cycle and scroll wheel walks log scrollback when focused. ClickAction infrastructure in `src/tui/app.rs` registers per-frame hit-test rects; renderer pushes targets, mouse handler dispatches.
- **Focused panel highlight**: dash_focus (Controller / Queue / History / Browse / Log) lights the entire perimeter of the focused box in cyan (top, bottom, AND side pipes); other panels stay DarkGray.
- **Mix rating**: `+`/`=` rates the most-recent crossfade good; `-`/`_` rates it bad. Appends to `~/.mixr/dj_memory.json` so Claude carries the lesson forward. Toast confirms; no-op if no mix has happened yet.
- **Auto-dashboard on first play**: when the first track starts (`play_track` from Idle), the view auto-switches to Dashboard if currently on Browse/Queue/History/Settings/Search. Subsequent crossfade swaps stay on whatever view the user is in.
- Favorites: `f`/`*` toggles favorite on selected track; stored in `~/.mixr/favorites.json`; Favorites root menu entry shows saved tracks
- Genre favorites sorted to top with ★ in genre list
- Pagination: `L` loads next page on track/chart/release lists
- Playlist creation: `+` opens playlist picker, creates new Beatport playlist or adds to existing

## IPC (Remote Control)

File-based IPC — write JSON to `~/.mixr/command`, polled each tick. Newline-delimited: `>` overwrite for one-shot, `>>` append to queue back-to-back. Engine atomically renames the file before reading — race-free. Bad lines silently skipped.

**Playback**: `skip`, `pause`, `teleport`, `mixnow`, `nudge` (±1), `jump` (±N bars), `extend` (N bars), `setrate` (float), `shiftgrid` (ms), `setmixin` (s), `volume` `{"playing":0.8,"incoming":0.5}`, `stop_deck` `{"deck":"a"}`, `seek_deck` `{"deck":"a","time":30.0}`

**Queue**: `clear`, `shuffle` / `smart_shuffle`, `queueall`, `queue_track` (Beatport ID), `favorite`

**Navigation**: `search` (str), `browse` (path), `navigate` (up/down/enter/back), `filter` (str)

**Library**: `local_library_dir` (path or empty), `rekordbox_xml` (path or empty), `engine_dj_db` (path or empty), `serato_db` (path or empty) — each triggers a root menu rebuild

**Mixer**: `eq` `{"deck":"a","low":-6,"mid":0,"high":3}`, `deck_filter` `{"deck":"a","pos":-0.5}` (−1..+1), `fader` `{"a":0.8,"b":1.0}`, `crossfader` (−1..+1), `transition` (beatmatched/beat/echoout/echo/bassswap/bass/filtersweep/filter/looproll/loop), `loop` `{"deck":"a","beats":4}` / `{"release":true}`

**Settings**: `quality` (lossless/256k/128k), `crossfade` (bars), `master_gain`, `install_rubberband`, `profile` (0/1/toggle), `monitor_device` (name; takes effect on next launch), `playlist_create` (name → toast id), `playlist_delete` (`{"id":N,"confirm":true}` actually deletes; bare-number form is rejected with a toast asking for `confirm:true`; 404 from server = success), `claudedj` `{"mode":"manual","quick_mix":true}` (any subset of ClaudeDjSettings keys), `rate_mix` (true/false/"+"/"-"/"good"/"bad"), `click` `{"col":N,"row":N,"shift":bool}` (synthesize mouse click for smoke tests)

**Views**: `dashboard`, `view_browse`, `view_queue`, `view_history`, `view_settings`, `waveform` (phrase/audio/off)

**Utilities**: `key` (char), `export`, `diagnose`, `get_screen`, `restart`, `status`, `test_mix` (Global Top 10 → queue all → teleport → crossfade)

Output files (read-only): `~/.mixr/status.json` (full state, every 2s), `~/.mixr/screen.txt` (screen dump), `~/.mixr/quick.txt` (compact key=value, every tick), `~/.mixr/events.jsonl` (append-only event log), `~/.mixr/diagnose.json` (on demand), `~/.mixr/history-DATE.txt/.json`, `~/.mixr/favorites.json`, `~/.mixr/cache/`, `~/.mixr/transitions.json`, `~/.mixr/dj_memory.json`.

## Claude DJ

AI-powered DJ using the Anthropic API (claude-haiku-4-5 model). API key: `~/.mixr/claude_key`. Toggle `C`; ask mode `/` on dashboard.

**Modes** (Settings → DJ Mode): `Auto` (engine drives curves, Claude picks tracks), `Assist` (comments only), `Manual` (Claude drives physical decks A/B — loads, previews, beatmatches, sweeps crossfader; engine provides phase-sync + downbeat-align safety rails but not autopilot; `transition.apply()` curves skipped).

**Trigger modes**: `Prep` (between mixes, `MAX_ROUNDS=10`) vs `Performance` (crossfading, `MAX_ROUNDS_MANUAL=20`). Auto-triggers: queue < 3 tracks (30s debounce), crossfade start, mid-mix checks at ~30%/~70%, stall watchdog >10s.

**Rate limiting**: 2s Auto/Assist, 1s Manual; 429 → exponential backoff (max 60s). Fresh tool_result bodies → 500 chars, older → 80 chars (retro-compaction). System prompt + tools prompt-cached; messages array not cached. Dynamic state in per-turn user message.

**Training memory** (`~/.mixr/dj_memory.json`): `+`/`=` rates most-recent crossfade good, `-`/`_` rates bad. Injected into Prep prompt (top 10 of each category, max 50 stored). `{"rate_mix":true}` IPC also works.

**Settings**: DJ Mode (Auto/Assist/Manual), DJ Camelot/BPM Gap, DJ Transitions, DJ Style, DJ Quick Mix, DJ Memory; **Quantize** + **Quantize Beats** (1/8..8), **Crossfade Bars** (8/16/32/64/Auto), **Glide Bars** (8/16/32/64/Max), **Jump Bars** (4/8/16/32), **Resume Session** (Never/Ask/Always), **Analyzer Engine** (Built-in/Stratum), **AI Beat/Grid/Phrase Detection** (Off/On).

**Manual-mode key tools**: `load_to_deck`, `preview_deck` (monitor bus only), `stop_preview`, `play_deck`, `seek_deck`, `set_channel_fader`, `set_crossfader` (snap), `sweep_crossfader(target, bars)` (engine-paced — call ONCE, not repeatedly), `jump_beats` (fix "off by N beats"), `read_alignment` (returns `beat_phase_ms`, `beat_in_bar` 0–3 each deck, `bar_in_phrase` 0–15).

**All Prep tools**: `browse_screen`, `select_item`, `go_back`, `search_tracks`, `queue_track`, `queue_all`, `mix_now`, `skip_track`, `read_phase`, `adjust_tempo`, `nudge`, `set_crossfade_bars`, `extend_playback`, `set_eq`, `set_filter`, `set_transition`, `loop_beats`, `loop_release`, `cue`, + all manual-mode tools above.

## Build & Run

```bash
cargo build --release
cargo run                              # interactive TUI
cargo run -- --play "Melodic House & Techno"           # queue genre
cargo run -- --play "Melodic House & Techno" --shuffle # queue + smart shuffle
cargo run -- --genre "Techno" --dashboard              # set genre, start on dashboard
cargo run -- --search "ARTBAT"       # jump to search
cargo run -- --browse "Genres/Techno/Top 100"          # navigate to path
cargo run -- --quality flac|256k|128k                  # set audio quality
cargo run -- --claude-dj "peak hour"   # enable Claude DJ
cargo run -- --claude-key KEY          # store API key
cargo run -- --logout                  # clear credentials
cargo run -- --status                  # print current playback status
cargo run -- --command '{"skip":1}'   # send IPC command to running instance
cargo run -- --export                  # export history
cargo run -- --favorites               # list favorited tracks
```

**Recommended wrappers**: `run.sh` (macOS/Linux) and `run.ps1` (Windows) auto-detect librubberband, pass `--features rubberband` when present, and implement the restart loop (exit 75).

**Rubberband auto-installer**: selecting Rubberband in Settings when the feature is not compiled runs the platform package manager (`brew` / `apt` / `dnf` / `pacman`), rebuilds with `--features rubberband`, and restarts. IPC: `{"install_rubberband":1}`. Unsupported OS shows a toast with manual install instructions.

**Licensing**: librubberband is GPL v2+ / commercial. mixr-rs does not bundle or redistribute it — the user's package manager installs it; it is dynamically linked only when the feature is enabled. Default builds contain zero GPL code.

## CLI Controls
- **↑↓** navigate, **Enter/→** select, **Esc** back, **Ctrl+C** quit
- **Space** preview, **Enter** queue, **a** queue all, **X** clear queue (Y/N confirm)
- **p** pause/play, **n** skip, **t** teleport, **T** rewind last mix, **m** mix now, **G** toggle analyzer + re-grid, **Shift+A** AI align analyze
- **<** / **>** jump N bars (quantized to bar; click `◀ JUMP N ▶` middle on dashboard to cycle 4/8/16/32), **[** / **]** nudge incoming (hold to continue), **;** / **'** shift grid ±2ms, **(** / **)** shift grid ±1 beat, **:** command prompt (e.g. `:skip 1`, `:transition echoout`), **S** split cue (auto during mix), **M** metronome
- **u** / **U** / **i** / **I** / **O** loop 1 / 2 / 4 / 8 / 16 beats (quantized; press same key again to release)
- **q** queue, **h** history, **d** dashboard, **b** browse, **/** or **s** search, **?** help, **,** settings
- **x** smart shuffle, **e** export, **f**/**\*** favorite, **r** sync favorites, **Ctrl+F** filter
- **o** open in browser, **+** playlist, **w**/**W** follow/unfollow, **y** clipboard, **L** load more
- **c** Claude DJ screen, **C** toggle Claude DJ, **K** MIDI learn, **1..4** hot cue jump, **!@#$** hot cue set
- **&** add playing track to Beatport cart (dashboard); **+**/**=** rate mix good (dashboard), **-**/**_** rate bad
- **z**/**Z** Virtual Mixer (Tab deck, ↑↓ row, ←→ adjust, r reset row, **0** reset all (Y/N confirm), Esc close)
- **v** compact/full, **w** waveform (phrase/audio/off), **{**/**}** grab/drop queue item
- **Tab** dashboard focus (Controller→Queue→History→Browse→Log); **↑↓** cycles 13 sections, **←→** adjusts

## Dependencies & Code Style
`cpal`, `symphonia`, `ratatui`+`crossterm`, `reqwest`, `aes`+`cbc`, `tokio`+`futures`, `hound`, `chrono`, `serde`+`serde_json`, `tracing`+`tracing-appender`+`tracing-subscriber`, `url`, `dirs`, `thiserror`+`anyhow`, `midir` (MIDI), `hidapi` (HID), `quick-xml` (rekordbox.xml), `rusqlite` (Engine DJ DB), `wry`+`tao` (OAuth WebView). Optional: `librubberband` (`--features rubberband`), `stratum-dsp` (`--features stratum`).
- Rust 2024 edition; `thiserror` errors, `anyhow` propagation; no `unsafe` except audio callback (`Arc<Mutex>`)
- Prefer owned types over lifetimes at API boundaries; module-per-file, `mod.rs` re-exports
- `tracing::info!`/`error!` for logging — never `println!` (corrupts TUI)
