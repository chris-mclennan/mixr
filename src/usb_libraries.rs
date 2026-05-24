//! USB-stick library auto-detection.
//!
//! DJ controllers (Pioneer CDJs, Numark Mixstreams, Denon Primes)
//! consume their library off USB sticks formatted with one of two
//! known directory layouts:
//!
//!   - **Engine DJ stick** (Numark/Denon): SQLite at the stick root
//!     `Engine Library/Database2/m.db` (or just `m.db` for newer
//!     firmware). Used by Mixstream Pro / Prime decks.
//!   - **Rekordbox stick** (Pioneer): binary database at
//!     `PIONEER/rekordbox/export.pdb`. Used by CDJ-3000 / XDJ-RX3.
//!
//! This module polls mount points (`/Volumes/*` on macOS,
//! `/media/$USER/*` and `/run/media/$USER/*` on Linux) every 2s and
//! reports any detected DJ-library stick. The caller (root browse
//! menu builder) consults `detected_sticks()` each time it builds
//! the menu and adds entries dynamically.
//!
//! Native event-driven detection (DiskArbitration on macOS, udev on
//! Linux, WMI on Windows) would be lower-overhead, but polling at
//! 0.5Hz is cheap (one `read_dir` per known mount root) and
//! cross-platform with no extra deps.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickEntry {
    pub mount: PathBuf,
    pub kind: StickKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickKind {
    /// Engine DJ format (Numark/Denon hardware). DB at
    /// `<mount>/Engine Library/Database2/m.db` typically.
    EngineDj,
    /// Rekordbox export (Pioneer). Binary `.pdb` under
    /// `<mount>/PIONEER/rekordbox/export.pdb`.
    Rekordbox,
}

impl StickKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::EngineDj => "Engine DJ",
            Self::Rekordbox => "Rekordbox",
        }
    }
}

struct Cache {
    last_scan: Option<Instant>,
    entries: Vec<StickEntry>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    last_scan: None,
    entries: Vec::new(),
});
const SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Currently-detected DJ-library USB sticks. Re-scans at most every
/// `SCAN_INTERVAL`; callers can poll freely (e.g., on every browse
/// menu rebuild) without thrashing the filesystem.
pub fn detected_sticks() -> Vec<StickEntry> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let stale = cache
        .last_scan
        .map(|t| t.elapsed() >= SCAN_INTERVAL)
        .unwrap_or(true);
    if stale {
        cache.entries = scan_now();
        cache.last_scan = Some(Instant::now());
    }
    cache.entries.clone()
}

fn scan_now() -> Vec<StickEntry> {
    let mut found = Vec::new();
    for root in mount_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let mount = entry.path();
            if !mount.is_dir() {
                continue;
            }
            if let Some(kind) = detect_kind(&mount) {
                found.push(StickEntry { mount, kind });
            }
        }
    }
    found
}

/// OS-specific places where removable disks mount. Polled on each
/// scan; non-existent roots are silently skipped (Linux distros
/// vary on which `/media` style they use).
fn mount_roots() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![PathBuf::from("/Volumes")]
    } else if cfg!(target_os = "linux") {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        vec![
            PathBuf::from(format!("/media/{user}")),
            PathBuf::from(format!("/run/media/{user}")),
            PathBuf::from("/media"),
            PathBuf::from("/mnt"),
        ]
    } else if cfg!(target_os = "windows") {
        // Windows assigns drive letters; iterate D-Z and try to read
        // the root. Cheap (most are absent → fast Err).
        ('D'..='Z')
            .map(|c| PathBuf::from(format!("{c}:\\")))
            .collect()
    } else {
        Vec::new()
    }
}

/// Inspect a mount for a known DJ-library layout. Returns the kind
/// if the marker file exists, None otherwise. Cheap: 1-2 stat
/// syscalls per mount.
fn detect_kind(mount: &Path) -> Option<StickKind> {
    // Engine DJ: try the standard path + the legacy alternate.
    if mount.join("Engine Library/Database2/m.db").exists()
        || mount.join("Engine Library/m.db").exists()
        || mount.join("m.db").exists()
    {
        return Some(StickKind::EngineDj);
    }
    if mount.join("PIONEER/rekordbox/export.pdb").exists() || mount.join("PIONEER/USBANLZ").exists()
    // newer rekordbox layouts
    {
        return Some(StickKind::Rekordbox);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_kind_labels() {
        assert_eq!(StickKind::EngineDj.label(), "Engine DJ");
        assert_eq!(StickKind::Rekordbox.label(), "Rekordbox");
    }

    #[test]
    fn detect_kind_returns_none_for_empty_dir() {
        let tmp = std::env::temp_dir().join(format!("mixr-stick-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(detect_kind(&tmp), None, "empty dir is not a DJ stick");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detect_kind_finds_engine() {
        let tmp = std::env::temp_dir().join(format!("mixr-engine-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("Engine Library/Database2")).unwrap();
        std::fs::write(tmp.join("Engine Library/Database2/m.db"), b"fake").unwrap();
        assert_eq!(detect_kind(&tmp), Some(StickKind::EngineDj));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detect_kind_finds_rekordbox() {
        let tmp = std::env::temp_dir().join(format!("mixr-rb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("PIONEER/rekordbox")).unwrap();
        std::fs::write(tmp.join("PIONEER/rekordbox/export.pdb"), b"fake").unwrap();
        assert_eq!(detect_kind(&tmp), Some(StickKind::Rekordbox));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
