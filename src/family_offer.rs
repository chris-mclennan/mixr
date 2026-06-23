//! First-launch "did you know about the rest of the family?" check.
//! Same shape as mnml's `family_offer` — keep them close,
//! sync by hand when one changes. See mnml/src/family_offer.rs for
//! the full design notes.

use std::path::PathBuf;

const FAMILY: &[&str] = &["mnml", "mixr"];
const SELF: &str = "mixr";

pub struct FamilyOffer {
    pub missing: Vec<&'static str>,
}

impl FamilyOffer {
    /// See `mnml/src/family_offer.rs` for the full rationale on why
    /// `mark_shown` runs BEFORE the empty-check return — the
    /// is_installed() probe stat's /Applications/<app>.app on macOS
    /// which Sequoia (15.x) gates behind a privacy prompt, and macOS
    /// only persists Allow/Deny per binary hash. The marker
    /// short-circuits the whole function on subsequent runs so the
    /// prompt only fires once per user, not once per cargo build.
    pub fn maybe_new() -> Option<Self> {
        if marker_path().exists() {
            return None;
        }
        let missing: Vec<&'static str> = FAMILY
            .iter()
            .copied()
            .filter(|name| *name != SELF && !is_installed(name))
            .collect();
        write_marker();
        if missing.is_empty() {
            return None;
        }
        Some(FamilyOffer { missing })
    }

    pub fn mark_shown(&self) {
        write_marker();
    }

    pub fn hint_lines(&self) -> Vec<String> {
        self.missing.iter().map(|app| hint_for(app)).collect()
    }
}

fn hint_for(app: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        format!("Try {app}: brew install chris-mclennan/tap/{app}  ·  https://{app}.sh")
    }
    #[cfg(all(target_os = "linux", not(target_os = "macos")))]
    {
        format!(
            "Try {app}: brew install chris-mclennan/tap/{app}  ·  apt/dnf/AppImage at https://{app}.sh"
        )
    }
    #[cfg(target_os = "windows")]
    {
        format!("Try {app}: winget install chris-mclennan.{app}  ·  https://{app}.sh")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        format!("Try {app}: https://{app}.sh")
    }
}

fn is_installed(app: &str) -> bool {
    if path_lookup(app) {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        let p = format!("/Applications/{app}.app");
        if std::path::Path::new(&p).exists() {
            return true;
        }
    }
    false
}

fn path_lookup(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return true;
        }
        #[cfg(target_os = "windows")]
        {
            for ext in &[".exe", ".cmd", ".bat"] {
                let mut p = candidate.clone();
                let stem = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                p.set_file_name(format!("{stem}{ext}"));
                if p.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

fn marker_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config")
        .join("mixr")
        .join(".family-offer-shown")
}

fn write_marker() {
    let path = marker_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, b"shown\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_contains_self() {
        assert!(FAMILY.contains(&SELF));
    }

    #[test]
    fn hint_for_includes_app_name() {
        assert!(hint_for("mnml").contains("mnml"));
    }

    #[test]
    fn path_lookup_finds_common_binary() {
        assert!(path_lookup("ls") || path_lookup("ls.exe"));
    }
}
