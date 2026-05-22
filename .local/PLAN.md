# mixr — Plan & Roadmap

Working roadmap and planning notes. Shipped features and their test coverage
live in [`FEATURES.md`](../FEATURES.md); detailed development status in
[`CLAUDE.md`](../CLAUDE.md); the user-facing summary in
[`CHANGELOG.md`](../CHANGELOG.md).

---

## Going public

mixr ships open-source as a fresh **`mixr`** repository on GitHub. The current
`mixr-rs` repo carries the full development history and is not made public as-is.

- [ ] Sanitise history → seed the public `mixr` repo (no secrets, no private
      notes in the history).
- [ ] Confirm the crates.io crate name and reconcile it with the package name
      (`mixr-rs`) and the binary name (`mixr`).
- [ ] Get the tree `cargo fmt`-clean and `cargo clippy`-clean, then tighten CI
      (`ci.yml`) to gate on `fmt --check` and `clippy -D warnings` like the
      other tmnl-family repos. CI currently runs clippy for visibility only.
- [ ] First tagged release once the docs + CI are settled.

## Test coverage

[`FEATURES.md`](../FEATURES.md) tags each feature `unit` / `smoke` / `manual` /
`none`. Closing the `none` and `manual`-only gaps is ongoing — priority targets:

- [ ] Transport edge cases — jump / extend / set-rate / nudge / grid-shift
      currently `none`.
- [ ] Mixer automation paths exercised only by manual QA.
- [ ] Widen `scripts/keybind_smoke.sh` IPC coverage.

## Roadmap themes

- [ ] **Cross-platform parity** — macOS is primary; harden the Linux and Windows
      paths (audio device enumeration, the WebView login window, MIDI/HID).
- [ ] **Controller ecosystem** — more hardware presets beyond the Numark
      Mixstream and the generic 2-channel layout.
- [ ] **Transition engine** — additional transition types and richer
      `transitions.json` rule conditions.
- [ ] **AI DJ** — deeper memory, better track-selection signals, smoother
      Manual-mode deck operation.
- [ ] **Native mode** — polish the `tmnl --blit` integration alongside the
      `tmnl-protocol` evolution.

## Notes

- The real-time audio callback is performance-critical — keep allocation and
  locking out of it; the audio profiler (`{"profile":1}` IPC) is the diagnostic.
- The file-IPC surface is a public contract — version changes carefully.
