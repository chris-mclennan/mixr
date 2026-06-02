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
    let id = match app.keymap.resolve(key) {
        Some(id) => id.to_string(),
        None => return false,
    };
    // `registry()` is `'static` so resolving the command doesn't borrow
    // anything from `app`. `when` is `Copy` (it's a fn pointer), so we
    // can lift it out before borrowing `app` again to invoke it.
    let Some(cmd) = registry().get(&id) else {
        return false;
    };
    let when = cmd.when;
    let run = cmd.run;
    if let Some(w) = when
        && !w(app)
    {
        return false;
    }
    run(app);
    true
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
            keys: &[],
            run: |_app| { /* TODO: migrate from keys.rs */ },
            when: None,
        },
        Command {
            id: "view.browse",
            title: "Browse library",
            group: "VIEWS",
            keys: &[],
            run: |_app| { /* TODO: migrate from keys.rs */ },
            when: None,
        },
        Command {
            id: "view.history",
            title: "Play history",
            group: "VIEWS",
            keys: &[],
            run: |_app| { /* TODO: migrate from keys.rs */ },
            when: None,
        },
        Command {
            id: "view.settings",
            title: "Settings",
            group: "VIEWS",
            keys: &[],
            run: |_app| { /* TODO: migrate from keys.rs */ },
            when: None,
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
        // First fully-migrated command. Toggles the dashboard's inline
        // help legend. Active only when on Dashboard and no modal is
        // capturing input — the legacy `KeyCode::Char('?')` arm at
        // `keys.rs` would have been gated on the same conditions.
        Command {
            id: "view.help",
            title: "Toggle dashboard help legend",
            group: "VIEWS",
            keys: &["?"],
            run: |app| {
                app.dash_help = !app.dash_help;
            },
            when: Some(|app| {
                use super::app::ViewMode;
                matches!(app.view_mode, ViewMode::Dashboard)
                    && !app.dash_fav_picker
                    && !app.dj_asking
                    && !app.filtering
                    && app.command_prompt.is_none()
                    && app.pending_midi_map.is_none()
                    && app.pending_confirm.is_none()
            }),
        },
    ]
}
