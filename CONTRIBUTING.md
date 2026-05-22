# Contributing to mixr

Thanks for your interest in mixr. This guide covers the workflow and conventions.

## Getting started

```bash
git clone https://github.com/chris-mclennan/mixr-rs
cd mixr-rs
cargo build
cargo run            # the binary is `mixr`
```

mixr builds on stable Rust — MSRV **1.85**, edition 2024. The default build
pulls in no GPL code and needs no system audio library beyond what the platform
ships. macOS is the primary target; Linux and Windows are supported.

mixr depends on the sibling crate
[`tmnl-protocol`](https://github.com/chris-mclennan/tmnl-protocol) by path —
check it out alongside this repo.

### Optional features

- `rubberband` — pitch-invariant stretching via librubberband FFI. **GPL** —
  see [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md); default builds don't
  touch it.
- `timestretch` — a pure-Rust pitch stretcher (MIT, no FFI).
- `stratum` — an alternate BPM/key analyzer for A/B testing.

## The verification gate

Every change must pass, in order:

```bash
cargo fmt
cargo build
cargo clippy --all-targets   # warning-free
cargo test
```

There is also a keybinding smoke test that drives mixr over IPC:

```bash
./scripts/keybind_smoke.sh
```

## Conventions

- Run `cargo fmt` and keep `cargo clippy --all-targets` warning-free before every
  commit.
- Add tests for new behaviour. The real-time audio callback is performance
  sensitive — keep allocation and locking out of it.
- The file-IPC surface (`~/.mixr/command` and the status files) is a public
  contract; treat changes to it the way you'd treat an API change.
- Match the surrounding code style; see [`CLAUDE.md`](CLAUDE.md) and
  `.claude/rules/` for the audio / TUI / Rust conventions.

## Pull requests

1. Branch from `main`.
2. Make your change with tests; run the verification gate.
3. Open a PR describing the change and how you verified it.
4. CI runs `fmt` + `clippy -D warnings` + `test` — keep it green.

## Reporting bugs & requesting features

Use the [issue tracker](https://github.com/chris-mclennan/mixr-rs/issues). For
audio bugs, include your OS, output device, and whether a pitch-stretch feature
was enabled.

## License

By contributing, you agree that your contributions will be dual licensed under
the MIT and Apache-2.0 licenses, as described in [README.md](README.md#license),
without any additional terms or conditions.
