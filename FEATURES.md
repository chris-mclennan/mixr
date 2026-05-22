# mixr features & test coverage

Source-of-truth catalog of every feature we ship, grouped by area. Each
entry lists how the user reaches it, where it's implemented, and how
it's tested. Anything marked **none** is a gap.

**Test column legend:**
- `unit` — `cargo test` covers it (file listed)
- `smoke` — `scripts/keybind_smoke.sh` exercises it via IPC
- `manual` — only human QA
- `none` — untested today

Last reviewed: 2026-04-16 (after agent review pass 3, 150 unit tests).

---

## Playback & transport

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Play / pause current deck | `p` key, `{"pause":1}` IPC | `audio/engine.rs` `pause()` | smoke (state=Playing visible in quick.txt) |
| Skip to next queued track | `n` key, `{"skip":1}` IPC | `engine::skip` | smoke |
| Teleport to mix-in point | `t` key, `{"teleport":1}` IPC | `engine::teleport` | manual |
| Trigger crossfade immediately | `m` key, `{"mixnow":1}` IPC | `engine::mix_now` | manual |
| Jump ±N bars | `<`/`>` keys, `{"jump":N}` IPC | `engine::jump`, `Mixer::jump_bars` | none |
| Extend playback by N bars | `{"extend":N}` IPC | `engine::extend_playback` | none |
| Set incoming rate directly | `{"setrate":1.05}` IPC | `engine::set_incoming_rate` | none |
| Nudge incoming for phase | `[`/`]` keys, `{"nudge":1}` IPC | `Mixer::nudge_rate` | none |
| Shift beat grid ±ms | `{"shiftgrid":5.0}` IPC | `engine::shift_grid` | none |
| Set mix-in point (seconds) | `{"setmixin":45.0}` IPC | `engine::set_mix_in_point` | none |
| Deterministic mix harness | `{"test_mix":1}` IPC | `browse_path` + crossfade chain | manual (verified end-to-end) |

## Queue management

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Enqueue one track | `Enter` on track | `engine::enqueue` | smoke |
| Queue all on current screen | `a` key, `{"queueall":1}` | `engine::enqueue` loop | smoke |
| Clear queue | `X` key, `{"clear":1}` | `engine::clear_queue` | smoke |
| Smart shuffle (BPM+key) | `x` key, `{"shuffle":1}` | `engine::smart_shuffle` | smoke (preserves count) |
| Queue by Beatport ID | `{"queue_track":N}` IPC | app.rs handler | none |
| Reorder grab/drop | `{`/`}` keys | app.rs handler | none |
| Track duplicate guard | Tick-loop dedupe | `engine.rs:1309` | none |

## Transitions (5 types + rule engine)

| Feature | Entry | Impl | Test |
|---|---|---|---|
| BeatMatched (cos/sin equal-power) | Default | `transition.rs` | unit (volume curves, equal-power invariant) |
| EchoOut (hard cut + echo tail) | BPM gap >8% | `transition.rs` | unit (fader ramp, ramp regression, piecewise continuous) |
| BassSwap (EQ lows swap @ midpoint) | Matched BPM, key dist ≤1 | `transition.rs` | unit (midpoint volume, swap-boundary continuity) |
| FilterSweep (LP→HP sweep) | Matched BPM, key dist =2 | `transition.rs` | unit (equal-power curve shared) |
| LoopRoll (4-beat loop on playing) | Explicit override only | `transition.rs` | unit (equal-power curve shared) |
| Incoming volume monotonic (all types) | Per-sample curve | `transition.rs` | unit (dense sweep) |
| Fader needle monotonic + endpoints | Visual | `transition.rs` | unit (all 5 types) |
| Type selection by BPM+key | `choose()` | `transition.rs` | unit (camelot routing, double-tempo boundary) |
| Override via IPC | `{"transition":"name"}` | app.rs handler | none |
| Rule engine (ordered when/then) | `~/.mixr/transitions.json` | `transition_rules.rs` | unit (5 rule-engine tests) |
| `mix_count_mod(0)` guard | Rule condition | `transition_rules.rs` | unit |
| `last_transition_in` w/ None | Rule condition | `transition_rules.rs` | unit |
| Rules editor UI | Settings → Edit Rules | `tui/rules_editor.rs` | none |

