//! Session save / restore — snapshots the engine state to
//! `~/.mixr/session.json` so a relaunch can resume where the user
//! left off. Scope: the currently-playing track + position, the
//! staged incoming (if any), and the queue. EQ/filter knob state
//! is intentionally not captured — it's tied to the in-flight mix,
//! not worth persisting across a restart.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::beatport::models::BeatportTrack;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackState {
    pub track: BeatportTrack,
    pub position: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub playing: Option<TrackState>,
    pub incoming: Option<TrackState>,
    #[serde(default)]
    pub queue: Vec<BeatportTrack>,
    /// Which physical deck held the playing track. "a" or "b" so
    /// the serialized form stays human-readable.
    #[serde(default = "default_deck")]
    pub playing_deck: String,
    /// Wall-clock timestamp the snapshot was taken (ISO 8601). Purely
    /// informational — shown in the resume prompt.
    #[serde(default)]
    pub saved_at: String,
}

fn default_deck() -> String { "a".into() }

fn session_path() -> PathBuf {
    let dir = dirs::home_dir().unwrap_or_default().join(".mixr");
    std::fs::create_dir_all(&dir).ok();
    dir.join("session.json")
}

/// Serialize + write atomically (write-to-temp + rename) so a crash
/// mid-write can't leave a half-file that crashes a future launch.
pub fn save(snap: &SessionSnapshot) -> std::io::Result<()> {
    let path = session_path();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(snap)
        .map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load() -> Option<SessionSnapshot> {
    let path = session_path();
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn delete() {
    let _ = std::fs::remove_file(session_path());
}

