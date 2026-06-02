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

### Phase 2c — in progress (18 chords migrated so far)

**Done:** `?`, `B`, `m`, `G`, `S`, `M`, `p`, `n`, `t`, `T`, `d`, `b`,
`h`, `,`, `q`, `e`, `x`, `K`. All use `no_modal_capture` (global) or
`dashboard_normal` (dashboard-nested) guards.

**Still in `keys.rs`** (rough categories):
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
