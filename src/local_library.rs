//! Local audio library — walks a user-configured directory, extracts
//! metadata via symphonia tags, and surfaces tracks in the browse UI
//! alongside Beatport. Tracks play through the same engine; the only
//! difference is the source (local file vs streaming URL).
//!
//! Cheap walk: synchronously enumerates audio files (FLAC/AAC/MP3/M4A/
//! WAV/OGG) up to a depth limit. Metadata extraction uses symphonia's
//! probe — only reads the file's tag/metadata block, not the audio
//! payload, so a 10k-track library scans in a second or two.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;

use crate::beatport::models::{BeatportTrack, BeatportTrackArtist};

/// Audio extensions we recognize. Anything else in the directory is
/// silently skipped (cover art, README, .DS_Store, etc.).
const AUDIO_EXTS: &[&str] = &["flac", "aac", "m4a", "mp3", "wav", "ogg", "opus"];

/// Walk up to this depth into subdirectories. 4 levels is plenty for
/// `Music/Genre/Artist/Album/track.flac` style trees without scanning
/// the entire home directory if someone misconfigures the path.
const MAX_DEPTH: usize = 4;

/// Scan the configured local-library directory. Returns tracks sorted
/// by `Artist - Title`. Empty if the dir doesn't exist or is empty.
pub fn scan_library(dir: &str) -> Vec<BeatportTrack> {
    if dir.is_empty() {
        return Vec::new();
    }
    let root = Path::new(dir);
    if !root.is_dir() {
        return Vec::new();
    }

    let mut paths = Vec::new();
    walk(root, 0, &mut paths);
    paths.sort();

    let mut tracks: Vec<BeatportTrack> = paths
        .into_iter()
        .filter_map(|p| extract_track(&p))
        .collect();
    tracks.sort_by(|a, b| {
        a.artist_name()
            .to_lowercase()
            .cmp(&b.artist_name().to_lowercase())
            .then_with(|| {
                a.full_title()
                    .to_lowercase()
                    .cmp(&b.full_title().to_lowercase())
            })
    });
    tracks
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth + 1, out);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && AUDIO_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext))
        {
            out.push(path);
        }
    }
}

/// Extract title/artist/album/bpm from a file's tag block. Returns
/// None if the file can't be probed at all (corrupt or unsupported).
fn extract_track(path: &Path) -> Option<BeatportTrack> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;

    let mut format = probed.format;
    let mut title: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut album: Option<String> = None;
    let mut bpm: Option<f64> = None;
    let mut key: Option<String> = None;
    let mut genre: Option<String> = None;

    let metadata_log = format.metadata();
    if let Some(rev) = metadata_log.current() {
        read_tags(
            rev,
            &mut title,
            &mut artist,
            &mut album,
            &mut bpm,
            &mut key,
            &mut genre,
        );
    } else if let Some(meta) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        read_tags(
            meta,
            &mut title,
            &mut artist,
            &mut album,
            &mut bpm,
            &mut key,
            &mut genre,
        );
    }

    // Track length from the format's first track's codec params.
    let duration = format.default_track().and_then(|t| {
        let sr = t.codec_params.sample_rate? as f64;
        let frames = t.codec_params.n_frames? as f64;
        Some(frames / sr)
    });

    // Filename fallback when tags are missing — strip extension.
    let filename = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let title = title.unwrap_or_else(|| filename.to_string());
    let artist = artist.unwrap_or_else(|| "Unknown".to_string());

    Some(BeatportTrack {
        id: stable_path_id(path),
        title,
        mix_name: None,
        artists: vec![BeatportTrackArtist {
            id: 0,
            name: artist,
        }],
        bpm,
        key,
        duration,
        label_id: None,
        label_name: album,
        genre_id: None,
        genre_name: genre,
        genre_slug: None,
        release_id: None,
        release_date: None,
        remixers: vec![],
        local_path: Some(path.to_string_lossy().into_owned()),
    })
}

fn read_tags(
    rev: &MetadataRevision,
    title: &mut Option<String>,
    artist: &mut Option<String>,
    album: &mut Option<String>,
    bpm: &mut Option<f64>,
    key: &mut Option<String>,
    genre: &mut Option<String>,
) {
    for tag in rev.tags() {
        let value = tag.value.to_string();
        if value.is_empty() {
            continue;
        }
        match tag.std_key {
            Some(StandardTagKey::TrackTitle) => *title = Some(value),
            Some(StandardTagKey::Artist) | Some(StandardTagKey::AlbumArtist) => {
                if artist.is_none() {
                    *artist = Some(value);
                }
            }
            Some(StandardTagKey::Album) => *album = Some(value),
            Some(StandardTagKey::Bpm) => {
                if bpm.is_none()
                    && let Ok(n) = value.parse::<f64>()
                {
                    *bpm = Some(n);
                }
            }
            Some(StandardTagKey::Genre) => *genre = Some(value),
            _ => {
                // Some tag keys not covered by StandardTagKey — sniff
                // by raw key name. INITIALKEY is the common Camelot/key
                // tag.
                let k = tag.key.to_uppercase();
                if (k == "INITIALKEY" || k == "KEY") && key.is_none() {
                    *key = Some(value);
                }
            }
        }
    }
}

/// Stable 63-bit hash of the file path. Used as the BeatportTrack id
/// for local tracks so favorites/queue dedup still works (compares
/// numeric ids). High bit forced to 1 so local IDs never collide
/// with real Beatport ids (which are positive but won't reach the
/// upper half of the i64 range any time soon).
fn stable_path_id(path: &Path) -> i64 {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    let v = h.finish();
    // Set high bit to 1 → reinterpret as i64. Bit 63 set on i64 = negative.
    (v | (1u64 << 63)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dir_returns_empty() {
        assert!(scan_library("").is_empty());
        assert!(scan_library("/this/path/does/not/exist/anywhere").is_empty());
    }

    #[test]
    fn stable_path_id_is_negative_and_consistent() {
        let p = Path::new("/some/track.flac");
        let id1 = stable_path_id(p);
        let id2 = stable_path_id(p);
        assert_eq!(id1, id2, "hash must be deterministic");
        assert!(
            id1 < 0,
            "local IDs must be negative to avoid collision with Beatport IDs"
        );
    }

    #[test]
    fn stable_path_id_varies_across_paths() {
        let a = stable_path_id(Path::new("/a/track1.flac"));
        let b = stable_path_id(Path::new("/a/track2.flac"));
        assert_ne!(a, b, "different paths must hash to different ids");
    }
}
