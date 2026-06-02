//! The mixr command registry — the spine the help overlay, command prompt
//! (`:`), and (later) MIDI/UI bindings hang off of.
//!
//! Each [`Command`] is a named, group-tagged action with optional default
//! keys and a `fn(&mut App)` handler. The registry is process-global
//! (`OnceLock`) and built once at startup from [`builtin_commands`].
//!
//! Mirrors mnml's `command.rs` shape so the two apps stay structurally
//! similar — the help overlay (`help_lines` in `screens.rs`) reads this
//! registry plus the resolved [`crate::tui::keymap::Keymap`] to render
//! its `<chord>  <title>` rows.
//!
//! **Migration state**: this registry is incrementally adopted. Bindings
//! migrated here are dispatched via [`run`] at the top of `App::handle_key`;
//! everything else falls through to the historical match in `keys.rs`. See
//! `docs/COMMAND_MIGRATION.md` for the remaining work.

// Scaffolding for #59 — handlers don't run yet (dispatch still lives in
// `handle_key`). The dead-code warnings lift as bindings migrate.
#![allow(dead_code)]

use crossterm::event::KeyEvent;

use std::collections::HashMap;
use std::sync::OnceLock;

use super::app::App;

pub type CommandFn = fn(&mut App);
/// Context predicate. Returns true when the command is eligible to fire
/// for the current `App` state. `None` ⇒ "always eligible" (rare; most
/// mixr chords are state-dependent — see `docs/COMMAND_MIGRATION.md`).
pub type WhenFn = fn(&App) -> bool;

#[derive(Clone)]
pub struct Command {
    pub id: &'static str,
    pub title: &'static str,
    /// Help-overlay section (e.g. `"VIEWS"`, `"PLAYBACK"`, `"BROWSING"`).
    pub group: &'static str,
    /// Default keyspecs (e.g. `"ctrl+c"`, `","`, `"?"`). May be empty
    /// (command-prompt-only). [`crate::tui::keymap::Keymap`] parses these.
    pub keys: &'static [&'static str],
    pub run: CommandFn,
    /// State guard. `try_dispatch` only runs the command when this
    /// returns `true` (or it's `None`). Lets the same chord mean
    /// different things in different states — see #59 plan.
    pub when: Option<WhenFn>,
}

impl Command {
    /// A short human-readable hint for the help overlay (`"ctrl+c"`, `","`).
    pub fn key_hint(&self) -> String {
        self.keys.join(" / ")
    }
}

pub struct Registry {
    commands: Vec<Command>,
    by_id: HashMap<&'static str, usize>,
}

impl Registry {
    fn build() -> Self {
        let commands = builtin_commands();
        let by_id = commands
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();
        Registry { commands, by_id }
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.by_id.get(id).map(|&i| &self.commands[i])
    }

    pub fn all(&self) -> &[Command] {
        &self.commands
    }
}

/// The process-global registry.
pub fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Registry::build)
}

/// Run a command by id against `app`. Returns `true` if dispatched.
/// Ignores `when` — direct invocation by id assumes the caller has
/// already verified the context (typical use: command palette).
pub fn run(id: &str, app: &mut App) -> bool {
    if let Some(cmd) = registry().get(id) {
        (cmd.run)(app);
        true
    } else {
        false
    }
}

/// Walk the registry and yield `(keys, title, group)` rows for every
/// command with a non-empty default `keys`. The `keys` field is the
/// joined display form (`"+ / ="` for a command bound to both),
/// matching mnml's help format. Stable order — `builtin_commands`
/// order. Used by the help overlay to auto-generate rows for
/// migrated chords (#59).
pub fn help_rows() -> Vec<(String, &'static str, &'static str)> {
    registry()
        .all()
        .iter()
        .filter(|c| !c.keys.is_empty())
        .map(|c| (c.keys.join(" / "), c.title, c.group))
        .collect()
}

