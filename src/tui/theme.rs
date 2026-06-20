//! Family theme — mixr follows mnml's colours.
//!
//! mnml writes its resolved active theme to `~/.config/mnml/current-theme.toml`
//! (see mnml's `docs/THEMING.md`). mixr reads that file and projects its
//! `[base_30]` onto a small [`Palette`] of packed-rgba `u32`s (the same format
//! the blit frame encoder emits). The blit `color_to_rgba` remap consults this
//! so mixr's dim/secondary text + semantic colours match whatever theme mnml is
//! on — and retint live when mnml switches theme (mtime poll, once per tick).
//!
//! No dependency on the wire `Message::Palette` (which carries only 3 colours):
//! the file carries the full set and works whether mixr is hosted or standalone.

use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

use tmnl_protocol::pack_rgba_u8;

/// mnml's family palette, as packed-rgba `u32`s. One field per role mixr's
/// renderer needs; `color_to_rgba` maps ratatui `Color`s onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: u32,
    pub fg: u32,
    pub dim: u32,
    pub accent: u32,
    pub red: u32,
    pub green: u32,
    pub yellow: u32,
    pub blue: u32,
    pub cyan: u32,
    pub purple: u32,
    pub orange: u32,
}

impl Palette {
    /// mixr's historical hardcoded colours — the fallback for any `[base_30]`
    /// key missing from the theme file. Matches the legacy `color_to_rgba`
    /// constants so a partial theme degrades gracefully.
    fn defaults() -> Palette {
        Palette {
            bg: pack_rgba_u8(0x10, 0x11, 0x1c, 0xff),
            fg: pack_rgba_u8(0xab, 0xb2, 0xbf, 0xff),
            dim: pack_rgba_u8(0x42, 0x46, 0x4e, 0xff),
            accent: pack_rgba_u8(0x6e, 0xa2, 0xe7, 0xff),
            red: pack_rgba_u8(0xe0, 0x60, 0x60, 0xff),
            green: pack_rgba_u8(0x84, 0xc8, 0x6f, 0xff),
            yellow: pack_rgba_u8(0xee, 0xbb, 0x57, 0xff),
            blue: pack_rgba_u8(0x6e, 0xa2, 0xe7, 0xff),
            cyan: pack_rgba_u8(0x5f, 0xb3, 0xa1, 0xff),
            purple: pack_rgba_u8(0xc9, 0x7a, 0xea, 0xff),
            orange: pack_rgba_u8(0xfc, 0xa2, 0xaa, 0xff),
        }
    }
}

/// `~/.config/mnml/current-theme.toml` — mnml's canonical theme file. Mirrors
/// mnml's own path logic exactly ( `$XDG_CONFIG_HOME` else `$HOME/.config` —
/// **not** `dirs::config_dir()`, which is `~/Library/...` on macOS).
fn theme_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("mnml").join("current-theme.toml"));
    }
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join(".config")
            .join("mnml")
            .join("current-theme.toml")
    })
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let h = |a: usize| u8::from_str_radix(&s[a..a + 2], 16).ok();
    Some((h(0)?, h(2)?, h(4)?))
}

/// Project the `[base_30]` table of an mnml theme file onto a [`Palette`].
/// Tolerant: only `[base_30]` rows are read, missing keys fall back to
/// [`Palette::defaults`], unknown keys + comments are ignored.
fn project(src: &str) -> Palette {
    let mut m = std::collections::HashMap::new();
    let mut in_base30 = false;
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_base30 = line == "[base_30]";
            continue;
        }
        if !in_base30 {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && let Some(rgb) = parse_hex(v.trim().trim_matches('"'))
        {
            m.insert(k.trim().to_string(), rgb);
        }
    }
    let d = Palette::defaults();
    let pick = |key: &str, fallback: u32| {
        m.get(key)
            .map(|&(r, g, b)| pack_rgba_u8(r, g, b, 0xff))
            .unwrap_or(fallback)
    };
    Palette {
        bg: pick("one_bg", d.bg),
        fg: pick("white", d.fg),
        // The dim role — what was "too dark". mnml emits it as light_grey.
        dim: pick("light_grey", d.dim),
        accent: pick("blue", d.accent),
        red: pick("red", d.red),
        green: pick("green", d.green),
        yellow: pick("yellow", d.yellow),
        blue: pick("blue", d.blue),
        cyan: pick("cyan", d.cyan),
        purple: pick("purple", d.purple),
        orange: pick("orange", d.orange),
    }
}

struct State {
    palette: Option<Palette>,
    mtime: Option<SystemTime>,
}

fn state() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        RwLock::new(State {
            palette: None,
            mtime: None,
        })
    })
}

/// The current family palette, or `None` when mnml's theme file isn't present
/// (mixr then keeps its own colours). Cheap: a clone of the cached value.
pub fn palette() -> Option<Palette> {
    state().read().ok().and_then(|s| s.palette)
}

/// Reload the palette if mnml's theme file changed since last checked. Call
/// once per tick from the render loop — a single `stat()` in steady state; a
/// full parse only when the file's mtime actually moves (theme switch).
pub fn poll_refresh() {
    let Some(path) = theme_path() else {
        return;
    };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    {
        let s = state().read().expect("theme lock poisoned");
        if mtime == s.mtime {
            return; // unchanged (incl. both-absent) — nothing to do
        }
    }
    let palette = std::fs::read_to_string(&path).ok().map(|src| project(&src));
    let mut s = state().write().expect("theme lock poisoned");
    s.palette = palette;
    s.mtime = mtime;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_reads_base30_and_falls_back() {
        let src = "\
name = \"onedark\"\n\
[base_30]\n\
white = \"#abb2bf\"\n\
one_bg = \"#282c34\"\n\
light_grey = \"#80848d\"\n\
blue = \"#61afef\"\n\
[base_16]\n\
base00 = \"#1e222a\"\n";
        let p = project(src);
        assert_eq!(p.fg, pack_rgba_u8(0xab, 0xb2, 0xbf, 0xff));
        assert_eq!(p.bg, pack_rgba_u8(0x28, 0x2c, 0x34, 0xff));
        // the dim role is read from light_grey (the user's "too dark" fix)
        assert_eq!(p.dim, pack_rgba_u8(0x80, 0x84, 0x8d, 0xff));
        assert_eq!(p.accent, pack_rgba_u8(0x61, 0xaf, 0xef, 0xff));
        // a key absent from the file → mixr's default (not from [base_16])
        assert_eq!(p.red, Palette::defaults().red);
    }
}
