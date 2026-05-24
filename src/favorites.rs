use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::beatport::models::BeatportTrack;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoriteTrack {
    pub id: i64,
    pub title: String,
    pub artist_name: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub duration: Option<f64>,
    pub label_name: Option<String>,
    pub genre_name: Option<String>,
}

impl From<&BeatportTrack> for FavoriteTrack {
    fn from(t: &BeatportTrack) -> Self {
        Self {
            id: t.id,
            title: t.full_title(),
            artist_name: t.artist_name(),
            bpm: t.bpm,
            key: t.key.clone(),
            duration: t.duration,
            label_name: t.label_name.clone(),
            genre_name: t.genre_name.clone(),
        }
    }
}

impl FavoriteTrack {
    pub fn to_beatport_track(&self) -> BeatportTrack {
        BeatportTrack {
            id: self.id,
            title: self.title.clone(),
            mix_name: None,
            artists: vec![crate::beatport::models::BeatportTrackArtist {
                id: 0,
                name: self.artist_name.clone(),
            }],
            bpm: self.bpm,
            key: self.key.clone(),
            duration: self.duration,
            label_id: None,
            label_name: self.label_name.clone(),
            genre_id: None,
            genre_name: self.genre_name.clone(),
            genre_slug: None,
            release_id: None,
            release_date: None,
            remixers: Vec::new(),
            local_path: None,
        }
    }
}

pub struct FavoritesDB {
    favorites: HashMap<i64, FavoriteTrack>,
}

impl FavoritesDB {
    fn file_path() -> PathBuf {
        let dir = dirs::home_dir().unwrap_or_default().join(".mixr");
        std::fs::create_dir_all(&dir).ok();
        dir.join("favorites.json")
    }

    pub fn load() -> Self {
        let path = Self::file_path();
        let favorites = match std::fs::read_to_string(&path) {
            Ok(data) => {
                let list: Vec<FavoriteTrack> = serde_json::from_str(&data).unwrap_or_default();
                list.into_iter().map(|f| (f.id, f)).collect()
            }
            Err(_) => HashMap::new(),
        };
        Self { favorites }
    }

    fn save(&self) {
        let list: Vec<&FavoriteTrack> = self.favorites.values().collect();
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            std::fs::write(Self::file_path(), json).ok();
        }
    }

    pub fn toggle(&mut self, track: &BeatportTrack) -> bool {
        let added =
            if let std::collections::hash_map::Entry::Vacant(e) = self.favorites.entry(track.id) {
                e.insert(FavoriteTrack::from(track));
                self.save();
                true
            } else {
                self.favorites.remove(&track.id);
                self.save();
                false
            };
        crate::ipc::write_event(&serde_json::json!({
            "kind": if added { "favorited" } else { "unfavorited" },
            "track_id": track.id,
            "title": track.full_title(),
            "artist": track.artist_name(),
        }));
        added
    }

    pub fn all_tracks(&self) -> Vec<BeatportTrack> {
        self.favorites
            .values()
            .map(|f| f.to_beatport_track())
            .collect()
    }
}