/// Look up `key` in `app.keymap`, resolve the resulting id in the
/// registry, check the command's `when` guard, and run it. Returns
/// `true` when dispatched — the caller (typically `App::handle_key`)
/// should then `return` so the legacy match doesn't double-fire.
///
/// Takes `&mut App` (not a separate `&Keymap`) because `Keymap` lives
/// inside `App` and Rust can't split-borrow it from the rest of the
/// struct. We resolve to an owned `String` to drop the keymap borrow
/// before calling the handler.
pub fn try_dispatch(key: &KeyEvent, app: &mut App) -> bool {
    // Snapshot the resolved ids — the borrow on `app.keymap` has to
    // be dropped before we hand `app` to a handler. `resolve_all`
    // returns `&[String]` so a cheap clone is enough.
    let ids: Vec<String> = app.keymap.resolve_all(key).to_vec();
    if ids.is_empty() {
        return false;
    }
    // Try each binding in order; run the first whose `when` passes.
    // `registry()` is `'static` so command lookups don't borrow `app`.
    // `when` / `run` are `Copy` (fn pointers), liftable before the
    // mutable borrow.
    for id in &ids {
        let Some(cmd) = registry().get(id) else {
            continue;
        };
        let when = cmd.when;
        let run = cmd.run;
        if let Some(w) = when
            && !w(app)
        {
            continue;
        }
        run(app);
        return true;
    }
    false
}

// ── Shared `when` predicates ──────────────────────────────────────
// Most chords on a given view share a precondition ("we're in
// Dashboard mode and nothing is capturing input"). Factoring those
// predicates here keeps `Command` rows skim-readable.

/// True when no modal / prompt / filter is capturing keystrokes —
/// safe to dispatch any chord with global semantics (e.g. `S` toggle
/// split cue) regardless of `view_mode`.
fn no_modal_capture(app: &App) -> bool {
    !app.dash_fav_picker
        && !app.dj_asking
        && !app.filtering
        && app.command_prompt.is_none()
        && app.pending_midi_map.is_none()
        && app.pending_confirm.is_none()
}

/// `no_modal_capture` AND we're on the dashboard. The default guard
/// for chords that were nested inside the legacy
/// `if matches!(self.view_mode, ViewMode::Dashboard) { ... }` block.
fn dashboard_normal(app: &App) -> bool {
    use super::app::ViewMode;
    matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
}