## Virtual mixer

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Per-deck volume (0..1) | `Mixer::set_volume` | `mixer.rs` | unit (clamp) |
| 3-band EQ (-24..+12 dB) | `{"eq":{...}}` IPC | `Mixer::set_eq_{low,mid,high}` | unit (low clamp, shelves passthrough) |
| Single-knob filter (-1..+1) | `{"deck_filter":{...}}` IPC | `Mixer::set_filter` | unit (clamp) |
| Channel faders (0..1) | `{"fader":{...}}` IPC | `Mixer::set_channel_fader` | none |
| Crossfader (-1..+1) | `{"crossfader":N}` IPC | `Mixer::set_crossfader` | none |
| Delay FX (wet/feedback/time) | Used by EchoOut | `Mixer::set_delay_*` | unit (read_echo silence when paused) |
| Beat-aligned loop | `{"loop":{...}}` IPC | `Mixer::loop_beats` / `loop_release` | unit (loop wraps in fill_buffer) |
| Hot cues 0..3 (set/jump/clear) | `{"cue":{...}}` IPC | `Mixer::cue_{set,jump,clear}` | none |
| Tempo match to target | `Mixer::match_bpm` | `mixer.rs` | unit |
| Set rate | `Mixer::set_rate` | `mixer.rs` | unit |
| Tap nudge | `{"nudge":1}` | `Mixer::nudge_rate` | none |
| Grid shift | `{"shiftgrid":ms}` | `Mixer::shift_grid` | none |
| Virtual Mixer overlay | `z`/`Z` keys | `tui/app.rs` render_mixer_overlay | smoke (opens/closes) |
| Mixer row up/down/reset | ↑↓ / `r` / `R` | `adjust_mixer_row` | none |

## Browse hierarchy

Verbatim from `beatport/catalog.rs`. Every menu item is listed with its
`MenuAction` and the screen type it produces — no paraphrasing, no
guessed labels. A leaf `→ Type` means "this action loads and displays a
list of that type"; `→ static` means a static menu constructed in code.

