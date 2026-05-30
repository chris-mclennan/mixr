---
title: First run
description: Launch mixr, load a track on each deck, mix.
---

## Launch

```sh
mixr
```

First run prompts you to log into Beatport (one-time). After that, you land on the two-deck layout.

## Layout

- **Deck A** (left) — track display, jog wheel, transport, EQ + filter
- **Deck B** (right) — same
- **Crossfader** between them
- **Search bar** at the top — Beatport's catalog + your local library
- **Up next** — the AI-suggested track list, sorted by BPM/key compatibility with the playing deck

## Keys

| Key | Action |
| --- | --- |
| `Space` | Play/pause focused deck |
| `Tab` | Toggle focused deck (A ↔ B) |
| `[` / `]` | Crossfader left/right |
| `q` / `w` | Deck A pitch down / up |
| `o` / `p` | Deck B pitch down / up |
| `/` | Search bar |
| `Enter` | Load focused search result on focused deck |
| `Ctrl-,` | Settings |

(All keys are remappable via `~/.config/mixr/keys.toml`.)

## With a hardware controller

Plug in a supported USB DJ controller before launching. mixr auto-detects via USB MIDI and maps standard buttons (play/pause, jog, EQ knobs, crossfader) to the matching software controls. No setup needed for supported devices.

## Streaming from Beatport

Search by track / artist / label / BPM / key. Click a result, hit Enter, the track loads on the focused deck and starts buffering. mixr keeps a small local cache so re-loading the same track is instant.
