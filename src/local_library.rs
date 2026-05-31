//! Local audio library — walks a user-configured directory, extracts
//! metadata via symphonia tags, and surfaces tracks in the browse UI
//! alongside Beatport. Tracks play through the same engine; the only
//! difference is the source (local file vs streaming URL).
//!
//! Cheap walk: synchronously enumerates audio files (FLAC/AAC/MP3/M4A/
//! WAV/OGG) up to a depth limit. Metadata extraction uses symphonia's
//! probe — only reads the file's tag/metadata block, not the audio
//! payload, so a 10k-track library scans in a second or two.
//!
//! Two scan modes:
//! - `scan_library` — recursive, returns a single sorted flat list
//!   (used by "All tracks (recursive)" and the legacy menu entry).
//! - `list_folder` — one level deep only, returning subfolders +
//!   tracks separately so `folder_screen` can build a drill-down
//!   Menu that matches the Beatport browser feel.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;

use crate::beatport::catalog::{BrowseScreen, MenuAction, MenuItem};
use crate::beatport::models::{BeatportTrack, BeatportTrackArtist};

/// Audio extensions we recognize. Anything else in the directory is
/// silently skipped (cover art, README, .DS_Store, etc.).
const AUDIO_EXTS: &[&str] = &["flac", "aac", "m4a", "mp3", "wav", "ogg", "opus"];

/// Walk up to this depth into subdirectories. 4 levels is plenty for
/// `Music/Genre/Artist/Album/track.flac` style trees without scanning
/// the entire home directory if someone misconfigures the path.
const MAX_DEPTH: usize = 4;

/// Recursive scan of a directory. Returns tracks sorted by
/// `Artist - Title`. Empty if `root` doesn't exist or is empty.
/// Used by "All tracks (recursive)" to flatten an arbitrary folder
/// subtree into a single TrackList.
pub fn scan_library(root: &Path) -> Vec<BeatportTrack> {
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

/// Enumerate one level of `dir`: immediate subdirectories and
/// immediate audio files (no recursion). Subdirectories are sorted
/// alphabetically (case-insensitive). Tracks are returned with full
/// metadata, sorted by `Artist - Title`. Returns `(folders, tracks)`.
/// Empty pair if `dir` doesn't exist or can't be read.
pub fn list_folder(dir: &Path) -> (Vec<PathBuf>, Vec<BeatportTrack>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut audio_paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip hidden / system files (.DS_Store, .git, ._foo, …).
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            folders.push(path);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && AUDIO_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext))
        {
            audio_paths.push(path);
        }
    }
    folders.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default()
    });
    audio_paths.sort();
    let mut tracks: Vec<BeatportTrack> = audio_paths
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
    (folders, tracks)
}

/// Build the browse screen for a single folder. Decides among three
/// shapes based on what's inside:
/// - Only subfolders → `Menu` of subfolders + "All tracks (recursive)".
/// - Only tracks → `TrackList` (rich rendering, BPM/key/duration).
/// - Both → `Menu` with "Tracks here (N)" + subfolders + "All tracks
///   (recursive)".
/// - Empty → `Menu` with just a hint row.
///
/// `root_dir` is the configured library root, used to render
/// breadcrumb-style titles relative to it. When `dir == root_dir` the
/// title is "Local Library"; deeper, the title is the relative path.
pub fn folder_screen(root_dir: &Path, dir: &Path) -> BrowseScreen {
    let (folders, tracks) = list_folder(dir);
    let title = folder_title(root_dir, dir);

    // Leaf with only tracks → skip the Menu wrap, push a TrackList.
    if folders.is_empty() && !tracks.is_empty() {
        let count = tracks.len();
        return BrowseScreen::TrackList {
            title: format!("{title} ({count})"),
            tracks,
        };
    }

    let mut items: Vec<MenuItem> = Vec::new();

    // Mixed folder: show the in-this-folder tracks first so the user
    // can jump straight to them without drilling further.
    if !tracks.is_empty() {
        items.push(MenuItem {
            label: format!("♪ Tracks here ({})", tracks.len()),
            action: MenuAction::LoadLocalFolderTracks(dir.to_path_buf()),
        });
    }

    // Subfolder entries — labelled with a leading folder glyph. Each
    // pushes another folder_screen via the PushLocalFolder action.
    for f in &folders {
        let name = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        items.push(MenuItem {
            label: format!("▸ {name}/"),
            action: MenuAction::PushLocalFolder(f.clone()),
        });
    }

    // "All tracks (recursive)" — the legacy flat-list mode, scoped to
    // this folder. Useful when the user wants the entire subtree as
    // one TrackList for sorting / searching.
    if !folders.is_empty() || !tracks.is_empty() {
        items.push(MenuItem {
            label: "⇲ All tracks (recursive)".into(),
            action: MenuAction::LoadLocalLibraryRecursive(dir.to_path_buf()),
        });
    }

    // Empty directory — render an explanatory row so the user isn't
    // staring at a blank screen.
    if items.is_empty() {
        items.push(MenuItem {
            label: "(empty folder)".into(),
            action: MenuAction::PushLocalFolder(dir.to_path_buf()),
        });
    }

    BrowseScreen::Menu { title, items }
}

