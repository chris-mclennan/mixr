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
                if latest != current
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
}
