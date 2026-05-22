use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportTrackArtist {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportTrack {
    pub id: i64,
    #[serde(alias = "name")]
    pub title: String,
    pub mix_name: Option<String>,
    #[serde(default)]
    pub artists: Vec<BeatportTrackArtist>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    /// Duration in seconds.
    pub duration: Option<f64>,
    pub label_id: Option<i64>,
    pub label_name: Option<String>,
    pub genre_id: Option<i64>,
    pub genre_name: Option<String>,
    pub genre_slug: Option<String>,
    pub release_id: Option<i64>,
    pub release_date: Option<String>,
    #[serde(default)]
    pub remixers: Vec<BeatportTrackArtist>,
    /// Path to a local audio file for tracks sourced from the user's
    /// own library (Settings → Local Library Directory). When set,
    /// the playback pipeline reads the file directly instead of
    /// fetching from Beatport. `id` for these tracks is a stable
    /// hash of the path so favorites/queue dedup work as normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

impl BeatportTrack {
    pub fn full_title(&self) -> String {
        match &self.mix_name {
            Some(mix) if !mix.trim().is_empty() => {
                format!("{} ({})", self.title.trim(), mix.trim())
            }
            _ => self.title.clone(),
        }
    }

    pub fn artist_name(&self) -> String {
        if self.artists.is_empty() {
            "Unknown Artist".into()
        } else {
            self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
        }
    }

    pub fn formatted_duration(&self) -> String {
        match self.duration {
            Some(d) => {
                let minutes = d as u64 / 60;
                let seconds = d as u64 % 60;
                format!("{minutes}:{seconds:02}")
            }
            None => "--:--".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportGenre {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportArtist {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportLabel {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportRelease {
    pub id: i64,
    pub name: String,
    #[serde(default = "unknown_artist")]
    pub artist_name: String,
    pub label_name: Option<String>,
    pub track_count: Option<i64>,
    pub release_date: Option<String>,
}

fn unknown_artist() -> String { "Unknown Artist".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatportChart {
    pub id: i64,
    pub name: String,
    pub owner_name: Option<String>,
    pub track_count: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum BeatportError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("invalid stream URL")]
    InvalidStreamUrl,
    #[error("server error: {0}")]
    ServerError(u16),
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),
}
