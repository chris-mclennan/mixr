//! Background "is there a newer release?" check. Mirrors mnml's
//! and tmnl's `update_check` — small enough to keep in sync by
//! hand. See mnml/src/update_check.rs for full design notes.
//!
//! On launch, spawn a std thread that does a single blocking GET
//! against `api.github.com/repos/chris-mclennan/mixr/releases/latest`.
//! If `tag_name` differs from `CARGO_PKG_VERSION`, stash the result
//! on a shared `Arc<UpdateCheck>` that `App::tick` polls. The first
//! tick after data arrives fires a single 12-second toast with the
//! release URL.
//!
//! Skipped in blit mode (separate spawn site; the toast wouldn't
//! have a useful surface until the host catches up).
//!
//! Deliberately simple — string-equality on the tag, no semver
//! parsing. False-positive trips only on an unreleased local dev
//! build whose Cargo.toml version still matches the latest tag;
//! the session-once flag stops re-fires.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const REPO: &str = "chris-mclennan/mixr";
const USER_AGENT: &str = "mixr-update-check (https://github.com/chris-mclennan/mixr)";

pub struct UpdateCheck {
    pub latest_version: Mutex<Option<String>>,
    pub announced: AtomicBool,
}

impl UpdateCheck {
    pub fn spawn() -> Arc<Self> {
        let handle = Arc::new(Self {
            latest_version: Mutex::new(None),
            announced: AtomicBool::new(false),
        });
        let bg = Arc::clone(&handle);
        std::thread::spawn(move || {
            if let Some(latest) = fetch_latest_tag() {
                let current = env!("CARGO_PKG_VERSION");
                // 2026-06-08 hunt M6: was `latest != current`, which
                // also fired on DOWNGRADE (running 0.1.4 against a
                // 0.1.3 latest release). Only announce when remote is
                // strictly newer.
                if is_newer(&latest, current)
                    && let Ok(mut slot) = bg.latest_version.lock()
                {
                    *slot = Some(latest);
                }
            }
        });
        handle
    }

    pub fn take_pending_announcement(&self) -> Option<String> {
        if self.announced.load(Ordering::Relaxed) {
            return None;
        }
        let latest = self.latest_version.lock().ok()?.clone()?;
        self.announced.store(true, Ordering::Relaxed);
        Some(latest)
    }

    pub fn release_url(latest: &str) -> String {
        format!("https://github.com/{REPO}/releases/tag/v{latest}")
    }
}

/// Compare two semver-shaped strings ("0.1.3", "0.1.10", etc.).
/// Returns true iff `remote` is strictly newer than `local`. Tail
/// segments default to 0 so "0.1" < "0.1.1". Anything unparseable
/// returns false — we'd rather skip a real upgrade than announce a
/// phantom one.
fn is_newer(remote: &str, local: &str) -> bool {
    fn parts(v: &str) -> Option<(u64, u64, u64)> {
        let v = v.trim_start_matches('v').split(['-', '+']).next()?;
        let mut it = v.split('.').map(|s| s.parse::<u64>().ok());
        let major = it.next()??;
        let minor = it.next().flatten().unwrap_or(0);
        let patch = it.next().flatten().unwrap_or(0);
        Some((major, minor, patch))
    }
    match (parts(remote), parts(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

fn fetch_latest_tag() -> Option<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?
        .get(&url)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = parsed.get("tag_name")?.as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_url_format() {
        assert_eq!(
            UpdateCheck::release_url("0.1.3"),
            format!("https://github.com/{REPO}/releases/tag/v0.1.3")
        );
    }

    #[test]
    fn take_pending_announcement_one_shot() {
        let uc = UpdateCheck {
            latest_version: Mutex::new(Some("9.0.0".into())),
            announced: AtomicBool::new(false),
        };
        assert_eq!(uc.take_pending_announcement().as_deref(), Some("9.0.0"));
        assert!(uc.take_pending_announcement().is_none());
    }

    #[test]
    fn is_newer_compares_semver_not_lexicographically() {
        // Lex compare would say "0.1.10" < "0.1.9". Semver says the
        // opposite. Lock in the right answer.
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.9", "0.1.10"));
        // Equal: not newer.
        assert!(!is_newer("0.1.3", "0.1.3"));
        // Downgrade: NOT newer (the bug the M6 fix targets).
        assert!(!is_newer("0.1.2", "0.1.4"));
        // Major / minor steps.
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(is_newer("0.2.0", "0.1.99"));
        // `v` prefix is tolerated.
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.2.0", "v0.1.0"));
        // Unparseable inputs are conservatively "not newer".
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("", "0.1.0"));
        // Pre-release suffix: stripped, so 0.1.3-beta == 0.1.3.
        assert!(!is_newer("0.1.3-beta", "0.1.3"));
        // Missing minor/patch: default to 0.
        assert!(!is_newer("0.1", "0.1.0"));
        assert!(is_newer("0.2", "0.1.99"));
    }
}