```
Beatport (root_screen)
├── Discover            [PushDiscover → static: discover_screen]
│   ├── Trending        [PushTrending → static: trending_screen]
│   │   ├── Global Top 10        [LoadGlobalTop10     → TrackList]
│   │   ├── Hype Top 10          [LoadHypeTop10       → TrackList]
│   │   ├── Trending Artists     [LoadTrendingArtists → ArtistList]
│   │   ├── Trending Labels      [LoadTrendingLabels  → LabelList]
│   │   └── Trending Genres      [LoadTrendingGenres  → GenreList]
│   ├── Global Top 100  [LoadGlobalTop100 → TrackList]
│   └── Hype Top 100    [LoadHypeTop100   → TrackList]
│
├── Genres              [PushGenres → GenreList (all genres)]
│   └── [genre]         [PushGenreDetail(id,name) → static: genre_detail_screen]
│       ├── Trending    [PushGenreTrending(id,name) → static: genre_trending_screen]
│       │   ├── Top 10              [LoadGenreTop10      → TrackList]
│       │   ├── Playlists           [LoadGenrePlaylists  → ChartList]
│       │   ├── Trending Artists    [LoadGenreArtists    → ArtistList]
│       │   └── Trending Labels     [LoadGenreLabels     → LabelList]
│       ├── Top 100     [LoadGenreTop100      → TrackList]
│       ├── Charts      [LoadGenreCharts      → ChartList]
│       ├── Tracks      [LoadGenreTracks      → TrackList]  (paginated)
│       ├── Releases    [LoadGenreReleases    → ReleaseList] (paginated)
│       ├── Exclusives  [LoadGenreExclusives  → TrackList]  (paginated)
│       ├── Hype        [LoadGenreHype        → TrackList]  (paginated)
│       ├── Decades     [PushGenreDecades(id) → static: decades_screen(Some(id))]
│       │   └── <same decade subtree as top-level Decades, with genre_id>
│       ├── Artists     [LoadGenreArtists     → ArtistList]
│       └── Labels      [LoadGenreLabels      → LabelList]
│
├── Decades             [PushDecades → static: decades_screen(None)]
│   ├── 2020s           [PushDecade(range, name, None) → decade_detail_screen]
│   │   ├── Tracks      [LoadDecadeTracks    → TrackList] (paginated)
│   │   ├── Releases    [LoadDecadeReleases  → ReleaseList] (paginated)
│   │   ├── Charts      [LoadDecadeCharts    → ChartList]   (paginated)
│   │   └── Years       [PushDecadeYears → decade_years_screen]
│   │       └── [year]  [PushYear → year_detail_screen]
│   │           ├── Tracks    [LoadDecadeTracks]
│   │           ├── Releases  [LoadDecadeReleases]
│   │           └── Charts    [LoadDecadeCharts]
│   ├── 2010s  (same shape)
│   ├── 2000s  (same shape)
│   ├── 1990s  (same shape)
│   └── 1980s  (same shape)
│
├── My Beatport         [PushMyBeatport → static: my_beatport_screen]
│   ├── Tracks          [LoadMyTracks         → TrackList] (paginated)
│   ├── Artists         [LoadMyArtists        → ArtistList]
│   ├── Labels          [LoadMyLabels         → LabelList]
│   └── Recommendations [LoadRecommendations  → TrackList]
│
├── My Library          [PushMyLibrary → static: my_library_screen]
│   ├── Collection      [LoadMyDownloads      → TrackList]
│   ├── Cart            [LoadMyCart           → TrackList]
│   └── Playlists       [LoadMyPlaylists      → ChartList]
│
└── Favorites           [PushFavorites → TrackList (local favorites)]

# Secondary entry points reached from TrackList column-drill (→ key):
# ArtistList item   → PushArtistDetail(id,name) → artist_detail_screen:
#   Top 100 [LoadArtistTop100 → TrackList]
#   Tracks  [LoadArtistTracks → TrackList] (paginated)
#   Releases [LoadArtistReleases → ReleaseList] (paginated)
#   Follow / Unfollow [FollowArtist]
#
# LabelList item    → PushLabelDetail(id,name) → label_detail_screen:
#   Top 100, Tracks (paginated), Releases (paginated), Follow/Unfollow
#
# ChartList item    → LoadChartTracks → TrackList
# ReleaseList item  → LoadReleaseTracks → TrackList
# Playlist (from My Library) item → LoadPlaylistTracks → TrackList
```

Terminology anchor: `TrackList`/`ChartList`/`ReleaseList`/`ArtistList`/
`LabelList`/`GenreList`/`Menu` are the seven `BrowseScreen` variants in
`beatport/catalog.rs:9`. Actions marked *(paginated)* support `L` (Load
More) via `catalog::execute_action_page`.

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Root menu | startup | `catalog::root_screen` | smoke (browse view) |
| Full tree traversal | keyboard / `{"browse":"path"}` | `app::browse_path` | smoke (Genres, Discover, Decades, nested drill Genres/Melodic House & Techno/Top 100 & Charts) |
| Drill into track column | →/Enter on artist/remixer/label/genre | `handle_browse_enter` | none |
| Open column in web browser | `o` key | launches browser | none |
| Pagination (Load More) | `L` key | `catalog::execute_action_page` | none |
| Local filter | `Ctrl+F` → type | `filter_text` | none |
| Preview a track | `Space` | `download_for_preview` | none |
| Metronome overlay | `M` key | `engine::toggle_metronome` | none |

## Search

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Open search input | `/`, `s` keys, `{"search":"q"}` | app.rs | smoke (view=search) |
| Run Beatport catalog search | Enter in search | `api::search` | manual |
| Close search | Esc | app.rs | smoke |

