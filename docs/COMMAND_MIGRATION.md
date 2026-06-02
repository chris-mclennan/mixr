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

### Phase 1 — current (done)
Foundation files + config field. Zero dispatch changes. Help overlay
unchanged.

### Phase 2 — context-guarded commands
Add a `when: fn(&App) -> bool` field to `Command`. Keymap returns only
commands whose guard passes. At each call site in `handle_key`, replace
the literal match arm with `if command::try_dispatch(&self.keymap, key, self)`.

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
