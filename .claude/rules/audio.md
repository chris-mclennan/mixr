# Audio Engine Rules

- Never `println!` or `print!` from any code — use `tracing::info!`/`error!`/`debug!` (stdout corrupts TUI)
- DeckPlayer is behind `Arc<Mutex>` — lock briefly, never hold across await points
- Audio callback (`fill_output`) must be lock-and-return — no allocation, no I/O, no panics
- Phase alignment uses `BeatGrid::phase_align_advance()` (signed shortest-path) with `seek_forward_safe` fallback
- Test crossfade at multiple BPM ranges (90, 128, 150+)
- Auto-grid correction is session-only — never persisted to disk
- ClaudeDJ rate limit is adaptive (starts 2s Auto/Assist, 1s Manual; 429 exponential backoff); round cap is mode-aware: `MAX_ROUNDS = 10` in Prep (between mixes), `MAX_ROUNDS_MANUAL = 20` in Performance (during crossfade); fresh tool_result bodies truncated to 500 chars, older ones retro-compacted to 80 chars across rounds so input tokens stay under the 50k/min Anthropic limit
- `set_crossfade_bars` must validate range (4-64); glide_bars is config-only (0 = Max sentinel, no engine setter)
- BeatGrid is Copy — pass by value, not reference
