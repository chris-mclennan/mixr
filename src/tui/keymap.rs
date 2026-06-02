//! Key-spec parsing + the config-driven keymap resolver.
//!
//! Mirrors mnml's `src/input/keymap.rs` — same chord normalization, same
//! `parse_key_spec` grammar. Adapted for raw `crossterm` (mixr uses
//! crossterm directly; mnml uses `ratatui::crossterm`).
//!
//! [`Keymap`] is the *one table* app-level chords resolve through: built
//! from every [`crate::tui::command::Command`]'s default `keys` then
//! overlaid with user `[keys.global]` config.

// Scaffolding for #59 — `resolve` isn't called from `handle_key` yet.
// The dead-code warnings lift as bindings migrate per
// `docs/COMMAND_MIGRATION.md`.
#![allow(dead_code)]

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::AppConfig;

/// Normalized `(code, modifiers)` pair. An uppercase `Char` is lowered
/// with SHIFT made explicit, so `"P"` and `"shift+p"` collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub fn of(ev: &KeyEvent) -> Chord {
        let mut mods = ev.modifiers;
        let code = match ev.code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_ascii_lowercase())
            }
            other => other,
        };
        Chord { code, mods }
    }

    /// Pretty-print as a key spec (`ctrl+c`, `enter`, `f1`, …).
    // `Chord` is Copy + small; clippy wants `self`-by-value but
    // `chord.to_spec()` reads better at call sites.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_spec(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            parts.push("ctrl");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            parts.push("alt");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            parts.push("shift");
        }
        let name = match self.code {
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Tab => "tab".to_string(),
            KeyCode::BackTab => "backtab".to_string(),
            KeyCode::Esc => "esc".to_string(),
            KeyCode::Backspace => "backspace".to_string(),
            KeyCode::Delete => "delete".to_string(),
            KeyCode::Insert => "insert".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::Left => "left".to_string(),
            KeyCode::Right => "right".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            KeyCode::PageUp => "pageup".to_string(),
            KeyCode::PageDown => "pagedown".to_string(),
            KeyCode::F(n) => format!("f{n}"),
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            other => format!("{other:?}"),
        };
        parts.push(&name);
        parts.join("+")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    map: HashMap<Chord, String>,
}

impl Keymap {
    /// Defaults from [`crate::tui::command::registry`], then `[keys.global]`
    /// from `~/.mixr/config.json` (`""` / `"none"` / `"unbound"` removes).
    pub fn build(cfg: &AppConfig) -> Keymap {
        let mut km = Keymap::default();
        for cmd in crate::tui::command::registry().all() {
            for spec in cmd.keys {
                if let Some(ev) = parse_key_spec(spec) {
                    km.map.insert(Chord::of(&ev), cmd.id.to_string());
                }
            }
        }
        if let Some(table) = cfg.keys.get("global") {
            for (key, id) in table {
                let Some(ev) = parse_key_spec(key) else {
                    eprintln!("mixr: [keys.global] bad key spec {key:?} — ignored");
                    continue;
                };
                let chord = Chord::of(&ev);
                let id = id.trim();
                if id.is_empty() || id == "none" || id == "unbound" {
                    km.map.remove(&chord);
                } else {
                    km.map.insert(chord, id.to_string());
                }
            }
        }
        km
    }

    pub fn resolve(&self, ev: &KeyEvent) -> Option<&str> {
        self.map.get(&Chord::of(ev)).map(String::as_str)
    }

    pub fn binding_count(&self) -> usize {
        self.map.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Chord, &str)> {
        self.map.iter().map(|(c, s)| (c, s.as_str()))
    }

    pub fn bind(&mut self, spec: &str, id: &str) {
        if let Some(ev) = parse_key_spec(spec) {
            self.map.insert(Chord::of(&ev), id.to_string());
        }
    }
}

pub fn parse_key_spec(spec: &str) -> Option<KeyEvent> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let mut mods = KeyModifiers::NONE;
    let mut rest = spec;
    loop {
        let lower = rest.to_ascii_lowercase();
        if let Some(r) = lower
            .strip_prefix("ctrl+")
            .or_else(|| lower.strip_prefix("c-"))
        {
            mods |= KeyModifiers::CONTROL;
            rest = &rest[rest.len() - r.len()..];
        } else if let Some(r) = lower
            .strip_prefix("shift+")
            .or_else(|| lower.strip_prefix("s-"))
        {
            mods |= KeyModifiers::SHIFT;
            rest = &rest[rest.len() - r.len()..];
        } else if let Some(r) = lower
            .strip_prefix("alt+")
            .or_else(|| lower.strip_prefix("a-"))
            .or_else(|| lower.strip_prefix("meta+"))
        {
            mods |= KeyModifiers::ALT;
            rest = &rest[rest.len() - r.len()..];
        } else {
            break;
        }
    }
    let code = key_code(rest)?;
    Some(KeyEvent::new(code, mods))
}

fn key_code(token: &str) -> Option<KeyCode> {
    let t = token.to_ascii_lowercase();
    Some(match t.as_str() {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        s if s.starts_with('f') && s[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = s[1..].parse().ok()?;
            if (1..=12).contains(&n) {
                KeyCode::F(n)
            } else {
                return None;
            }
        }
        _ => {
            let mut chars = token.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modified_and_named() {
        let e = parse_key_spec("ctrl+c").unwrap();
        assert_eq!(e.code, KeyCode::Char('c'));
        assert!(e.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(parse_key_spec("enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_key_spec("?").unwrap().code, KeyCode::Char('?'));
        assert!(parse_key_spec("nope-not-a-key").is_none());
    }

    #[test]
    fn chord_normalizes_uppercase_char() {
        let a = Chord::of(&KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
        let b = Chord::of(&KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT));
        assert_eq!(a, b);
    }

    #[test]
    fn default_keymap_resolves_migrated_chords() {
        // Only commands with non-empty `keys` end up in the keymap.
        // As bindings migrate out of `keys.rs` (see
        // `docs/COMMAND_MIGRATION.md`), more chords land here.
        let km = Keymap::build(&AppConfig::default());
        let ev = |s: &str| parse_key_spec(s).unwrap();
        assert_eq!(km.resolve(&ev("?")), Some("view.help"));
        assert_eq!(km.resolve(&ev("d")), Some("view.dashboard"));
        assert_eq!(km.resolve(&ev("b")), Some("view.browse"));
        assert_eq!(km.resolve(&ev("h")), Some("view.history"));
        assert_eq!(km.resolve(&ev(",")), Some("view.settings"));
        assert_eq!(km.resolve(&ev("q")), Some("view.queue"));
        assert_eq!(km.resolve(&ev("p")), Some("engine.pause"));
        assert_eq!(km.resolve(&ev("m")), Some("engine.mix_now"));
    }
}