## Recording

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Start/stop master recording | `R` key, `{"record":"start\|stop\|toggle"}` | `audio::recorder` | smoke (file created + finalized) |
| Auto-start on launch | Setting `record_always` | `apply_and_sync_setting` | none |
| Format: WAV via hound | Setting | `recorder.rs` | unit (IEEE extended + round-trips) |
| Format: AIFF hand-rolled | Setting | `recorder.rs` | unit (header math) |
| `.cue` sheet alongside | Setting `record_cue_sheet` | `recorder::write_cue_sheet` | unit (`ms_to_mmssff` boundaries) |
| Save set as Beatport playlist | Setting `record_save_as_playlist` | `stop_recording_with_playlist` | none |
| Non-blocking push (pool) | RT-path | `recorder::push` + pool | unit (stop_prepare invariants) |
| Off-lock finalize | On stop | `stop_prepare` + `StopHandoff::finalize` | unit |
| Session label | Filename | `recorder::session_label` | manual |

## Crossfade math & phase sync

| Feature | Entry | Impl | Test |
|---|---|---|---|
| BeatGrid phase (0..1) at a time | Internal | `beat_grid.rs` | unit (phase bounds across 60–220 BPM) |
| Bar phase midpoint | Internal | `beat_grid.rs` | unit |
| `next_downbeat` epsilon fix | Internal | `beat_grid::next_downbeat` | unit (boundary regression at 90/128/150/180 BPM) |
| Phase offset between grids | Internal | `beat_grid::phase_offset` | unit (identical, same-BPM shift, cross-BPM bounded-by-half-beat) |
| Beat/bar interval vs BPM | Internal | `beat_grid.rs` | unit (90/128/150/180 BPM) |
| Camelot key distance | Rule engine | `transition::camelot_distance` | unit (wrap, mode swap, unparseable) |
| Rate correction (kp bands) | `CrossfadeController` | `crossfade.rs` | unit (dead zone, 3-15ms, clamp, monotonic ladder, upper-band clamp, sign, smoothing flip, convergence) |
| Rate correction beat throttle | Within-beat cache | `crossfade.rs` | unit (cached value held) |
| Crossfade duration vs BPM | `CrossfadeController::duration` | `crossfade.rs` | unit (90/128/150 BPM) |
| Cosine ease-out glide | Post-swap | `engine::glide_target_rate` | unit (boundaries, monotonic, ease-out, handoff continuous from rate_correction drift) |
| Pitch-stretcher position accounting | Deck `fill_buffer` → `position += consumed` | `deck.rs` + `pitch_stretch::FakeStretcher` | unit (rate-scaled, BrokenStretcher negative control) |
| Soft-knee limiter | Master out | `engine::apply_limiter` | unit (off/passthrough/monotonic/unity) |

## Monitor device

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Second cpal output for cue | Setting `monitor_device`, `{"monitor_device":"name"}` IPC | `engine::build_monitor_stream` | smoke (set/clear persists to config, effective on restart) |
| Ring cap matches device rate | Internal | `monitor_ring_cap` | none |
| `try_lock` on RT path | Monitor callback | `engine.rs:1564` | none |
| Device list for settings | `output_device_names()` | `engine.rs` | none |

