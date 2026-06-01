# Changelog

All notable changes to **mixr** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Day-to-day development notes live in [`CLAUDE.md`](CLAUDE.md); the roadmap in
[`.local/PLAN.md`](.local/PLAN.md).

## [Unreleased]

## [0.1.3] - 2026-05-31

### Added

- **Settings — Library section.** New text/path rows for `local_library_dir`,
  `rekordbox_xml`, `engine_dj_db`, `serato_db`. Enter on a row → edit mode →
  type the path → Enter saves. No more editing `~/.mixr/config.toml` by hand.

### Changed

- macOS `.dmg` artifact now ships with cargo-dist's standard naming.
- Install page's macOS download button points at the DMG (drag-to-install).

## [0.1.2] - 2026-05-31

### Changed

- macOS `.dmg` ships with the cargo-dist standard naming
  (`mixr-rs-<triple>.dmg`); install page URLs now resolve.
- Smaller fixes (release pipeline cleanup).

## [0.1.1] - 2026-05-31

### Added

- **Folder-drill-down for Local Library** (replaces the 10k-track flat dump):
  subfolders show as menu rows, leaves resolve to rich `TrackList`
  (BPM / key / artist / title / duration), "All tracks (recursive)" at every
  level to flatten the subtree.
- First `.app` bundle + DMG artifacts shipping with releases.

### Fixed

- Settings overlay scroll — focused row stays visible when navigating past
  the bottom of the visible window.

## [0.1.0]

### Added

- **Two-deck engine** — 16-bar phase-locked crossfades with five transition
  types (BeatMatched, EchoOut, BassSwap, FilterSweep, LoopRoll), per-track BPM
  detection, beat-onset and beat-grid analysis.
- **Beatport streaming** — OAuth 2.0 with PKCE against `dj.beatport.com`; an
  embedded WebView handles login, tokens stay on the machine.
- **Library import** — rekordbox (XML + USB `.pdb`), Engine DJ, Serato V2, and a
  local files browser.
- **Virtual mixer** — 3-band EQ, single-knob filter, delay, and beat-aware loop,
  controllable by keyboard, IPC, MIDI, or the AI DJ.
- **AI DJ** — an Anthropic-API-driven DJ that picks tracks, beatmatches, and
  operates the mixer in Auto / Assist / Manual modes, with per-style memory.
- **MIDI controller support** — any controller via `~/.mixr/midi-map.json`, MIDI
  learn, and presets for the Numark Mixstream Pro and a generic 2-channel layout.
- **Pitch stretching** — varispeed by default, with optional pitch-invariant
  Rubberband (FFI) or a pure-Rust WSOLA + phase-vocoder engine.
- **File-based IPC** — drive the whole rig by writing JSON to `~/.mixr/command`;
  read state from `status.json` / `screen.txt` / `quick.txt` / `events.jsonl`.
- **Native integration** — `mixr --blit` runs mixr as a native pane inside the
  [`tmnl`](https://github.com/chris-mclennan/tmnl) terminal.

[Unreleased]: https://github.com/chris-mclennan/mixr/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/chris-mclennan/mixr/releases/tag/v0.1.0