/// Initial command set — these are the *labels* the keymap binds against.
/// Handlers are intentionally no-op stubs for now; the actual dispatch
/// happens through `handle_key`'s existing match. As bindings are migrated
/// out of `keys.rs`, each handler here gets a real body and the
/// corresponding arm in `handle_key` is deleted.
///
/// See `docs/COMMAND_MIGRATION.md` for the porting checklist.
fn builtin_commands() -> Vec<Command> {
    vec![
        // Stubs without default keys — present in the registry so the
        // command palette / help list knows about them, but not yet
        // bound (and not yet implemented). Default `keys` get filled
        // in as each chord migrates from `keys.rs`.
        Command {
            id: "view.dashboard",
            title: "Dashboard (live mix view)",
            group: "VIEWS",
            keys: &["d"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Dashboard;
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "view.browse",
            title: "Browse library",
            group: "VIEWS",
            keys: &["b"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Browse;
                app.selected = 0;
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "view.history",
            title: "Play history",
            group: "VIEWS",
            keys: &["h"],
            run: |app| {
                app.view_mode = super::app::ViewMode::History;
                app.selected = 0;
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "view.settings",
            title: "Settings",
            group: "VIEWS",
            keys: &[","],
            run: |app| {
                app.view_mode = super::app::ViewMode::Settings;
                app.selected = 0;
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "view.queue",
            title: "Queue view",
            group: "VIEWS",
            keys: &["q"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Queue;
                app.selected = 0;
                app.queue_grab_index = None;
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "app.quit",
            title: "Quit mixr",
            group: "APP",
            // ctrl+c is caught by the event loop in main.rs (before
            // handle_key); this row is here for the help listing only.
            keys: &[],
            run: |_app| { /* TODO: migrate from keys.rs */ },
            when: None,
        },
        // Toggle the dashboard's inline help legend. Active only on
        // Dashboard with no modal capturing input — matches the
        // conditions the legacy `KeyCode::Char('?')` arm was gated on.
        Command {
            id: "view.help",
            title: "Toggle dashboard help legend",
            group: "VIEWS",
            keys: &["?"],
            run: |app| {
                app.dash_help = !app.dash_help;
            },
            when: Some(dashboard_normal),
        },
        // Open the full Help view (filterable / scrollable). Fires
        // when `?` is pressed outside Dashboard. Two commands share
        // the chord — `view.help` wins on Dashboard via the
        // multi-value keymap; this one wins elsewhere.
        Command {
            id: "view.open_help",
            title: "Open Help view (filter / scroll)",
            group: "VIEWS",
            keys: &["?"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Help;
                app.help_filter.clear();
                app.help_scroll = 0;
            },
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Cycle the dashboard layout: Full → Panel(Queue → History →
        // Browse → Log) → Full. Saves to config so next launch
        // restores the user's choice.
        Command {
            id: "view.cycle_dash_layout",
            title: "Cycle dashboard layout",
            group: "VIEWS",
            keys: &["v"],
            run: |app| {
                use super::dashboard::{DashLayout, PanelSection};
                let (next_layout, next_section, label) =
                    match (app.dash_layout, app.dash_panel_section) {
                        (DashLayout::Full, _) => {
                            (DashLayout::Panel, PanelSection::Queue, "Panel: queue")
                        }
                        (DashLayout::Panel, PanelSection::Queue) => {
                            (DashLayout::Panel, PanelSection::History, "Panel: history")
                        }
                        (DashLayout::Panel, PanelSection::History) => {
                            (DashLayout::Panel, PanelSection::Browse, "Panel: browse")
                        }
                        (DashLayout::Panel, PanelSection::Browse) => {
                            (DashLayout::Panel, PanelSection::Log, "Panel: log")
                        }
                        (DashLayout::Panel, PanelSection::Log) => {
                            (DashLayout::Full, PanelSection::Queue, "Full view")
                        }
                    };
                app.dash_layout = next_layout;
                app.dash_panel_section = next_section;
                app.config.dash_layout = next_layout;
                app.config.dash_panel_section = next_section;
                app.config.save();
                app.toast.show(label, 1.0);
            },
            when: Some(dashboard_normal),
        },
        // Toggle compact / full track-list rendering. Fires from any
        // view *except* Dashboard (where `v` cycles the layout). Same
        // chord, mutually-exclusive `when` predicates.
        Command {
            id: "view.toggle_compact",
            title: "Compact / Full track-list view",
            group: "VIEWS",
            keys: &["v"],
            run: |app| {
                app.config.compact_view = !app.config.compact_view;
                app.config.save();
                app.toast.show(
                    if app.config.compact_view {
                        "Compact view"
                    } else {
                        "Full view"
                    },
                    1.0,
                );
            },
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Cycle dashboard focus (Controller → Queue → History → Browse).
        Command {
            id: "dash.cycle_focus",
            title: "Cycle dashboard focus",
            group: "VIEWS",
            keys: &["tab"],
            run: |app| {
                app.dash_focus = app.dash_focus.next();
            },
            when: Some(dashboard_normal),
        },
        // Cycle the waveform display mode (phrase/audio/off) when on
        // the dashboard. Top-level `w` (follow/unfollow artist in
        // Browse) is unaffected — it falls through.
        Command {
            id: "view.cycle_waveform",
            title: "Cycle waveform mode (phrase/audio/off)",
            group: "VIEWS",
            keys: &["w"],
            run: |app| {
                app.waveform_mode = app.waveform_mode.next();
                app.toast
                    .show(&format!("Waveform: {}", app.waveform_mode.label()), 1.0);
            },
            when: Some(dashboard_normal),
        },
        // Rate the most-recent mix good (only on dashboard — elsewhere
        // `+` / `-` mean other things, e.g. add to playlist, history
        // rating). `=` / `_` accepted as unshifted alternatives.
        Command {
            id: "engine.rate_mix_good",
            title: "Rate most-recent mix: good (DJ memory)",
            group: "PLAYBACK",
            keys: &["+", "="],
            run: |app| {
                app.handle_ipc_command(crate::ipc::IpcCommand::RateMix(true));
            },
            when: Some(dashboard_normal),
        },
        Command {
            id: "engine.rate_mix_bad",
            title: "Rate most-recent mix: bad (DJ memory)",
            group: "PLAYBACK",
            keys: &["-", "_"],
            run: |app| {
                app.handle_ipc_command(crate::ipc::IpcCommand::RateMix(false));
            },
            when: Some(dashboard_normal),
        },
        // Manual panic / train-wreck bail. Forces the in-progress
        // crossfade onto EchoOut to salvage a bad mix. No-op when not
        // currently crossfading.
        Command {
            id: "engine.bail_crossfade",
            title: "Bail crossfade (manual panic → EchoOut)",
            group: "PLAYBACK",
            keys: &["B"],
            run: |app| {
                if app.engine.bail_crossfade() {
                    app.toast.show("⚠ Bailed to EchoOut", 2.0);
                } else {
                    app.toast.show("Nothing to bail (not crossfading)", 1.0);
                }
            },
            when: Some(dashboard_normal),
        },
        // Force the crossfade to start now. Fires from any view (the
        // legacy arm was outside the Dashboard-only block).
        Command {
            id: "engine.mix_now",
            title: "Mix now (force crossfade)",
            group: "PLAYBACK",
            keys: &["m"],
            run: |app| {
                app.engine.mix_now();
                app.toast.show("Mix now", 1.0);
            },
            when: Some(no_modal_capture),
        },
        // Toggle split cue (deck A → left, deck B → right). Global.
        Command {
            id: "engine.toggle_split_cue",
            title: "Toggle split cue (A=L, B=R)",
            group: "PLAYBACK",
            keys: &["S"],
            run: |app| {
                let on = app.engine.toggle_split_cue();
                app.toast.show(
                    if on {
                        "Split cue: ON (L=deck A, R=deck B)"
                    } else {
                        "Split cue: OFF"
                    },
                    2.0,
                );
            },
            when: Some(no_modal_capture),
        },
        // Toggle the metronome click. Global.
        Command {
            id: "engine.toggle_metronome",
            title: "Toggle metronome",
            group: "PLAYBACK",
            keys: &["M"],
            run: |app| {
                let on = app.engine.toggle_metronome();
                app.toast.show(
                    if on {
                        "Metronome: ON"
                    } else {
                        "Metronome: OFF"
                    },
                    1.0,
                );
            },
            when: Some(no_modal_capture),
        },
        // Play / pause the engine.
        Command {
            id: "engine.pause",
            title: "Play / pause",
            group: "PLAYBACK",
            keys: &["p"],
            run: |app| {
                app.engine.pause();
                app.toast.show("Play/Pause", 1.0);
            },
            when: Some(no_modal_capture),
        },
        // Skip the playing track.
        Command {
            id: "engine.skip",
            title: "Skip / next track",
            group: "PLAYBACK",
            keys: &["n"],
            run: |app| {
                app.engine.skip();
                app.toast.show("Skipped", 1.0);
            },
            when: Some(no_modal_capture),
        },
        // Teleport playhead to the next mix point.
        Command {
            id: "engine.teleport",
            title: "Teleport to mix point",
            group: "PLAYBACK",
            keys: &["t"],
            run: |app| {
                let cfg = app.config.clone();
                app.engine.teleport(&cfg);
                app.toast.show("Teleport to mix point", 1.0);
            },
            when: Some(no_modal_capture),
        },
        // Rewind the last mix (replay / experiment).
        Command {
            id: "engine.rewind_last",
            title: "Rewind last mix (replay/experiment)",
            group: "PLAYBACK",
            keys: &["T"],
            run: |app| {
                use crate::audio::engine::RewindOutcome;
                match app.engine.request_rewind() {
                    Some(RewindOutcome::InPlace) => {
                        app.toast.show("Rewinding last mix", 2.0);
                    }
                    Some(RewindOutcome::NeedLoad(track)) => {
                        let name = format!("{} - {}", track.artist_name(), track.full_title());
                        app.download_and_play(std::sync::Arc::new(track), true);
                        app.toast.show(&format!("Rewinding: {name}"), 2.0);
                    }
                    Some(RewindOutcome::Blocked) => {
                        app.toast
                            .show("Wait for the current mix to finish before rewinding", 2.5);
                    }
                    None => {
                        app.toast.show("No mix to rewind", 1.5);
                    }
                }
            },
            when: Some(no_modal_capture),
        },
        // Export the play history (to a file the engine knows about).
        Command {
            id: "engine.export_history",
            title: "Export play history",
            group: "QUEUE",
            keys: &["e"],
            run: |app| {
                let count = app.engine.export_history();
                if count > 0 {
                    app.toast
                        .show(&format!("History exported: {count} tracks"), 2.0);
                } else {
                    app.toast.show("No history to export", 1.0);
                }
            },
            when: Some(no_modal_capture),
        },
        // Smart-shuffle the queue (BPM + key).
        Command {
            id: "engine.smart_shuffle",
            title: "Smart shuffle queue (BPM + key)",
            group: "QUEUE",
            keys: &["x"],
            run: |app| {
                app.engine.smart_shuffle();
                app.toast.show("Queue shuffled (BPM+key)", 1.5);
            },
            when: Some(no_modal_capture),
        },
        // Open MIDI Learn view.
        Command {
            id: "view.midi_learn",
            title: "MIDI Learn (bind controllers)",
            group: "VIEWS",
            keys: &["K"],
            run: |app| {
                app.view_mode = super::app::ViewMode::MidiLearn;
                app.midi_learn_action_sel = 0;
                app.midi_learn_captured = None;
            },
            when: Some(no_modal_capture),
        },
        // Quantized loop toggles — i/u/U/I/O = 4/1/2/8/16 beats.
        Command {
            id: "engine.loop_4",
            title: "Loop 4 beats (quantized, toggle)",
            group: "PLAYBACK",
            keys: &["i"],
            run: |app| {
                app.engine.loop_toggle_playing(4.0);
                app.toast.show("Loop 4 beats", 1.0);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.loop_8",
            title: "Loop 8 beats (quantized, toggle)",
            group: "PLAYBACK",
            keys: &["I"],
            run: |app| {
                app.engine.loop_toggle_playing(8.0);
                app.toast.show("Loop 8 beats", 1.0);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.loop_1",
            title: "Loop 1 beat (quantized, toggle)",
            group: "PLAYBACK",
            keys: &["u"],
            run: |app| {
                app.engine.loop_toggle_playing(1.0);
                app.toast.show("Loop 1 beat", 1.0);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.loop_2",
            title: "Loop 2 beats (quantized, toggle)",
            group: "PLAYBACK",
            keys: &["U"],
            run: |app| {
                app.engine.loop_toggle_playing(2.0);
                app.toast.show("Loop 2 beats", 1.0);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.loop_16",
            title: "Loop 16 beats (quantized, toggle)",
            group: "PLAYBACK",
            keys: &["O"],
            run: |app| {
                app.engine.loop_toggle_playing(16.0);
                app.toast.show("Loop 16 beats", 1.0);
            },
            when: Some(no_modal_capture),
        },
        // Beat-grid fine shifts (±2ms) on `;` / `'`. Use when the 1s
        // don't quite land together.
        Command {
            id: "engine.grid_shift_back_2ms",
            title: "Shift beat grid -2ms (phase fix)",
            group: "PLAYBACK",
            keys: &[";"],
            run: |app| {
                app.engine.shift_grid_active(-2.0);
                app.toast.show("Grid ◀ -2ms", 0.5);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.grid_shift_fwd_2ms",
            title: "Shift beat grid +2ms (phase fix)",
            group: "PLAYBACK",
            keys: &["'"],
            run: |app| {
                app.engine.shift_grid_active(2.0);
                app.toast.show("Grid ▶ +2ms", 0.5);
            },
            when: Some(no_modal_capture),
        },
        // Whole-beat shifts (±1 beat) on `(` / `)`. Use when grid
        // origin is off by a beat (downbeat fix).
        Command {
            id: "engine.grid_shift_back_beat",
            title: "Shift beat grid -1 beat (downbeat fix)",
            group: "PLAYBACK",
            keys: &["("],
            run: |app| {
                app.engine.shift_grid_active_beats(-1);
                app.toast.show("Grid ◀ -1 beat", 0.8);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.grid_shift_fwd_beat",
            title: "Shift beat grid +1 beat (downbeat fix)",
            group: "PLAYBACK",
            keys: &[")"],
            run: |app| {
                app.engine.shift_grid_active_beats(1);
                app.toast.show("Grid ▶ +1 beat", 0.8);
            },
            when: Some(no_modal_capture),
        },
        // Jump N bars back / forward (N from config).
        Command {
            id: "engine.jump_back_bars",
            title: "Jump back N bars",
            group: "PLAYBACK",
            keys: &["<"],
            run: |app| {
                let bars = app.config.jump_bars as i32;
                app.engine.jump(-bars);
                app.toast.show(&format!("Jump -{bars} bars"), 1.0);
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.jump_fwd_bars",
            title: "Jump forward N bars",
            group: "PLAYBACK",
            keys: &[">"],
            run: |app| {
                let bars = app.config.jump_bars as i32;
                app.engine.jump(bars);
                app.toast.show(&format!("Jump +{bars} bars"), 1.0);
            },
            when: Some(no_modal_capture),
        },
        // Open the `:` command prompt overlay.
        Command {
            id: "prompt.command",
            title: "Open command prompt (vim-style :)",
            group: "APP",
            keys: &[":"],
            run: |app| {
                app.command_prompt = Some(String::new());
            },
            when: Some(no_modal_capture),
        },
        // Toggle Claude DJ.
        Command {
            id: "claude.toggle",
            title: "Toggle Claude DJ on/off",
            group: "APP",
            keys: &["C"],
            run: |app| {
                app.toggle_claude_dj();
            },
            when: Some(no_modal_capture),
        },
        // Cycle the analyzer engine (built-in ⇄ stratum) and
        // re-analyze the playing deck in-place. Used to A/B detectors
        // on a bad mix.
        Command {
            id: "engine.cycle_analyzer",
            title: "Toggle analyzer engine + re-grid",
            group: "PLAYBACK",
            keys: &["G"],
            run: |app| {
                use crate::config::AnalyzerEngine;
                app.config.analyzer_engine = match app.config.analyzer_engine {
                    AnalyzerEngine::Builtin => AnalyzerEngine::Stratum,
                    AnalyzerEngine::Stratum => AnalyzerEngine::Builtin,
                };
                app.config.save();
                let label = match app.config.analyzer_engine {
                    AnalyzerEngine::Builtin => "built-in",
                    AnalyzerEngine::Stratum => "stratum",
                };
                let fallback_note = if matches!(app.config.analyzer_engine, AnalyzerEngine::Stratum)
                    && !cfg!(feature = "stratum")
                {
                    " (not compiled — using built-in)"
                } else {
                    ""
                };
                match app.engine.reanalyze_playing(app.config.analyzer_engine) {
                    Some(bpm) => app.toast.show(
                        &format!("Engine: {label}{fallback_note} — re-gridded @ {bpm:.1} BPM"),
                        3.0,
                    ),
                    None => app.toast.show(
                        &format!("Engine: {label}{fallback_note} (no track loaded)"),
                        2.0,
                    ),
                }
            },
            when: Some(no_modal_capture),
        },
    ]
}