## Claude DJ

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Toggle on/off | `C` key | `claude/dj.rs` | manual |
| Full DJ screen (log scrollback) | `c` key, `ViewMode::ClaudeDj` | `tui/claude_screen.rs` | smoke (opens) |
| Ask DJ prompt | `/` from dashboard | `dj_asking` buffer | manual |
| Tool rounds per trigger | `MAX_ROUNDS = 10` | `dj.rs` | unit (boundary + regression guard) |
| Auto-trigger on low queue | tick loop | `trigger_dj` | manual |
| Auto-trigger on crossfade | tick loop | `trigger_dj` | manual |
| Rate limit (adaptive backoff + clear) | `min_interval` / `handle_rate_limit` | `dj.rs` | unit (clears state, 60s cap, reset) |
| Tool result retro-compaction | Older results squashed across rounds | `compact_old_tool_results` | unit (squash/noop/idempotent) |
| Dangling tool_use closure | Before fresh trigger | `close_dangling_tool_use` | unit (stub + resolved noop) |
| Conversation trim (40 msgs, safe cut) | Internal | `trim_conversation` | unit (short noop + pairing) |
| Tool set (~20 tools) | `tool_definitions` | `dj.rs` | none |
| Prep vs Performance mode split | `CallMode::Prep/Performance` | `dj.rs` | unit (tool set coverage, Prep cap=10 vs Performance cap=20) |
| Manual mode (physical-deck tools) | Settings → DJ Mode: Manual | `dj.rs` + `engine.rs` | unit (manual crossfade progress, prompt references load_to_deck) |
| Sweep crossfader (engine-paced) | `sweep_crossfader` tool / `engine.sweep_crossfader()` | `engine.rs` | unit (tick advances, clears when done) |
| Stall watchdog (manual mode) | Tick loop | `tui/app.rs` | manual |
| Mid-mix phase check (30%/70%) | Tick loop, manual+assist | `tui/app.rs` | manual |
| Training memory (`~/.mixr/dj_memory.json`) | `+`/`-` hotkeys, `rate_mix` IPC | `claude/memory.rs` | unit (good/bad/trim/summary/inject-limit/format) |
| Mix rating hotkeys `+`/`-` | Dashboard | `tui/app.rs` | none |
| Settings: 7 Claude DJ rows | Settings UI rows 28–34 | `tui/settings.rs` | none |
| Quick-mix mode (`QUICK_MIX_MIN_BARS=16`) | DJ Quick Mix setting | `engine.rs` | unit (bar threshold) |
| `AlignmentReadout` + `read_alignment` | `engine.read_alignment()` | `engine.rs` | unit |
| `bar_offset_beats` / `bar_aligned_seek_offset` | Downbeat alignment on crossfade start | `beat_grid.rs` | unit (mismatch detection, seek correctness, complementary) |
| Prompt caching (stable system prompt) | Session-local | `dj.rs` | unit (system prompt stable across trigger; dynamic state in user msg) |

## Settings & config

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Settings screen | `,` key, `{"view_settings":1}` | `tui/settings.rs` | smoke |
| Cycle option Enter/Right | Enter / → | `apply_and_sync_setting` | none |
| Cycle option Left | ← | same helper | none |
| Audio quality | FLAC/256k/128k | config | none |
| Preview quality | 128k/256k/FLAC | config | none |
| Crossfade bars | 8/16/32 | config | none |
| Tempo range ±4..16% | config | config | none |
| Mix-in point | FirstBeat/Drop/Middle | config | none |
| View mode Compact/Full | `v` key | config | none |
| Browser (Chrome/Safari/…) | config | config | none |
| Monitor device picker | Settings row | `output_device_names()` | none |
| Master limiter (Off/SoftKnee) | config | `apply_limiter` | unit |
| Pitch stretch (Off/Rubberband) | config | `pitch_stretch.rs` | smoke (off↔rubberband+rb alias+unknown fallback toast & config) |
| Default genre / favorites | picker | `genre_picker` | none |
| Edit transition rules | Settings row | `rules_editor` | none |

## IPC surface

All commands live in `src/ipc.rs::IpcCommand`. Every one is either tested
here or explicitly marked untested.

