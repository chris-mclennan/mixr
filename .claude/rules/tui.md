# TUI Rules

- Add `self.toast.show()` for every user-facing action (queue, favorite, skip, settings change, etc.)
- Never `println!` — it corrupts the TUI. Use `tracing` for debugging, Toast for user feedback.
- When adding/changing keybinds, update all three: app.rs (handler), screens.rs (help text), CLAUDE.md/README.md controls tables.
- Overlays use `push_overlay`/`pop_overlay`.
- ratatui widgets: prefer built-in List, Table, Gauge, Sparkline over manual rendering.
- Dashboard renders via `render_dashboard()` — dual decks, phase indicator, VU meters, mini queue.
- `/` or `s` opens search overlay. Esc returns to previous screen.
- `v` toggles `config.compact_view` globally. `w` cycles waveform mode.
- `c` opens Claude DJ screen. `C` toggles Claude DJ on/off.
- `z`/`Z` opens Virtual Mixer overlay (Tab: switch deck, ↑↓: select row, ←→: adjust, r: reset row, `0`: reset all with Y/N confirm, Esc: close).
- Destructive actions use `App.pending_confirm: Option<ConfirmAction>` — set the variant and show a toast asking Y/N; the global handler at the top of `handle_key` absorbs Y/N/Esc and dispatches the committed action.
