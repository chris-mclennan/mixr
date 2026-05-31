---
title: Install
description: How to install mixr.
---

## Cargo

```sh
cargo install mixr-rs
```

The crate name on crates.io is `mixr-rs`; the binary it installs is `mixr`.

## Build from source

```sh
git clone https://github.com/chris-mclennan/mixr
cd mixr
cargo build --release
./target/release/mixr
```

You'll need an audio output stack — CoreAudio on macOS, ALSA / PipeWire on Linux, WASAPI on Windows. All come standard.

For Beatport login, mixr opens an embedded WebView — that needs WebKit on Linux (`apt install libwebkit2gtk-4.1-dev` or distro equivalent). macOS + Windows bundle WebKit natively.

### Wrapper script (recommended)

The repo's `run.sh` / `run.ps1` auto-detect librubberband and implement the restart-on-exit-75 loop:

```sh
./run.sh          # macOS / Linux (bash)
./run.ps1         # Windows (PowerShell)
```

## Beatport login

On first launch, mixr opens an embedded WebView pointing at `dj.beatport.com`. Sign in there — credentials never touch mixr. The exchanged OAuth token is stored at `~/.mixr/auth.json` and refreshed automatically. Run `mixr --logout` to clear it.

> Audio quality tops out at 256k HLS (AAC). FLAC streams require a higher-tier OAuth scope only available to Beatport's own clients.

## Optional: pitch-invariant time stretching

By default mixr uses varispeed — pitch shifts with tempo, zero CPU overhead. For pitch-invariant stretching, pick one of:

**Rubberband** (recommended quality, GPL'd):

```sh
brew install rubberband              # macOS
apt  install librubberband-dev       # Debian / Ubuntu
dnf  install rubberband-devel        # Fedora
pacman -S rubberband                 # Arch
cargo build --release --features rubberband
```

Settings → Pitch Stretch → Rubberband, and mixr will auto-detect + auto-install + rebuild + restart.

**Timestretch** (pure Rust, no external lib):

```sh
cargo build --release --features timestretch
```

> Default builds (no feature flag) contain zero GPL code. If you ship a binary built with `--features rubberband`, you must comply with GPL v2+.

## Hardware controllers

Plug in your controller before launching mixr. Two preset MIDI mappings ship:

- **Numark Mixstream Pro Go Plus** — full preset
- **Generic 2-channel** — pad/jog/EQ/fader controllers

For other controllers: drop a TOML mapping in `~/.mixr/midi/<your-controller>.toml`, or use the in-app **MIDI Learn** mode (`K` from the dashboard) to bind by waggling each knob.

## Optional: Claude DJ

Bring your own Anthropic API key:

```sh
mixr --claude-key sk-ant-...
```

Stored at `~/.mixr/claude.toml`. Toggle Claude DJ from the dashboard with `C`; full Claude DJ screen with `c`.

## Verify

```sh
mixr --version
mixr --status                 # current state (no TUI)
mixr                          # launch the dashboard
```
