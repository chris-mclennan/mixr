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
            // `d` ON Dashboard goes to Browse (dash.toggle_d below) —
            // this command only fires elsewhere.
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Dashboard `d`: switch to Browse view (opposite of the
        // global `d` which goes to Dashboard).
        Command {
            id: "dash.toggle_d",
            title: "Dashboard d: jump to Browse view",
            group: "VIEWS",
            keys: &["d"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Browse;
            },
            when: Some(dashboard_normal),
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
            // Non-Dashboard guard — Dashboard `b` is focus-aware (see
            // dash.focus_browse below).
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Dashboard `b`: focus-aware. If already on the mini-browse
        // panel, switch to full Browse view; otherwise just move
        // focus to the mini-browse panel.
        Command {
            id: "dash.focus_browse",
            title: "Dashboard: focus mini-browse (or switch to Browse)",
            group: "VIEWS",
            keys: &["b"],
            run: |app| {
                use super::app::{DashFocus, ViewMode};
                if app.dash_focus == DashFocus::Browse {
                    app.view_mode = ViewMode::Browse;
                    app.selected = 0;
                } else {
                    app.dash_focus = DashFocus::Browse;
                }
            },
            when: Some(dashboard_normal),
        },
        // Dashboard `z`/`Z`: open the virtual mixer overlay.
        Command {
            id: "view.mixer",
            title: "Virtual mixer overlay (Dashboard)",
            group: "VIEWS",
            keys: &["z", "Z"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Mixer;
            },
            when: Some(dashboard_normal),
        },
        // Dashboard `A`: AI analyze the current mix alignment. Spawns
        // an async task that sends results back via action_tx.
        Command {
            id: "engine.ai_analyze",
            title: "AI analyze mix alignment",
            group: "PLAYBACK",
            keys: &["A"],
            run: |app| {
                use super::app::AppAction;
                if let Some(data) = app.engine.alignment_peaks() {
                    app.toast.show("AI analyzing mix alignment...", 2.0);
                    let tx = app.action_tx.clone();
                    tokio::spawn(async move {
                        match crate::audio::ai_beat::analyze_mix_alignment(
                            &data.playing_peaks,
                            &data.incoming_peaks,
                            data.playing_bpm,
                            data.incoming_bpm,
                        )
                        .await
                        {
                            Ok(a) => {
                                tx.send(AppAction::AlignmentResult {
                                    nudge_ms: a.nudge_ms,
                                    is_aligned: a.is_aligned,
                                    rate_correction: a.rate_correction,
                                    details: a.details,
                                })
                                .ok();
                            }
                            Err(e) => {
                                tx.send(AppAction::Toast(format!("Alignment error: {e}")))
                                    .ok();
                            }
                        }
                    });
                } else {
                    app.toast.show("Not crossfading", 1.0);
                }
            },
            when: Some(dashboard_normal),
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
        // Rate a past mix in the History view — `+`/`-` operate on the
        // highlighted history entry instead of the dashboard's
        // most-recent-mix rating. Same chord, history-specific `when`.
        Command {
            id: "history.rate_good",
            title: "Rate selected history entry: good",
            group: "QUEUE",
            keys: &["+", "="],
            run: |app| {
                let sel = app.selected;
                app.rate_history_entry(sel, true);
            },
            when: Some(|app| {
                use super::app::ViewMode;
                matches!(app.view_mode, ViewMode::History) && no_modal_capture(app)
            }),
        },
        Command {
            id: "history.rate_bad",
            title: "Rate selected history entry: bad",
            group: "QUEUE",
            keys: &["-", "_"],
            run: |app| {
                let sel = app.selected;
                app.rate_history_entry(sel, false);
            },
            when: Some(|app| {
                use super::app::ViewMode;
                matches!(app.view_mode, ViewMode::History) && no_modal_capture(app)
            }),
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
        // Clear the queue (with Y/N confirm).
        Command {
            id: "engine.clear_queue",
            title: "Clear queue (with confirmation)",
            group: "QUEUE",
            keys: &["X"],
            run: |app| {
                let n = app.engine.queue.len();
                if n == 0 {
                    app.toast.show("Queue already empty", 0.6);
                } else {
                    app.pending_confirm = Some(super::app::ConfirmAction::ClearQueue);
                    app.toast.show(
                        &format!(
                            "Clear queue ({n} track{})? Y/N",
                            if n == 1 { "" } else { "s" }
                        ),
                        5.0,
                    );
                }
            },
            when: Some(no_modal_capture),
        },
        // Queue grab — moves into Queue view if not there. `{` grabs
        // / drops the highlighted entry; `}` finishes the move.
        Command {
            id: "queue.grab",
            title: "Grab / drop queue entry (reorder)",
            group: "QUEUE",
            keys: &["{"],
            run: |app| {
                use super::app::ViewMode;
                if !matches!(app.view_mode, ViewMode::Queue) {
                    app.view_mode = ViewMode::Queue;
                    app.selected = 0;
                }
                if app.queue_grab_index.is_some() {
                    app.queue_grab_index = None;
                    app.toast.show("Dropped", 0.5);
                } else {
                    app.queue_grab_index = Some(app.selected);
                    app.toast
                        .show("Grabbed — move with ↑↓, press } to drop", 2.0);
                }
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "queue.drop",
            title: "Drop grabbed queue entry at cursor",
            group: "QUEUE",
            keys: &["}"],
            run: |app| {
                if let Some(from) = app.queue_grab_index.take() {
                    let to = app.selected;
                    app.engine.move_queue_item(from, to);
                    app.toast.show("Moved", 0.5);
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
        // Hot cues on the currently-playing deck — 1..4 jump, !@#$
        // set. 8 separate Commands (one per chord) because the
        // `keys` field is a static slice; pattern-matching ranges
        // (`'1'..='4'`) can't be expressed in the registry directly.
        Command {
            id: "engine.cue_jump_1",
            title: "Jump to hot cue 1",
            group: "PLAYBACK",
            keys: &["1"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_jump(is_a, 0);
                app.toast.show(
                    &format!("Cue 1 jump ({})", super::app::App::deck_label(is_a)),
                    0.5,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_jump_2",
            title: "Jump to hot cue 2",
            group: "PLAYBACK",
            keys: &["2"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_jump(is_a, 1);
                app.toast.show(
                    &format!("Cue 2 jump ({})", super::app::App::deck_label(is_a)),
                    0.5,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_jump_3",
            title: "Jump to hot cue 3",
            group: "PLAYBACK",
            keys: &["3"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_jump(is_a, 2);
                app.toast.show(
                    &format!("Cue 3 jump ({})", super::app::App::deck_label(is_a)),
                    0.5,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_jump_4",
            title: "Jump to hot cue 4",
            group: "PLAYBACK",
            keys: &["4"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_jump(is_a, 3);
                app.toast.show(
                    &format!("Cue 4 jump ({})", super::app::App::deck_label(is_a)),
                    0.5,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_set_1",
            title: "Set hot cue 1 at current position",
            group: "PLAYBACK",
            keys: &["!"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_set(is_a, 0);
                app.toast.show(
                    &format!("Cue 1 set ({})", super::app::App::deck_label(is_a)),
                    0.8,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_set_2",
            title: "Set hot cue 2 at current position",
            group: "PLAYBACK",
            keys: &["@"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_set(is_a, 1);
                app.toast.show(
                    &format!("Cue 2 set ({})", super::app::App::deck_label(is_a)),
                    0.8,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_set_3",
            title: "Set hot cue 3 at current position",
            group: "PLAYBACK",
            keys: &["#"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_set(is_a, 2);
                app.toast.show(
                    &format!("Cue 3 set ({})", super::app::App::deck_label(is_a)),
                    0.8,
                );
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.cue_set_4",
            title: "Set hot cue 4 at current position",
            group: "PLAYBACK",
            keys: &["$"],
            run: |app| {
                let is_a = app.cached_info.playing_is_a;
                app.engine.cue_set(is_a, 3);
                app.toast.show(
                    &format!("Cue 4 set ({})", super::app::App::deck_label(is_a)),
                    0.8,
                );
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
        // Nudge incoming deck — `[` left, `]` right (hold to keep
        // nudging). Returns the cumulative shift if a beat is captured.
        Command {
            id: "engine.nudge_left",
            title: "Nudge incoming ← (hold to keep nudging)",
            group: "PLAYBACK",
            keys: &["["],
            run: |app| {
                if let Some((shift, pos)) = app.engine.nudge(-1) {
                    app.toast
                        .show(&format!("{shift:+.0}ms  beat@{:.0}ms", pos * 1000.0), 1.0);
                } else {
                    app.toast.show("Nudge ◀", 0.5);
                }
            },
            when: Some(no_modal_capture),
        },
        Command {
            id: "engine.nudge_right",
            title: "Nudge incoming → (hold to keep nudging)",
            group: "PLAYBACK",
            keys: &["]"],
            run: |app| {
                if let Some((shift, pos)) = app.engine.nudge(1) {
                    app.toast
                        .show(&format!("{shift:+.0}ms  beat@{:.0}ms", pos * 1000.0), 1.0);
                } else {
                    app.toast.show("Nudge ▶", 0.5);
                }
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
        // Space: toggle track preview (4 bars from first beat with
        // metronome) on the highlighted Browse track.
        Command {
            id: "engine.preview_track",
            title: "Preview track (4-bar metronome)",
            group: "PLAYBACK",
            keys: &["space"],
            run: |app| {
                use super::app::ViewMode;
                if app.engine.is_previewing() {
                    app.engine.stop_preview();
                    app.toast.show("Preview stopped", 1.0);
                } else if matches!(app.view_mode, ViewMode::Browse)
                    && let Some(track) = app.current_screen().track_at(app.selected).cloned()
                {
                    app.download_for_preview(track);
                }
            },
            when: Some(no_modal_capture),
        },
        // Queue all tracks on the current screen (skips duplicates
        // already queued or loaded on a deck).
        Command {
            id: "engine.queue_all",
            title: "Queue all tracks on screen",
            group: "QUEUE",
            keys: &["a"],
            run: |app| {
                use crate::audio::engine::QueueEntry;
                if let Some(tracks) = app.current_screen().tracks() {
                    let total = tracks.len();
                    let mut added = 0;
                    #[allow(clippy::unnecessary_to_owned)]
                    for track in tracks.to_vec() {
                        if app.engine.enqueue(QueueEntry::from(track)) {
                            added += 1;
                        }
                    }
                    let msg = match (added, total - added) {
                        (0, _) => "All tracks already queued".to_string(),
                        (a, 0) => format!("Queued {a} tracks"),
                        (a, skipped) => {
                            format!("Queued {a}, skipped {skipped} duplicates")
                        }
                    };
                    app.toast.show(&msg, 1.5);
                }
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
        // Dashboard `/`: ask Claude DJ if enabled, otherwise switch to
        // Search view. Different from the global `/`/`s` below — same
        // chord, different `when`.
        Command {
            id: "dash.slash",
            title: "Ask Claude DJ (dashboard /) or Search",
            group: "VIEWS",
            keys: &["/"],
            run: |app| {
                let dj_on = app.claude_dj.is_some() && app.config.claude_dj_enabled;
                if dj_on {
                    app.dj_asking = true;
                    app.dj_ask_buffer.clear();
                    app.toast.show("Ask Claude DJ...", 1.0);
                } else {
                    app.view_mode = super::app::ViewMode::Search;
                    app.search_query.clear();
                    app.search_results.clear();
                    app.selected = 0;
                }
            },
            when: Some(dashboard_normal),
        },
        // Global search shortcut — `/` or `s` from any non-Dashboard
        // view. Switches to Search and clears state.
        Command {
            id: "view.search",
            title: "Search Beatport",
            group: "VIEWS",
            keys: &["/", "s"],
            run: |app| {
                app.view_mode = super::app::ViewMode::Search;
                app.search_query.clear();
                app.search_results.clear();
                app.selected = 0;
            },
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Open URL under the column-aware browse cursor in the OS
        // default browser.
        Command {
            id: "browse.open_in_browser",
            title: "Open in browser (column-aware)",
            group: "BROWSING",
            keys: &["o"],
            run: |app| {
                app.open_in_browser();
            },
            when: Some(no_modal_capture),
        },
        // Open the full Claude DJ screen.
        Command {
            id: "view.claude_dj",
            title: "Claude DJ screen (log scrollback + state)",
            group: "VIEWS",
            keys: &["c"],
            run: |app| {
                app.view_mode = super::app::ViewMode::ClaudeDj;
                app.scroll_offset = 0;
            },
            when: Some(no_modal_capture),
        },
        // Dashboard arrows — focus-aware (Controller cycles sections,
        // Browse moves cursor, Log scrolls history).
        Command {
            id: "dash.up",
            title: "Dashboard ↑",
            group: "VIEWS",
            keys: &["up"],
            run: |app| {
                use super::app::DashFocus;
                if app.dash_focus == DashFocus::Controller {
                    app.dash_section = app.dash_section.prev();
                } else if app.dash_focus == DashFocus::Browse && app.dash_browse_sel > 0 {
                    app.dash_browse_sel -= 1;
                } else if app.dash_focus == DashFocus::Log {
                    app.log_scroll_offset = (app.log_scroll_offset + 1).min(1000);
                }
            },
            when: Some(dashboard_normal),
        },
        Command {
            id: "dash.down",
            title: "Dashboard ↓",
            group: "VIEWS",
            keys: &["down"],
            run: |app| {
                use super::app::DashFocus;
                if app.dash_focus == DashFocus::Controller {
                    app.dash_section = app.dash_section.next();
                } else if app.dash_focus == DashFocus::Browse {
                    let count = app.current_screen().item_count();
                    if app.dash_browse_sel + 1 < count.min(8) {
                        app.dash_browse_sel += 1;
                    }
                } else if app.dash_focus == DashFocus::Log && app.log_scroll_offset > 0 {
                    app.log_scroll_offset -= 1;
                }
            },
            when: Some(dashboard_normal),
        },
        Command {
            id: "dash.right",
            title: "Dashboard Enter / →",
            group: "VIEWS",
            keys: &["enter", "right"],
            run: |app| {
                use super::app::DashFocus;
                if app.dash_focus == DashFocus::Controller {
                    app.handle_deck_control(1);
                } else if app.dash_focus == DashFocus::Browse {
                    app.selected = app.dash_browse_sel;
                    app.handle_browse_enter();
                    app.dash_browse_sel = 0;
                }
            },
            when: Some(dashboard_normal),
        },
        Command {
            id: "dash.left",
            title: "Dashboard ←",
            group: "VIEWS",
            keys: &["left"],
            run: |app| {
                use super::app::DashFocus;
                if app.dash_focus == DashFocus::Controller {
                    app.handle_deck_control(-1);
                } else if app.dash_focus == DashFocus::Browse && app.screen_stack.len() > 1 {
                    app.pop_screen();
                }
            },
            when: Some(dashboard_normal),
        },
        // Dashboard `Esc`: context-dependent.
        //   waveform_zoom set    → clear it
        //   browse focus + drill → pop_screen (restore selection)
        //   otherwise            → switch to Browse view
        Command {
            id: "dash.escape",
            title: "Dashboard Esc: zoom out / pop browse / Browse",
            group: "VIEWS",
            keys: &["esc"],
            run: |app| {
                use super::app::{DashFocus, ViewMode};
                if app.waveform_zoom.is_some() {
                    app.waveform_zoom = None;
                } else if app.dash_focus == DashFocus::Browse && app.screen_stack.len() > 1 {
                    app.pop_screen();
                } else {
                    app.view_mode = ViewMode::Browse;
                }
            },
            when: Some(dashboard_normal),
        },
        // Dashboard `L`: play-next the mini-browse-highlighted track
        // (focus-aware). Routes through engine.play_next which
        // chooses StartedFresh / LoadedAsIncoming / ReplacedIncoming /
        // QueuedAtFront based on the current mix state.
        Command {
            id: "dash.play_next",
            title: "Load next from mini-browse",
            group: "BROWSING",
            keys: &["L"],
            run: |app| {
                use super::app::DashFocus;
                use crate::audio::engine::PlayNextOutcome;
                if app.dash_focus == DashFocus::Browse
                    && let Some(track) = app.current_screen().track_at(app.dash_browse_sel).cloned()
                {
                    let name = format!("{} - {}", track.artist_name(), track.full_title());
                    let outcome = app.engine.play_next(track);
                    let msg = match outcome {
                        PlayNextOutcome::StartedFresh => format!("Playing next: {name}"),
                        PlayNextOutcome::LoadedAsIncoming => {
                            format!("Loaded as incoming: {name}")
                        }
                        PlayNextOutcome::ReplacedIncoming => {
                            format!("Replaced incoming with {name} (prev moved to queue)")
                        }
                        PlayNextOutcome::QueuedAtFront => format!("Queued next: {name}"),
                    };
                    app.toast.show(&msg, 2.0);
                }
            },
            when: Some(dashboard_normal),
        },
        // Dashboard `&`: add the mini-browse-highlighted track to the
        // Beatport cart (only when browse-focused). Mirrors the global
        // `&` in spirit but acts on dash_browse_sel.
        Command {
            id: "dash.add_to_cart",
            title: "Dashboard: add track to Beatport cart",
            group: "BROWSING",
            keys: &["&"],
            run: |app| {
                use super::app::DashFocus;
                if app.dash_focus == DashFocus::Browse
                    && let Some(track) = app.current_screen().track_at(app.dash_browse_sel)
                {
                    app.add_track_to_cart(track.clone());
                }
            },
            when: Some(dashboard_normal),
        },
        // Dashboard `f`/`*`: focus-aware favorite. On mini-browse,
        // favorite the highlighted track; otherwise favorite the
        // playing deck (with a picker if both decks are loaded).
        Command {
            id: "dash.favorite",
            title: "Dashboard: favorite (track / deck / picker)",
            group: "BROWSING",
            keys: &["f", "*"],
            run: |app| {
                use super::app::DashFocus;
                if app.dash_focus == DashFocus::Browse {
                    if let Some(track) = app.current_screen().track_at(app.dash_browse_sel) {
                        let track = track.clone();
                        app.toggle_favorite_track(track);
                    }
                } else {
                    let a_loaded = app.cached_info.deck_a_track.is_some();
                    let b_loaded = app.cached_info.deck_b_track.is_some();
                    match (a_loaded, b_loaded) {
                        (true, true) => {
                            app.dash_fav_picker = true;
                        }
                        (true, false) => app.toggle_favorite_deck(true),
                        (false, true) => app.toggle_favorite_deck(false),
                        (false, false) => app.toast.show("Nothing to favorite", 1.0),
                    }
                }
            },
            when: Some(dashboard_normal),
        },
        // Toggle favorite on the highlighted track (Browse view).
        // Dashboard `f`/`*` has different focus-aware behavior — that
        // arm is not yet migrated; it lives in keys.rs:739.
        Command {
            id: "favorites.toggle_track",
            title: "Toggle favorite on highlighted track",
            group: "BROWSING",
            keys: &["f", "*"],
            run: |app| {
                use crate::beatport::catalog::BrowseScreen;
                let track = match app.current_screen() {
                    BrowseScreen::TrackList { tracks, .. } => tracks.get(app.selected).cloned(),
                    _ => None,
                };
                if let Some(track) = track {
                    let added = app.favorites.toggle(&track);
                    let name = format!("{} - {}", track.artist_name(), track.full_title());
                    let msg = if added {
                        format!("★ {name}")
                    } else {
                        format!("Unfavorited: {name}")
                    };
                    app.toast.show(&msg, 1.5);
                }
            },
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Add the highlighted track to the Beatport cart for later
        // purchase. Browse-state, non-Dashboard.
        Command {
            id: "browse.add_to_cart",
            title: "Add track to Beatport cart",
            group: "BROWSING",
            keys: &["&"],
            run: |app| {
                use crate::beatport::catalog::BrowseScreen;
                let track = match app.current_screen() {
                    BrowseScreen::TrackList { tracks, .. } => tracks.get(app.selected).cloned(),
                    _ => None,
                };
                if let Some(track) = track {
                    app.add_track_to_cart(track);
                }
            },
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Copy the current screen dump (~/.mixr/screen.txt) to the
        // OS clipboard via `pbcopy`.
        Command {
            id: "app.copy_screen",
            title: "Copy screen dump to clipboard",
            group: "APP",
            keys: &["y"],
            run: |app| {
                let path = dirs::home_dir()
                    .unwrap_or_default()
                    .join(".mixr/screen.txt");
                if let Ok(content) = std::fs::read_to_string(&path) {
                    match std::process::Command::new("pbcopy")
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                    {
                        Ok(mut child) => {
                            if let Some(ref mut stdin) = child.stdin {
                                use std::io::Write;
                                stdin.write_all(content.as_bytes()).ok();
                            }
                            child.wait().ok();
                            app.toast.show("Screen copied to clipboard", 1.0);
                        }
                        Err(_) => app.toast.show("Failed to copy", 1.0),
                    }
                }
            },
            when: Some(no_modal_capture),
        },
        // No-op "favorites sync" — favorites are metadata-only on
        // main. The chord shows a hint toast.
        Command {
            id: "favorites.sync_hint",
            title: "Favorites sync (metadata-only — no-op hint)",
            group: "BROWSING",
            keys: &["r"],
            run: |app| {
                app.toast
                    .show("Favorites are metadata-only on main — no sync needed", 2.5);
            },
            when: Some(no_modal_capture),
        },
        // Load more (pagination) — fires on Browse view when a
        // last_load_action is captured.
        Command {
            id: "browse.load_more",
            title: "Load more (pagination)",
            group: "BROWSING",
            keys: &["L"],
            run: |app| {
                use super::app::ViewMode;
                if matches!(app.view_mode, ViewMode::Browse)
                    && let Some(action) = app.last_load_action.clone()
                {
                    app.load_more(&action);
                }
            },
            when: Some(no_modal_capture),
        },
        // Add the highlighted track to a playlist (opens picker).
        // Dashboard `+` is rate-mix-good — guarded against here.
        Command {
            id: "playlist.add_track",
            title: "Add track to playlist (opens picker)",
            group: "BROWSING",
            keys: &["+"],
            run: |app| {
                if let Some(track) = app.current_screen().track_at(app.selected) {
                    let track_id = track.id;
                    app.open_playlist_picker(track_id);
                }
            },
            when: Some(|app| {
                use super::app::ViewMode;
                !matches!(app.view_mode, ViewMode::Dashboard) && no_modal_capture(app)
            }),
        },
        // Start the local browse filter (Ctrl+F or Shift+F).
        Command {
            id: "browse.start_filter",
            title: "Local filter (Ctrl+F or Shift+F)",
            group: "BROWSING",
            keys: &["ctrl+f", "F"],
            run: |app| {
                use super::app::ViewMode;
                if matches!(app.view_mode, ViewMode::Browse) {
                    app.filtering = true;
                    app.filter_text.clear();
                    app.selected = 0;
                    app.toast.show("Filter: type to filter", 1.5);
                }
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
