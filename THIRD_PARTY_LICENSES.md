# Third-Party Licenses

mixr-rs itself is MIT-licensed (see `LICENSE`). Optional features
may pull in or call into third-party code with different licenses.

## Bundled dependencies (compiled into the default binary)

All Rust crates listed in `Cargo.toml` ship with mixr-rs binaries
under their own permissive licenses (MIT, Apache 2.0, BSD, MPL 2.0
in a few cases). Run `cargo license` for the full per-crate breakdown.

Key bundled crates worth noting:

- `cpal`, `symphonia`, `ratatui`, `crossterm`, `tokio`, `reqwest`,
  `serde`, `serde_json`, `tracing`, `wry`, `tao`, `muda`, `midir`,
  `hidapi`, `quick-xml`: all MIT or Apache-2.0
- `aes`, `cbc`, `sha2`, `base64`: all Apache-2.0 / MIT dual
- `chrono`: MIT / Apache-2.0

## Optional features (off by default)

### `--features rubberband`

Enables high-quality pitch-stretch via [librubberband](https://breakfastquay.com/rubberband/),
**GPL v2+ or commercial.** mixr-rs does **not bundle** librubberband — the
user installs it via their system package manager (`brew install rubberband`,
`apt install librubberband-dev`, etc.) and mixr dynamically links to it
at runtime when the feature is compiled in.

The default build (no `--features rubberband`) contains zero GPL
code. mixr-rs's MIT license applies to every default-build binary.

If you build with `--features rubberband` and redistribute the
resulting binary linked against librubberband, you may have
additional obligations under the GPL — consult the librubberband
project for guidance.

### `--features timestretch`

Enables a pure-Rust pitch-preserving stretcher via the
[`timestretch`](https://crates.io/crates/timestretch) crate,
MIT-licensed. Bundled when this feature is on; no external library
needed. No license entanglement.

### `--features stratum`

Enables the [`stratum-dsp`](https://crates.io/crates/stratum-dsp)
crate as an alternate BPM/key analyzer, MIT/Apache-2.0. Bundled
when this feature is on. No license entanglement.

## Beatport API access

mixr-rs uses Beatport's public API endpoints with the dj.beatport.com
web app's OAuth client_id. Beatport's terms of service govern API
usage — review them at <https://www.beatport.com/terms> if you plan
to deploy or redistribute mixr-rs.

mixr-rs does not bundle, redistribute, or persist Beatport audio
content. Tracks live in process memory only during playback.
