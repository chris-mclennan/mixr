---
title: Install
description: How to install mixr.
---

## Cargo

```sh
cargo install mixr-rs
```

The crate is `mixr-rs`; the binary it installs is `mixr`.

## Or build from source

```sh
git clone https://github.com/chris-mclennan/mixr-rs
cd mixr-rs
cargo build --release
./target/release/mixr
```

You'll need an audio output stack — CoreAudio on macOS, ALSA / PipeWire on Linux, WASAPI on Windows. All come standard.

## Beatport credentials

mixr needs your Beatport login to stream tracks. On first run it prompts for credentials and stores a refresh token at `~/.config/mixr/beatport.toml`.

## Hardware controllers

Plug in a supported controller (Pioneer DDJ-FLX4, DDJ-400, Numark Party Mix, etc.) before launching mixr; it auto-detects via USB MIDI. Unsupported controller? Write a TOML mapping — see the controllers guide (coming).

## Verify

```sh
mixr --version
```
