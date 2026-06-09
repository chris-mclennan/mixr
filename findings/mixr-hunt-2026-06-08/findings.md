# mixr bug hunt — 2026-06-08

**Severity counts:** HIGH 3 · MEDIUM 6 · LOW 5 · INFO 3 (17 total)

## Top 3 worst — fix first

1. **H1 — `R` keystroke instantly wipes entire config, no confirmation.**
   `src/tui/keys.rs:1099-1103`. One keystroke nukes everything (output
   device, transitions, Claude DJ, library paths).

2. **H2 — Settings `KeyCode::Char(...)` arms ignore modifiers.**
   `src/tui/keys.rs:1085, 1099, 1109`. `Ctrl+R`, `Alt+R`, `Cmd+R` all
   trigger reset because the match arms have no modifier guard. Same
   for `Char(',')` and `Char('d')` (close settings). Compounds with H1
   — common Ctrl+R muscle memory blows up the user's config.

3. **H3 — Unknown CLI flags silently accepted → TUI launch → audio device backtrace.**
   `cargo run -- --check` was silently accepted (not a known flag),
   fell through to TUI launch, then `MixEngine::new` failed (no default
   audio device) and propagated `anyhow` through `?` to main, dumping
   a 40-line backtrace.

## HIGH

- **H1, H2, H3** — see above.

## MEDIUM

- **M1** Text-edit field accepts `Ctrl+R` etc as literal chars
  (`src/tui/keys.rs:1010-1014`).
- **M2** `shorthand_to_json` JSON-injection via raw key interpolation
  (`src/ipc.rs:43-64`). Low real exposure (needs FS write to
  `~/.mixr/command`), but use `serde_json::to_string` on a built Map.
- **M3** `status.json` + config writes are non-atomic
  (`src/ipc.rs:284`, `src/config.rs:453`). Readers (mnml's now-playing
  chip, external scrapers) see partial JSON during write. Match the
  `read_command` pattern (write-tmp + rename).
- **M4** Local library walk doesn't detect symlink loops
  (`src/local_library.rs:213-230`). `path.is_dir()` follows symlinks.
  Doesn't crash but burns IO until MAX_DEPTH=4.
- **M5** `resync_all_engine_settings` doesn't push output/monitor
  device changes after Reset All (`src/tui/app.rs:2015-2038`). Live
  audio continues on the pre-reset device until next launch.
- **M6** Update-check uses raw string compare → false positives on
  downgrade (`src/update_check.rs:42`). Use semver compare.

## LOW

- **L1** `--claude-key KEY` exposes secret via `ps`
  (`src/main.rs:262-272`). Prefer stdin or env var.
- **L2** Version chip overdraws settings row 0 — the rows below clamp
  to `area.width`, not `area.width - version_len - 2`, so the focused
  row's rightmost characters get overpainted.
- **L3** `truncate_line_to_width(line, 0)` returns 1-char output —
  defensive early-return wanted.
- **L4** `find_first_audio` returns 0.0 when `sample_rate < 100` —
  silent wrong anchoring on corrupt headers.
- **L5** BPM detection silently falls back to 128.0 BPM in 3 distinct
  failure modes — return `Option<f64>` or a confidence score so the
  deck UI can flag manual-override cases.

## INFO

- **I1** Update-check has 10s blocking timeout on a background thread —
  `check_updates = false` is the escape hatch.
- **I2** Engine DJ DB read is `READ_ONLY` + parameterized SQL — no
  injection vector.
- **I3** Verify `♪` chip 18-char truncation path is exercised by
  `~/.mixr/quick.txt` before it hits the statusline.

## Triage order

1. **H3** — user-visible on first launch on machines without audio
2. **H1 + H2** — one-keystroke destructive footgun
3. **M3** — atomic writes matter for external readers (mnml now-playing)
4. **M5** — confusing UX after Reset All
5. **M2** — defensive cleanup
6. Rest — polish

## Methodology

Source review + smoke run (`--status`, `--version`). `cargo test` clean
at 300/300. No active mixr running for live drive — combination of code
review + boundary-condition reasoning.