| Command | Semantic | Test |
|---|---|---|
| `skip`, `pause`, `teleport`, `mixnow` | playback | smoke (routing) |
| `clear`, `shuffle`, `queueall`, `queue_track` | queue | smoke (clear/shuffle/queueall) |
| `search`, `browse`, `navigate`, `filter` | navigation | smoke (search) |
| `view_*` | view switch | smoke (all variants) |
| `dashboard`, `waveform` | dashboard | smoke |
| `nudge`, `jump`, `extend`, `setrate`, `shiftgrid`, `setmixin` | fine control | none |
| `volume`, `eq`, `deck_filter`, `fader`, `crossfader` | mixer | none |
| `transition`, `loop`, `cue` | transition / deck | none |
| `quality`, `crossfade`, `master_gain` | settings | none |
| `record` | recording | smoke (start/stop cycle) |
| `profile` | profiler toggle | smoke |
| `pitch_stretch`, `install_rubberband` | stretcher | smoke (pitch_stretch) |
| `monitor_device` | persist monitor device config; effective next launch | smoke |
| `playlist_create`, `playlist_delete` | Beatport playlist write | smoke (create→delete→idempotent redelete) |
| `claudedj` | partial Claude DJ settings patch | none |
| `rate_mix` | rate most-recent crossfade good/bad | none |
| `test_mix` | harness | manual |
| `favorite`, `export`, `diagnose`, `get_screen`, `restart`, `status` | utility | smoke (diagnose, get_screen) |
| `key` | SimulateKey (Char only) | smoke |

## Mouse

| Feature | Entry | Impl | Test |
|---|---|---|---|
| Scroll wheel = arrow keys | any view | `app::handle_mouse` | smoke (mouse_click section) |
| Click crossfader bar | dashboard | `dashboard::render_dashboard` + `ClickAction::SetCrossfaderRange` | unit (range math) |
| Drag crossfader | dashboard | `MouseEventKind::Drag` handler in `app.rs` | none |
| Click PLAY/JUMP/NUDGE/CUE | dashboard | per-row label hit-test | none |
| Click EQ/Filter/Tempo/Volume | dashboard | per-section hit-test → FocusDashSection | none |
| Click hot-cue dots `●1..●4` | dashboard | per-dot hit-test → SimulateKey('1'..'4') | none |
| Shift-click hot-cue = SET | dashboard | modifier rewrite to `!@#$` | none |
| Click rows in lists | browse/queue/history/settings | `push_list_row_targets` | none |
| Click activates selected row | second click on same row → Enter | `dispatch_click_action` | none |
| Click `[← back]` | non-dashboard views | top-right Esc target | smoke (mouse_click) |
| Click mini-browse panel | dashboard | `ClickAction::DashBrowseSelect` | none |
| Focused panel highlight | dashboard | `border_style(focused)` cyan vs DarkGray | none |
| Log scrollback | dash_focus=Log + scroll | `read_logs_offset` + `App::log_scroll_offset` | none |
| Synthetic click IPC | smoke harness | `{"click":{col,row,shift}}` → `Click` cmd | smoke (back-button + no-op) |
| `ClickTarget::contains` | hit-test math | `app.rs` | unit (boundary + zero-size) |

## TUI keybinds

Full table in `CLAUDE.md` *CLI Controls* section; everything listed there
is routed through `handle_key` and tested for view-mode routing by
`keybind_smoke.sh`. Not all individual keybinds inside a mode have
positive assertions — see "none" entries above for gaps.

## Coverage snapshot

- **150 unit tests** passing (audio math, mixer clamps, rule engine, recorder internals, beat grid, glide, Claude DJ settings + memory, manual-mode crossfade progress, Prep/Performance mode split, alignment readout, sweep crossfader, sweep tick interp + monotonicity, channel-fader × crossfader interaction, training memory CRUD, equal-power crossfader curve, Beatport cache TTL hit/miss, ClickTarget hit-test math, log-scroll arithmetic).
- **30 smoke assertions** passing (routing, IPC surface, queue, recording, utilities).
- **Known gaps**: browse-tree traversal, mixer-op readback, rules-editor key flow, Claude DJ tool execution, per-deck fine control, mix-rating hotkey, claudedj/rate_mix IPC.

## Adding a new feature

1. Implement it.
2. Add a row to this file.
3. Add the test (unit if it's pure; smoke if it's observable via IPC).
4. Update `CLAUDE.md` controls / IPC tables if user-facing.