/// Title for a folder screen — "Local Library" at the root, otherwise
/// the path of `dir` relative to `root_dir` (so the user sees a
/// breadcrumb-like cue in the title without us having to thread the
/// full stack of names).
fn folder_title(root_dir: &Path, dir: &Path) -> String {
    if dir == root_dir {
        return "Local Library".into();
    }
    match dir.strip_prefix(root_dir) {
        Ok(rel) => format!("Local · {}", rel.display()),
        Err(_) => {
            // dir is outside root_dir (e.g. a symlink target) — fall
            // back to just the folder's own name.
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string()
        }
    }
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
        assert!(scan_library(Path::new("")).is_empty());
        assert!(scan_library(Path::new("/this/path/does/not/exist/anywhere")).is_empty());
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

    /// Create a unique tempdir under `env::temp_dir()`. std-only so we
    /// don't add a tempfile dep just for these tests.
    fn make_tempdir(name: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("mixr-test-{name}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_title_at_root_is_local_library() {
        let root = PathBuf::from("/music");
        assert_eq!(folder_title(&root, &root), "Local Library");
    }

    #[test]
    fn folder_title_below_root_is_relative() {
        let root = PathBuf::from("/music");
        let sub = PathBuf::from("/music/2024/House");
        assert_eq!(folder_title(&root, &sub), "Local · 2024/House");
    }

    #[test]
    fn folder_title_outside_root_falls_back_to_dir_name() {
        let root = PathBuf::from("/music");
        let sub = PathBuf::from("/other/Folder");
        assert_eq!(folder_title(&root, &sub), "Folder");
    }

    #[test]
    fn list_folder_returns_empty_for_missing_dir() {
        let (folders, tracks) = list_folder(Path::new("/does/not/exist/anywhere"));
        assert!(folders.is_empty());
        assert!(tracks.is_empty());
    }

    #[test]
    fn list_folder_skips_hidden_entries_and_unknown_extensions() {
        let dir = make_tempdir("hidden");
        std::fs::create_dir(dir.join(".hidden_folder")).unwrap();
        std::fs::create_dir(dir.join("visible_folder")).unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        // Empty placeholder MP3 — extract_track will return None
        // (symphonia can't probe an empty file), so this file is
        // counted as audio by extension but yields zero tracks.
        std::fs::write(dir.join("track.mp3"), b"").unwrap();
        let (folders, tracks) = list_folder(&dir);
        assert_eq!(
            folders.len(),
            1,
            "only the non-hidden folder should be listed"
        );
        assert!(folders[0].ends_with("visible_folder"));
        assert!(
            tracks.is_empty(),
            "empty MP3 placeholder can't be probed by symphonia"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_folder_sorts_folders_alphabetically_case_insensitive() {
        let dir = make_tempdir("sort");
        for name in ["Zulu", "alpha", "Mike", "bravo"] {
            std::fs::create_dir(dir.join(name)).unwrap();
        }
        let (folders, _) = list_folder(&dir);
        let names: Vec<String> = folders
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["alpha", "bravo", "Mike", "Zulu"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_screen_empty_dir_yields_hint_menu() {
        let dir = make_tempdir("empty");
        match folder_screen(&dir, &dir) {
            BrowseScreen::Menu { title, items } => {
                assert_eq!(title, "Local Library");
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].label, "(empty folder)");
            }
            other => panic!("expected Menu, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_screen_only_subdirs_yields_menu_with_recursive_entry() {
        let dir = make_tempdir("subdirs");
        std::fs::create_dir(dir.join("A")).unwrap();
        std::fs::create_dir(dir.join("B")).unwrap();
        match folder_screen(&dir, &dir) {
            BrowseScreen::Menu { items, .. } => {
                // 2 subfolders + "All tracks (recursive)" tail.
                assert_eq!(items.len(), 3);
                assert!(items[0].label.starts_with("▸ A"));
                assert!(items[1].label.starts_with("▸ B"));
                assert!(items[2].label.contains("All tracks (recursive)"));
                assert!(matches!(
                    items[2].action,
                    MenuAction::LoadLocalLibraryRecursive(_)
                ));
            }
            other => panic!("expected Menu, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stable_path_id_varies_across_paths() {
        let a = stable_path_id(Path::new("/a/track1.flac"));
        let b = stable_path_id(Path::new("/a/track2.flac"));
        assert_ne!(a, b, "different paths must hash to different ids");
    }
}
