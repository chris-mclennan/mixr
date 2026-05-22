---
name: dj-monitor
description: Monitor mix quality continuously. Designed to run with `/loop 1m /dj-monitor`. Checks phase alignment, queue depth, and crossfade quality.
---

# DJ Monitor

Check the current state of the running mixr app and report any issues.

## Steps

1. Read `~/.mixr/mixr.log` (last 50 lines) for errors or phase issues
2. Check queue depth — alert if < 3 tracks
3. Look for phase offset warnings (> 15ms = poor alignment)
4. Report status in one line: "OK: 5 queued, phase 2.1ms" or "WARN: queue low (1 track)"

## Auto-correct

If phase offset consistently > 15ms during crossfade:
```bash
echo '{"nudge":-15}' > ~/.mixr/command
```

Keep it brief — this runs every minute.
