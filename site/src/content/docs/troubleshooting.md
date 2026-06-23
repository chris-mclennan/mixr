---
title: Troubleshooting
description: Common installation gotchas, macOS Tahoe warnings, and the nightly bundle.
---

## Nightly bundle

For day-to-day cargo builds you can ship yourself a second dock icon:

```sh
./scripts/build-app.sh --nightly
```

This writes `target/mixr-nightly.app` — bundle ID `sh.mixr.app.nightly` so
it coexists with the stable `mixr.app` in `/Applications` and pins to the
dock separately. The launcher always execs
`~/Projects/mixr/target/release/mixr`, so every `cargo build --release` is
picked up on the next click — no rebundling needed.

The icon inverts the stable palette: teal background with a charcoal
`mixr` wordmark (vs. stable's charcoal background + teal wordmark). At a
glance you can tell whether you're clicking the production build or your
working copy. The nightly bundle is intentionally local-only and is not
published from release CI.

## macOS Tahoe: "Support Ending for Intel-based Apps"

If you're on macOS Tahoe (26) and an early mixr build pops the *"Support
Ending for Intel-based Apps"* dialog on launch, you've hit a known
metadata bug — not a real architecture mismatch. The binary is
arm64-native; the bundle's `Info.plist` was missing
`LSMinimumSystemVersion = 11.0`, and Tahoe interprets the absence as a
legacy Intel app.

Fixed in [`1d4ff6f`](https://github.com/chris-mclennan/mixr/commit/1d4ff6f).
Redownload the latest `.dmg` from
[GitHub releases](https://github.com/chris-mclennan/mixr/releases/latest)
and the warning will be gone.
