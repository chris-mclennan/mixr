# Command + Keymap migration

Mixr's key handling is being incrementally refactored to mirror mnml's
`Command` + `Keymap` registry pattern, so the help overlay (#59) can be
auto-generated from a single source of truth.

## What landed

Scaffolding only. Compiles + tests pass; no dispatch is wired yet.

- `src/tui/command.rs` — `Command` struct, `Registry`, `registry()`,
  `run(id, app)` dispatcher. 6 stub commands registered (`view.dashboard`,
  `view.browse`, `view.history`, `view.settings`, `view.help`, `app.quit`)
  with no-op handlers.
- `src/tui/keymap.rs` — `Chord`, `parse_key_spec`, `Keymap::build`.
  Chord normalization (e.g. `"P"` ≡ `"shift+p"`) matches mnml exactly.
- `config.rs` — new `keys: HashMap<String, BTreeMap<String, String>>` on
  `AppConfig` so users can override / unbind via `~/.mixr/config.json`.
- 3 unit tests covering chord parsing, normalization, default bindings.

## The blocker: context guards

Mixr's bindings are heavily state-dependent. Examples from `keys.rs`:

| chord | line | context |
|---|---|---|
| `d` | 663 | sets `view_mode = Browse` (when on dashboard) |
| `d` | 1279 | returns to Dashboard (when in mixer overlay) |
| `b` | 619 | search prompt: backspace-equivalent |
| `b` | 887 | switches to Browse view |
| `b` | 1006 | jumps to Browse (when on history) |
| `?` | 715 | toggles `dash_help` |

A flat `chord → command-id` table can't handle this — the dispatcher
needs to know "which context are we in." mnml solves this via the
`InputHandler` trait (vim normal/insert/visual each have their own
handler, and the global keymap is only consulted from the standard-mode
outer scope). Mixr doesn't have that abstraction yet.

## Path forward

### Phase 1 — done
Foundation files + config field. Zero dispatch changes. Help overlay
unchanged.

### Phase 2a — done
`Command::when: Option<fn(&App) -> bool>` field added. `try_dispatch`
primitive landed in `command.rs` — looks up `key` in `keymap`, resolves
to a registry entry, checks the `when` guard, runs the handler. **Not
yet called from `handle_key`** (one wrong move there would change the
semantics of `?` / `d` / `b` mid-session). All 6 stub commands have
`when: None` for now since their handlers are still no-ops.

### Phase 2b — done
- `App.keymap: Keymap` field added + built from config in `App::new`.
- `try_dispatch(&KeyEvent, &mut App)` signature refactored to take
  `&mut App` (the borrow-split problem — `Keymap` lives inside `App`).
- `handle_key` calls `try_dispatch` immediately after the modal
  early-returns (`pending_midi_map`, `pending_confirm`,
  `command_prompt`, `pending_resume_prompt`) and before the per-view
  match.
- **First chord migrated**: `?` → `view.help`. Handler toggles
  `app.dash_help`. `when` guard: Dashboard view, no modal capturing
  input, no filtering, no DJ-ask buffer. The legacy
  `KeyCode::Char('?')` arm in `keys.rs` is gone.

### Phase 2c — **substantially complete** (~100 chords migrated, both legacy match blocks empty)

**Final milestone**: every chord that fires outside an active modal
context has migrated to the registry. Both the Dashboard-mode
`if matches!(view_mode, Dashboard) { match key.code { ... } }`
block AND the post-modal global `match key.code { ... }` block
are fully empty in `keys.rs`. They're left as marker comments
showing where the legacy code lived.

**Done (by category):**
- **Views**: `d` (two variants), `b` (two variants), `h`, `,`, `q`,
  `Tab`, `?` (two variants), `K`, `/` (two variants), `s`, `c`,
  arrows (`↑↓←→/↩` Dashboard focus-aware)
- **Playback**: `B`, `m`, `S`, `M`, `G`, `p`, `n`, `t`, `T`, `A`,
  `1`-`4`, `!@#$`, `i`/`I`/`u`/`U`/`O`, `;`/`'`/(/), `<`/`>`,
  `+`/`=`/`-`/`_` (three variants each), `[`/`]`, `space`, `w`
  (dashboard), `v` (two variants)
- **Queue**: `e`, `x`, `X`, `{`, `}`, `L` (dashboard), `a`
- **Browsing**: `o`, `r`, `+` (non-Dashboard), `Ctrl+F`/`F`,
  `f`/`*` (two variants), `&` (two variants), Dashboard Esc
- **App**: `y`, `C`, `:`
- **Quit**: `Ctrl+C` (event-loop owned, registered for help only)

**Helper predicates**: `no_modal_capture(app)` and
`dashboard_normal(app)` cover ~80% of guards. Inline custom guards
handle the rest (`!Dashboard && no_modal_capture`,
`view_mode == History && ...`).

The multi-value keymap was added in 2b mid-stream — same chord →
list of commands, first matching `when` wins. Critical for chords
like `d`, `b`, `?`, `v`, `+`, `/`, `f` where the same chord has
different meanings on Dashboard vs elsewhere.

**What's left in `keys.rs`** (intentionally not migrated):
- Modal-capture handlers that need to greedily consume keystrokes:
  - Settings text-edit (Esc/Enter/Backspace/Char)
  - Mixer overlay rows (Tab/Up/Down/Left/Right/r/0)
  - Rules editor (delegated to `super::rules_editor::handle_key`)
  - Search input (Esc/Enter/Backspace/Char/Up/Down)
  - Filter input (Esc/Enter/Backspace/Char/Up/Down)
  - Playlist name input + playlist picker (Esc/Enter/Backspace/Char/Up/Down)
  - MIDI learn capture (action picker + binding overlay)
  - Resume prompt (Y/N/Esc with snapshot apply)
  - Midi-map confirm (Y/Enter/N/Esc)
  - Command prompt body (Esc/Enter/Backspace/Char)

These are inherently context-greedy — a registered `Command` with
a single fn-pointer handler doesn't model "this chord absorbs every
keystroke until the modal exits." Keeping them in the
modal-early-return blocks is the right shape.

### Phase 3 — done

Help screen is auto-generated for every chord in the registry, with
hand-maintained `legacy_help_lines()` fallback for CLI options /
file paths / Beatport navigation (non-chord rows).

### Status: registry-driven mixr ✓

Mixr's command system is now spine-aligned with mnml. Adding a new
binding is a one-row edit to `builtin_commands` — the keymap,
dispatch, and help all update automatically. Future work: rebinding
via `[keys.global]` in `~/.mixr/config.json` (the plumbing is
already in place; just needs documentation and a settings UI row).

**Still in `keys.rs`** (rough categories — fewer than at start):
- Dashboard-nested with focus-sensitivity: `Up`/`Down`/`Enter`/`Left`/
  `Right`, `L` (load next), `&` (add to cart), `f`/`*` (favorite),
  `+`/`-` (rate mix), `A` (AI analyze, async), `1`–`4` (hot cues),
  `Shift+1..4` (set cue), `Tab` (cycle focus), `z`/`Z` (mixer overlay),
  `v` (dashboard layout cycle), `/` (DJ ask / search).
- Top-level multi-context (same chord, different `when` per state):
  `+`/`=`/`-`/`_` (rate mix vs playlist picker vs ratings),
  `?` (also `view_mode = Help` outside dashboard — second variant
  not yet added), `v` (compact view toggle when not Dashboard),
  `w`/`W` (follow/unfollow artist, async).
- Browse-state navigation: arrows, Enter, `/`, `Esc`, `Backspace`
  (during filter), search-result navigation.
- Hot cue jumps + sets (Shift+1..4 = !@#$).
- Mixer overlay rows.
- Rules editor.
- Compact `v`, history `y` (copy to clipboard), `+` (add to playlist).

### Phase 3 — help auto-gen
Once enough top-level chords are migrated, replace
`screens::help_lines()` with a function that walks `registry().all()`
+ reverses `self.keymap`. Mirrors mnml's `app::help::build_help`.

For partial migration, the auto-gen function could prepend the
registry rows + append a "(hand-maintained legacy)" section with the
existing `help_lines()` body for un-migrated chords. Drift-free for
the migrated half; still hand-edited for the rest.

Migration order (lowest-risk first):
1. **Globally unambiguous chords**: `ctrl+c` (quit) — these have one
   semantic regardless of state.
2. **Top-level-only chords**: `?`, `,`, `d`, `b`, `h` — gate with a
   `when: fn(app) -> bool` that checks `app.view_mode == Dashboard &&
   app.prompt.is_none() && …`.
3. **Mode-overlay chords**: mixer overlay (`z`/`Z`), rules editor — each
   becomes its own command group with a guard for the matching state.
4. **Browse-state chords**: arrows, Enter, `/`, etc. — last, hardest.

### Phase 3 — help auto-gen
Once all bindings are in the registry, replace `screens::help_lines()`
with a function that walks `registry().all()` + reverses `self.keymap`,
mirroring mnml's `app::help::build_help`.

### Phase 4 — tmnl
Same playbook in tmnl (#58). tmnl uses `winit::KeyEvent` not crossterm,
so chord-parsing needs a parallel module; everything else matches.

## Open question

Should `Command` handlers take `&mut App` (current) or a wider context
struct (e.g. `&mut Dispatcher`) that owns `App` + the toast queue + the
focus-stack? Mixr's handlers currently mutate several at once; the
wider context might let migration happen without making every helper
method `pub`. Decide at Phase 2 start.
