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

use std::collections::HashMap;
use std::sync::OnceLock;

use super::app::App;

pub type CommandFn = fn(&mut App);

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
pub fn run(id: &str, app: &mut App) -> bool {
    if let Some(cmd) = registry().get(id) {
        (cmd.run)(app);
        true
    } else {
        false
    }
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
        Command {
            id: "view.dashboard",
            title: "Dashboard (live mix view)",
            group: "VIEWS",
            keys: &["d"],
            run: |_app| { /* TODO: migrate from keys.rs */ },
        },
        Command {
            id: "view.browse",
            title: "Browse library",
            group: "VIEWS",
            keys: &["b"],
            run: |_app| { /* TODO: migrate from keys.rs */ },
        },
        Command {
            id: "view.history",
            title: "Play history",
            group: "VIEWS",
            keys: &["h"],
            run: |_app| { /* TODO: migrate from keys.rs */ },
        },
        Command {
            id: "view.settings",
            title: "Settings",
            group: "VIEWS",
            keys: &[","],
            run: |_app| { /* TODO: migrate from keys.rs */ },
        },
        Command {
            id: "view.help",
            title: "This help",
            group: "VIEWS",
            keys: &["?"],
            run: |_app| { /* TODO: migrate from keys.rs */ },
        },
        Command {
            id: "app.quit",
            title: "Quit mixr",
            group: "APP",
            keys: &["ctrl+c"],
            run: |_app| { /* TODO: migrate from keys.rs */ },
        },
    ]
}
