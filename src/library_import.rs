//! Library imports — turn other DJ software's libraries into mixr
//! tracks. Currently supports:
//!
//!   - **rekordbox.xml** (Pioneer rekordbox desktop export — standard XML schema)
//!   - **rekordbox export.pdb** (Pioneer rekordbox USB-stick / CDJ — DeviceSQL binary)
//!   - **Engine DJ** (Numark/Denon — SQLite m.db, desktop + USB stick)
//!   - **Serato Database V2** (Serato — proprietary tag/length/value binary)
//!
//! Each import returns `Vec<BeatportTrack>` with `local_path` set so
//! the existing playback pipeline can play these tracks the same way
//! it plays user-library files (see `local_library` for that path).
//! Imports are pure metadata — they don't move audio, just point at
//! where the files already are on disk.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::beatport::models::{BeatportTrack, BeatportTrackArtist};

/// Read a rekordbox library XML export and return all tracks with
/// playable metadata. rekordbox writes UTF-8 XML at the path the
/// user picks during File → Export → Collection in xml format. We
/// pull title, artist, BPM, key (Tonality), duration, file path
/// from the `<TRACK>` elements; `<TEMPO>` and `<POSITION_MARK>`
/// children are ignored for now (could feed beat grid later).
pub fn import_rekordbox_xml(xml_path: &Path) -> anyhow::Result<Vec<BeatportTrack>> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let xml = std::fs::read(xml_path)?;
    let mut reader = Reader::from_reader(&xml[..]);
    reader.config_mut().trim_text(true);

    let mut tracks = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) if e.name().as_ref() == b"TRACK" => {
                if let Some(track) = track_from_attributes(e) {
                    tracks.push(track);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "rekordbox.xml parse error at {}: {e}",
                    reader.buffer_position()
                ));
            }
            _ => {}
        }
        buf.clear();
    }
    tracing::info!("Imported {} tracks from rekordbox.xml", tracks.len());
    Ok(tracks)
}

/// Extract track fields from a rekordbox `<TRACK ...>` element. Each
/// rekordbox xml track has 30+ attributes — we only care about the
/// ones we can play with: name, artist, BPM, key, duration,
/// location (the file path).
fn track_from_attributes(e: &quick_xml::events::BytesStart) -> Option<BeatportTrack> {
    let mut name = None;
    let mut artist = None;
    let mut album = None;
    let mut bpm: Option<f64> = None;
    let mut key = None;
    let mut duration: Option<f64> = None;
    let mut location = None;
    let mut genre = None;

    for attr in e.attributes().flatten() {
        let key_bytes = attr.key.as_ref();
        let value = match attr.unescape_value() {
            Ok(v) => v.into_owned(),
            Err(_) => continue,
        };
        match key_bytes {
            b"Name" => name = Some(value),
            b"Artist" => artist = Some(value),
            b"Album" => album = Some(value),
            b"AverageBpm" => bpm = value.parse().ok(),
            b"Tonality" => key = Some(value),
            b"TotalTime" => duration = value.parse().ok(),
            b"Location" => location = Some(value),
            b"Genre" => genre = Some(value),
            _ => {}
        }
    }

    // Rekordbox writes file:// URLs URL-encoded. Decode + drop the
    // scheme so we can pass a real filesystem path to the player.
    let path_str = location.as_ref().map(|loc| decode_file_url(loc))?;
    let path = PathBuf::from(&path_str);
    // Skip entries that point at files we can't open. rekordbox can
    // hold dead links (drive offline, file moved) — silently filter.
    if !path.exists() {
        return None;
    }

    let title = name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string()
    });
    let artist_name = artist.unwrap_or_else(|| "Unknown".into());

    Some(BeatportTrack {
        id: stable_path_id(&path),
        title,
        mix_name: None,
        artists: vec![BeatportTrackArtist {
            id: 0,
            name: artist_name,
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
        local_path: Some(path_str),
    })
}

/// Decode a `file://localhost/Users/me/Music/track.flac` URL into
/// a plain filesystem path. rekordbox always writes the scheme +
/// localhost prefix; cross-platform variants strip those. Manual
/// decode (URL-encoded characters) since `url` crate would require
/// a dep just for this single call.
fn decode_file_url(url: &str) -> String {
    let stripped = url
        .strip_prefix("file://localhost")
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);
    percent_decode(stripped)
}

/// Minimal percent-decode (RFC 3986). Handles `%20` → space,
/// `%23` → `#`, etc. Anything else passes through.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex(bytes[i + 1]);
            let lo = hex(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Same id-hash strategy as `local_library`: high-bit-set negative
/// i64 derived from the file path. Lets favorites/queue dedup work
/// across both import flows without collisions with Beatport ids.
fn stable_path_id(path: &Path) -> i64 {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    (h.finish() | (1u64 << 63)) as i64
}

// ── Engine DJ ───────────────────────────────────────────────────────

/// Read an Engine DJ database (`m.db`, SQLite). Used by Numark
/// Mixstream and Denon Prime hardware. Two cases:
///   - Desktop: `~/Music/Engine Library/Database2/m.db`
///   - USB stick: `<mount>/Engine Library/Database2/m.db` (or older
///     layouts at the root)
///
/// Schema (Engine DJ v3+, condensed):
///   `Track` table — id, title, artist, album, genre, bitrate,
///   bpmAnalyzed (or beatData), key, length (seconds), path
///   `path` is relative to the database root (so we resolve it
///   against the DB's parent dir).
///
/// Older schemas had different table names (`AnalyzedTrack`,
/// `Tracks`) — best-effort: we try the modern v3 query first and
/// fall back if the table doesn't exist.
pub fn import_engine_db(db_path: &Path) -> anyhow::Result<Vec<BeatportTrack>> {
    use rusqlite::{Connection, OpenFlags};

    if !db_path.exists() {
        return Err(anyhow::anyhow!(
            "Engine DB not found: {}",
            db_path.display()
        ));
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let library_root = db_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/"));

    let tracks = query_engine_v3(&conn, &library_root)
        .or_else(|_| query_engine_legacy(&conn, &library_root))?;

    tracing::info!(
        "Imported {} tracks from Engine DJ DB at {}",
        tracks.len(),
        db_path.display()
    );
    Ok(tracks)
}

fn query_engine_v3(
    conn: &rusqlite::Connection,
    library_root: &Path,
) -> anyhow::Result<Vec<BeatportTrack>> {
    // Engine DJ v3: `Track` table holds metadata. `bpmAnalyzed` is
    // the post-analysis BPM (preferred over user-entered `bpm`).
    // `key` is integer 0-23 (Camelot wheel ordering); we leave it
    // as the raw int → string for now since mapping is non-trivial.
    let mut stmt = conn.prepare(
        "SELECT id, title, artist, album, genre, length, bpmAnalyzed, bpm, key, path \
         FROM Track WHERE path IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        let path: String = row.get(9)?;
        let resolved = resolve_engine_path(library_root, &path);
        Ok((
            row.get::<_, Option<String>>(1)?, // title
            row.get::<_, Option<String>>(2)?, // artist
            row.get::<_, Option<String>>(3)?, // album
            row.get::<_, Option<String>>(4)?, // genre
            row.get::<_, Option<f64>>(5)?,    // length
            row.get::<_, Option<f64>>(6)?
                .or_else(||                       // bpmAnalyzed
                row.get::<_, Option<f64>>(7).ok().flatten()), // fallback to bpm
            row.get::<_, Option<i64>>(8)?,    // key (int)
            resolved,
        ))
    })?;

    let mut tracks = Vec::new();
    for row in rows.flatten() {
        let (title, artist, album, genre, length, bpm, key_int, path) = row;
        if !path.exists() {
            continue;
        }
        let id = stable_path_id(&path);
        tracks.push(BeatportTrack {
            id,
            title: title.unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_string()
            }),
            mix_name: None,
            artists: vec![BeatportTrackArtist {
                id: 0,
                name: artist.unwrap_or_else(|| "Unknown".into()),
            }],
            bpm,
            key: key_int.map(engine_key_to_camelot),
            duration: length,
            label_id: None,
            label_name: album,
            genre_id: None,
            genre_name: genre,
            genre_slug: None,
            release_id: None,
            release_date: None,
            remixers: vec![],
            local_path: Some(path.to_string_lossy().into_owned()),
        });
    }
    Ok(tracks)
}

fn query_engine_legacy(
    conn: &rusqlite::Connection,
    library_root: &Path,
) -> anyhow::Result<Vec<BeatportTrack>> {
    // Legacy schema had `AnalyzedTrack` joined to `Track`. Best-
    // effort fallback — if both queries fail, the user gets a clear
    // error pointing at the DB path.
    let mut stmt = conn
        .prepare("SELECT id, title, artist, length, bpm, path FROM Track WHERE path IS NOT NULL")?;
    let rows = stmt.query_map([], |row| {
        let path: String = row.get(5)?;
        Ok((
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            resolve_engine_path(library_root, &path),
        ))
    })?;
    let mut tracks = Vec::new();
    for row in rows.flatten() {
        let (title, artist, length, bpm, path) = row;
        if !path.exists() {
            continue;
        }
        tracks.push(BeatportTrack {
            id: stable_path_id(&path),
            title: title.unwrap_or_else(|| "Untitled".into()),
            mix_name: None,
            artists: vec![BeatportTrackArtist {
                id: 0,
                name: artist.unwrap_or_else(|| "Unknown".into()),
            }],
            bpm,
            key: None,
            duration: length,
            label_id: None,
            label_name: None,
            genre_id: None,
            genre_name: None,
            genre_slug: None,
            release_id: None,
            release_date: None,
            remixers: vec![],
            local_path: Some(path.to_string_lossy().into_owned()),
        });
    }
    Ok(tracks)
}

/// Engine DJ stores track paths relative to the DB root. The DB at
/// `<root>/Engine Library/Database2/m.db` references files at
/// `<root>/<path>`. Sometimes paths start with `/` (already root-
/// rooted) — handle both. Always returns an absolute path.
fn resolve_engine_path(library_root: &Path, path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        library_root.join(p)
    }
}

/// Engine DJ stores the musical key as an integer 0-23 in Camelot
/// wheel order (1A, 1B, 2A, 2B, ..., 12A, 12B). Map back to the
/// human label so it matches what Beatport returns.
fn engine_key_to_camelot(k: i64) -> String {
    if !(0..=23).contains(&k) {
        return format!("?{k}");
    }
    let camelot_num = (k / 2) + 1;
    let camelot_letter = if k % 2 == 0 { "A" } else { "B" };
    format!("{camelot_num}{camelot_letter}")
}

// ── Serato ──────────────────────────────────────────────────────────

/// Read a Serato `database V2` file (lives at
/// `~/Music/_Serato_/database V2` on a typical install).
///
/// Format (reverse-engineered, documented in mixxxdj/mixxx wiki):
///   - Stream of 8-byte-headered records: 4-byte ASCII tag + 4-byte
///     big-endian length, followed by `length` bytes of payload.
///   - Container tags like `otrk` (track) hold nested records in
///     their payload.
///   - String tags carry UTF-16BE payloads.
///   - Numeric tags vary; we only need the string ones for metadata.
///
/// We pull `pfil` (path), `tsng` (title), `tart` (artist), `talb`
/// (album), `tbpm` (BPM as string), `tkey` (key), `tlen` (length
/// "MM:SS" or seconds), `tgen` (genre).
pub fn import_serato_database(db_path: &Path) -> anyhow::Result<Vec<BeatportTrack>> {
    let bytes = std::fs::read(db_path)?;
    let library_root = db_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/"));

    let mut tracks = Vec::new();
    let mut cursor = 0;
    while cursor + 8 <= bytes.len() {
        let tag = &bytes[cursor..cursor + 4];
        let len = u32::from_be_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        let payload_start = cursor + 8;
        let payload_end = payload_start + len;
        if payload_end > bytes.len() {
            break;
        }

        if tag == b"otrk"
            && let Some(track) =
                parse_serato_track(&bytes[payload_start..payload_end], &library_root)
        {
            tracks.push(track);
        }
        cursor = payload_end;
    }
    tracing::info!(
        "Imported {} tracks from Serato database at {}",
        tracks.len(),
        db_path.display()
    );
    Ok(tracks)
}

/// Parse the inner payload of an `otrk` container — a stream of
/// nested tag/length/value records that carry the track's fields.
fn parse_serato_track(payload: &[u8], library_root: &Path) -> Option<BeatportTrack> {
    let mut cursor = 0;
    let mut path_str: Option<String> = None;
    let mut title: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut album: Option<String> = None;
    let mut bpm: Option<f64> = None;
    let mut key: Option<String> = None;
    let mut duration: Option<f64> = None;
    let mut genre: Option<String> = None;

    while cursor + 8 <= payload.len() {
        let tag = &payload[cursor..cursor + 4];
        let len = u32::from_be_bytes([
            payload[cursor + 4],
            payload[cursor + 5],
            payload[cursor + 6],
            payload[cursor + 7],
        ]) as usize;
        let val_start = cursor + 8;
        let val_end = val_start + len;
        if val_end > payload.len() {
            break;
        }

        let value_bytes = &payload[val_start..val_end];
        match tag {
            b"pfil" => path_str = Some(decode_utf16be(value_bytes)),
            b"tsng" => title = Some(decode_utf16be(value_bytes)),
            b"tart" => artist = Some(decode_utf16be(value_bytes)),
            b"talb" => album = Some(decode_utf16be(value_bytes)),
            b"tgen" => genre = Some(decode_utf16be(value_bytes)),
            b"tkey" => key = Some(decode_utf16be(value_bytes)),
            b"tbpm" => bpm = decode_utf16be(value_bytes).parse().ok(),
            b"tlen" => duration = parse_serato_duration(&decode_utf16be(value_bytes)),
            _ => {}
        }
        cursor = val_end;
    }

    let raw_path = path_str?;
    // Serato stores paths relative to the library root (or absolute
    // on macOS — sometimes leading "/Users/..."). Resolve both.
    let candidate = std::path::PathBuf::from(&raw_path);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        library_root.join(&raw_path)
    };
    if !resolved.exists() {
        return None;
    }

    Some(BeatportTrack {
        id: stable_path_id(&resolved),
        title: title.unwrap_or_else(|| {
            resolved
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Untitled")
                .to_string()
        }),
        mix_name: None,
        artists: vec![BeatportTrackArtist {
            id: 0,
            name: artist.unwrap_or_else(|| "Unknown".into()),
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
        local_path: Some(resolved.to_string_lossy().into_owned()),
    })
}

/// Decode UTF-16 big-endian bytes (the encoding Serato uses for
/// string fields) into a Rust String. Best-effort: invalid pairs
/// become U+FFFD via `String::from_utf16_lossy`.
fn decode_utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Serato's `tlen` field is sometimes a "MM:SS" string and sometimes
/// just a number-of-seconds string. Try both.
fn parse_serato_duration(s: &str) -> Option<f64> {
    if let Ok(secs) = s.parse::<f64>() {
        return Some(secs);
    }
    // MM:SS form.
    let mut parts = s.split(':');
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(m * 60.0 + s)
}

// ── Rekordbox PDB (USB stick / `export.pdb`) ────────────────────────
//
// Pioneer's DeviceSQL database — what rekordbox writes to the USB
// stick at `<root>/PIONEER/rekordbox/export.pdb`. CDJs read this
// directly off the stick. Format reverse-engineered by Henry Betts,
// Fabian Lesniak and James Elliott:
//
//   https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/exports.html
//
// Layout in one paragraph: the file is a list of fixed-size pages
// (page_size declared in header). Each page's first 0x28 bytes are a
// page header; after that comes the row heap; the last bytes of the
// page are row-group footers stacked backwards (36 bytes each, one
// per 16 rows). Each row group lists up to 16 u16 offsets into the
// heap plus a presence bitmask. Tables are linked lists of pages
// declared in the file header. We need four tables: Tracks, Artists,
// Genres, Keys — joined on Track's artist_id/genre_id/key_id.

const PDB_PAGE_TYPE_TRACKS: u32 = 0;
const PDB_PAGE_TYPE_GENRES: u32 = 1;
const PDB_PAGE_TYPE_ARTISTS: u32 = 2;
const PDB_PAGE_TYPE_KEYS: u32 = 5;
const PDB_PAGE_HEADER_SIZE: u64 = 0x28;
const PDB_ROW_GROUP_SIZE: u64 = 36;

/// Read a rekordbox `export.pdb` file (USB stick or CDJ-mounted).
/// Returns BeatportTrack entries with `local_path` rooted at the
/// USB volume — the database's file_path field is relative to the
/// volume root (e.g. `/Contents/Tracks/foo.flac`).
pub fn import_rekordbox_pdb(pdb_path: &Path) -> anyhow::Result<Vec<BeatportTrack>> {
    let bytes = std::fs::read(pdb_path)?;
    // Volume root is the directory containing PIONEER/. The .pdb sits
    // at <root>/PIONEER/rekordbox/export.pdb so we walk up three.
    let volume_root = pdb_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"));

    let header = parse_pdb_header(&bytes)
        .ok_or_else(|| anyhow::anyhow!("rekordbox.pdb: malformed header"))?;

    let mut artists: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut genres: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut keys: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for table in &header.tables {
        match table.page_type {
            PDB_PAGE_TYPE_ARTISTS => walk_pdb_pages(&bytes, &header, table, |row| {
                if let Some((id, name)) = parse_pdb_artist_row(row) {
                    artists.insert(id, name);
                }
            }),
            PDB_PAGE_TYPE_GENRES => walk_pdb_pages(&bytes, &header, table, |row| {
                if let Some((id, name)) = parse_pdb_genre_row(row) {
                    genres.insert(id, name);
                }
            }),
            PDB_PAGE_TYPE_KEYS => walk_pdb_pages(&bytes, &header, table, |row| {
                if let Some((id, name)) = parse_pdb_key_row(row) {
                    keys.insert(id, name);
                }
            }),
            _ => {}
        }
    }

    let mut tracks: Vec<BeatportTrack> = Vec::new();
    for table in &header.tables {
        if table.page_type != PDB_PAGE_TYPE_TRACKS {
            continue;
        }
        walk_pdb_pages(&bytes, &header, table, |row| {
            if let Some(pdb) = parse_pdb_track_row(row) {
                let resolved = resolve_pdb_path(&volume_root, &pdb.file_path);
                if !resolved.exists() {
                    return;
                }
                tracks.push(BeatportTrack {
                    id: stable_path_id(&resolved),
                    title: if pdb.title.is_empty() {
                        resolved
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Untitled")
                            .to_string()
                    } else {
                        pdb.title
                    },
                    mix_name: if pdb.mix_name.is_empty() {
                        None
                    } else {
                        Some(pdb.mix_name)
                    },
                    artists: vec![BeatportTrackArtist {
                        id: pdb.artist_id as i64,
                        name: artists
                            .get(&pdb.artist_id)
                            .cloned()
                            .unwrap_or_else(|| "Unknown".into()),
                    }],
                    bpm: if pdb.tempo > 0 {
                        Some(pdb.tempo as f64 / 100.0)
                    } else {
                        None
                    },
                    key: keys.get(&pdb.key_id).cloned(),
                    duration: if pdb.duration > 0 {
                        Some(pdb.duration as f64)
                    } else {
                        None
                    },
                    label_id: None,
                    label_name: None,
                    genre_id: None,
                    genre_name: genres.get(&pdb.genre_id).cloned(),
                    genre_slug: None,
                    release_id: None,
                    release_date: if pdb.release_date.is_empty() {
                        None
                    } else {
                        Some(pdb.release_date)
                    },
                    remixers: vec![],
                    local_path: Some(resolved.to_string_lossy().into_owned()),
                });
            }
        });
    }

    tracing::info!(
        "Imported {} tracks from rekordbox.pdb at {}",
        tracks.len(),
        pdb_path.display()
    );
    Ok(tracks)
}

/// Resolve a PDB file_path against the USB volume root. PDB paths
/// are stored with a leading `/` and are relative to the stick root,
/// so we strip the slash and join.
fn resolve_pdb_path(volume_root: &Path, file_path: &str) -> PathBuf {
    let trimmed = file_path.trim_start_matches('/');
    volume_root.join(trimmed)
}

struct PdbHeader {
    page_size: u32,
    tables: Vec<PdbTable>,
}

struct PdbTable {
    page_type: u32,
    first_page: u32,
    last_page: u32,
}

fn parse_pdb_header(bytes: &[u8]) -> Option<PdbHeader> {
    // u32 unknown1=0, u32 page_size, u32 num_tables, u32 next_unused,
    // u32 unknown, u32 sequence, u32 gap=0, then num_tables * Table.
    if bytes.len() < 28 {
        return None;
    }
    if read_u32_le(bytes, 0) != 0 {
        return None;
    }
    let page_size = read_u32_le(bytes, 4);
    let num_tables = read_u32_le(bytes, 8) as usize;
    if read_u32_le(bytes, 24) != 0 {
        return None;
    }

    let mut tables = Vec::with_capacity(num_tables);
    let mut cursor = 28;
    for _ in 0..num_tables {
        if cursor + 16 > bytes.len() {
            return None;
        }
        let page_type = read_u32_le(bytes, cursor);
        // empty_candidate at +4 is ignored.
        let first_page = read_u32_le(bytes, cursor + 8);
        let last_page = read_u32_le(bytes, cursor + 12);
        tables.push(PdbTable {
            page_type,
            first_page,
            last_page,
        });
        cursor += 16;
    }
    Some(PdbHeader { page_size, tables })
}

/// Walk every page of a table, calling `each` with each present row's
/// raw byte slice. Pages are a linked list: page_index 0 is invalid,
/// subsequent pages chain via `next_page`. We stop when we hit
/// `last_page`. Empty/dataless pages are skipped via the page_flags
/// bit (0x40 set = no data).
fn walk_pdb_pages(bytes: &[u8], header: &PdbHeader, table: &PdbTable, mut each: impl FnMut(&[u8])) {
    let page_size = header.page_size as u64;
    let mut page_index = table.first_page;
    let mut visited = 0;
    // Cap the walk at a reasonable upper bound — corrupt files have
    // been seen in the wild with self-referential next_page chains.
    while visited < 100_000 {
        visited += 1;
        let page_offset = (page_index as u64) * page_size;
        if page_offset as usize + page_size as usize > bytes.len() {
            return;
        }
        let page = &bytes[page_offset as usize..(page_offset + page_size) as usize];
        if read_u32_le(page, 0) != 0 {
            return;
        } // magic
        let this_index = read_u32_le(page, 4);
        let _page_type = read_u32_le(page, 8);
        let next_page = read_u32_le(page, 12);
        let num_rows_small = page[24] as u16;
        let page_flags = page[27];
        let num_rows_large = read_u16_le(page, 36);
        let num_rows = if num_rows_large > num_rows_small && num_rows_large != 0x1fff {
            num_rows_large
        } else {
            num_rows_small
        };
        let has_data = (page_flags & 0x40) == 0;

        if has_data && num_rows > 0 {
            walk_pdb_row_groups(page, num_rows, page_offset, |row_offset| {
                let abs = page_offset + row_offset as u64;
                if (abs as usize) < bytes.len() {
                    each(&bytes[abs as usize..]);
                }
            });
        }

        if this_index == table.last_page {
            return;
        }
        if next_page == page_index {
            return;
        }
        page_index = next_page;
    }
}

/// Walk all row groups in one page. Row groups are stacked at the
/// END of the page, growing backwards by 36 bytes each. Each group
/// is `[16 * u16 row_offsets][u16 presence_flags][u16 unknown]`.
/// Only rows whose presence-flag bit is set are real.
fn walk_pdb_row_groups(
    page: &[u8],
    num_rows: u16,
    page_heap_offset: u64,
    mut each: impl FnMut(u16),
) {
    // Number of row groups needed to cover num_rows.
    let group_count = (num_rows as usize).div_ceil(16);
    let page_end = page.len() as i64;

    // Group 0 (oldest, holding rows 0..15) is the LAST one in the
    // file — its end is at page_end. Subsequent groups walk forward
    // toward earlier row indices, but they live earlier in the file.
    // Re-derive by group order: in the file, the last group (group at
    // index group_count-1, holding the highest row indices) starts at
    // page_end - 36; group 0 (lowest row indices) starts at
    // page_end - group_count*36.
    for group_idx in 0..group_count {
        let group_end =
            page_end - ((group_count - 1 - group_idx) as i64) * (PDB_ROW_GROUP_SIZE as i64);
        if group_end < (PDB_ROW_GROUP_SIZE as i64) || group_end > page_end {
            continue;
        }
        let group_start = group_end - PDB_ROW_GROUP_SIZE as i64;
        let group = &page[group_start as usize..group_end as usize];
        // Layout inside the group (32 bytes of offsets, then flags + unknown):
        //   row_offsets[16] (u16 each, indexed from end)
        //   row_presence_flags (u16)
        //   unknown (u16)
        let presence = read_u16_le(group, 32);
        for i in 0..16 {
            // Row index within the page (skip groups before this one).
            let abs_row_index = group_idx * 16 + i;
            if abs_row_index >= num_rows as usize {
                break;
            }
            if (presence & (1 << i)) == 0 {
                continue;
            }
            // Row offsets are stored at end-(2*(i+1)) per rekordcrate.
            let off_pos = 32 - 2 * (i + 1);
            let row_offset = read_u16_le(group, off_pos);
            // Defend against insane offsets.
            if (row_offset as u64) < PDB_PAGE_HEADER_SIZE {
                continue;
            }
            if (row_offset as u64) >= (page.len() as u64 - page_heap_offset.min(page.len() as u64))
            {
                // Just sanity — let caller bound-check too.
            }
            each(row_offset);
        }
    }
}

fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Parse a DeviceSQL string starting at `slice[0]`. Returns the
/// decoded string (best-effort lossy on encoding errors). Three
/// encodings the format can produce:
///   - Short ASCII: header byte with low bit set; len = (header>>1)-1
///   - Long ASCII (flags=0x40): u16 length, padding byte, then ASCII
///   - Long UCS-2 LE (flags=0x90): same length, then u16 codepoints
///   - ISRC (flags=0x90, magic 0x03): rekordbox quirk — used for the
///     ISRC-only field; null-terminated ASCII after a 0x03 byte
fn decode_devicesql_string(slice: &[u8]) -> Option<String> {
    if slice.is_empty() {
        return None;
    }
    let header = slice[0];
    if header & 1 == 1 {
        // Short ASCII: total length in bytes is (header>>1) including
        // the header itself, so content_len = (header>>1) - 1.
        let content_len = (header as usize >> 1).saturating_sub(1);
        if 1 + content_len > slice.len() {
            return None;
        }
        return Some(String::from_utf8_lossy(&slice[1..1 + content_len]).into_owned());
    }
    // Long form: u8 flags(=header), u16 length, u8 padding, then body.
    if slice.len() < 4 {
        return None;
    }
    let length = read_u16_le(slice, 1) as usize;
    let _padding = slice[3];
    let body_len = length.saturating_sub(4);
    let body = slice.get(4..4 + body_len)?;
    match header {
        0x40 => Some(String::from_utf8_lossy(body).into_owned()),
        0x90 => {
            // ISRC variant has magic 0x03 + null-terminated ASCII.
            if !body.is_empty() && body[0] == 0x03 {
                let end = body[1..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| 1 + p)
                    .unwrap_or(body.len());
                return Some(String::from_utf8_lossy(&body[1..end]).into_owned());
            }
            // UCS-2 LE: read u16 pairs.
            if !body_len.is_multiple_of(2) {
                return None;
            }
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

/// Parsed Track row — only the fields we actually use. Strings are
/// already decoded; ids are kept raw for the Artist/Genre/Key joins.
struct PdbTrackRow {
    artist_id: u32,
    key_id: u32,
    genre_id: u32,
    tempo: u32,    // centi-BPM
    duration: u16, // seconds
    title: String,
    mix_name: String,
    file_path: String,
    release_date: String,
}

/// Track row: 94 bytes of fixed numeric fields, then 21 u16
/// "FilePtr16" string offsets (relative to the row's start address).
/// Each FilePtr16 points to a DeviceSQLString later in the row.
fn parse_pdb_track_row(row: &[u8]) -> Option<PdbTrackRow> {
    // We need 94 + 21*2 = 136 bytes of fixed-size header.
    if row.len() < 136 {
        return None;
    }

    // Key numeric offsets (see rekordcrate Track struct comments).
    let key_id = read_u32_le(row, 32);
    let tempo = read_u32_le(row, 56);
    let genre_id = read_u32_le(row, 60);
    let artist_id = read_u32_le(row, 68);
    let duration = read_u16_le(row, 84);

    // The 21 string offsets start at byte 94. We only care about a
    // few of them — index by position in the published list:
    //   0 isrc, 1-4 unknown, 5 message, 6 kuvo_public, 7 autoload,
    //   8-9 unknown, 10 date_added, 11 release_date, 12 mix_name,
    //   13 unknown, 14 analyze_path, 15 analyze_date, 16 comment,
    //   17 title, 18 unknown, 19 filename, 20 file_path
    let read_str_at = |idx: usize| -> String {
        let off_pos = 94 + idx * 2;
        let str_off = read_u16_le(row, off_pos) as usize;
        if str_off == 0 || str_off >= row.len() {
            return String::new();
        }
        decode_devicesql_string(&row[str_off..]).unwrap_or_default()
    };

    Some(PdbTrackRow {
        artist_id,
        key_id,
        genre_id,
        tempo,
        duration,
        title: read_str_at(17),
        mix_name: read_str_at(12),
        file_path: read_str_at(20),
        release_date: read_str_at(11),
    })
}

/// Artist row: subtype (0x60 short / 0x64 long), index_shift, id,
/// unknown, ofs_name_near. If subtype == 0x64, then a u16 ofs_name_far
/// follows. The string lives at `row_start + (subtype==0x64 ? ofs_far : ofs_near)`.
fn parse_pdb_artist_row(row: &[u8]) -> Option<(u32, String)> {
    if row.len() < 10 {
        return None;
    }
    let subtype = read_u16_le(row, 0);
    let id = read_u32_le(row, 4);
    let ofs_near = row[9];
    let str_off: usize = if subtype == 0x64 {
        if row.len() < 12 {
            return None;
        }
        read_u16_le(row, 10) as usize
    } else {
        ofs_near as usize
    };
    if str_off == 0 || str_off >= row.len() {
        return Some((id, String::new()));
    }
    let name = decode_devicesql_string(&row[str_off..]).unwrap_or_default();
    Some((id, name))
}

/// Genre row: u32 id, then DeviceSQLString name immediately after.
fn parse_pdb_genre_row(row: &[u8]) -> Option<(u32, String)> {
    if row.len() < 5 {
        return None;
    }
    let id = read_u32_le(row, 0);
    let name = decode_devicesql_string(&row[4..]).unwrap_or_default();
    Some((id, name))
}

/// Key row: u32 id, u32 id2 (duplicate), then DeviceSQLString name.
fn parse_pdb_key_row(row: &[u8]) -> Option<(u32, String)> {
    if row.len() < 9 {
        return None;
    }
    let id = read_u32_le(row, 0);
    let name = decode_devicesql_string(&row[8..]).unwrap_or_default();
    Some((id, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_unescapes_space_and_pound() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("Drum%20%26%20Bass"), "Drum & Bass");
        assert_eq!(percent_decode("hash%23one"), "hash#one");
    }

    #[test]
    fn percent_decode_passthroughs_invalid() {
        assert_eq!(percent_decode("%ZZ"), "%ZZ", "invalid hex stays as-is");
        assert_eq!(percent_decode("100%"), "100%", "trailing % stays");
    }

    #[test]
    fn decode_file_url_strips_scheme() {
        assert_eq!(
            decode_file_url("file://localhost/Users/me/track.flac"),
            "/Users/me/track.flac"
        );
        assert_eq!(
            decode_file_url("file:///Users/me/track%20with%20spaces.flac"),
            "/Users/me/track with spaces.flac"
        );
    }

    #[test]
    fn missing_file_returns_none() {
        // Try to import an XML that doesn't exist — graceful error.
        let result = import_rekordbox_xml(Path::new("/this/path/is/imaginary.xml"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_minimal_xml_collection() {
        // Synthetic minimal rekordbox-shaped XML pointing at this
        // test file itself (which definitely exists on disk).
        let this_file = std::env::current_exe().unwrap();
        let path_url = format!("file://localhost{}", this_file.to_string_lossy());
        let url_encoded = path_url.replace(' ', "%20");
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS>
  <COLLECTION Entries="1">
    <TRACK TrackID="1" Name="Test Track" Artist="Tester" AverageBpm="128.50" Tonality="7A" TotalTime="240" Location="{url_encoded}"/>
  </COLLECTION>
</DJ_PLAYLISTS>"#
        );
        let tmp = std::env::temp_dir().join("mixr-rekordbox-test.xml");
        std::fs::write(&tmp, &xml).unwrap();
        let tracks = import_rekordbox_xml(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.title, "Test Track");
        assert_eq!(t.artists[0].name, "Tester");
        assert_eq!(t.bpm, Some(128.5));
        assert_eq!(t.key.as_deref(), Some("7A"));
        assert_eq!(t.duration, Some(240.0));
        assert!(t.local_path.is_some());
    }

    #[test]
    fn engine_key_to_camelot_round_trip() {
        // 0 → 1A, 1 → 1B, 2 → 2A, ... 23 → 12B
        assert_eq!(engine_key_to_camelot(0), "1A");
        assert_eq!(engine_key_to_camelot(1), "1B");
        assert_eq!(engine_key_to_camelot(14), "8A");
        assert_eq!(engine_key_to_camelot(23), "12B");
        assert_eq!(
            engine_key_to_camelot(99),
            "?99",
            "out-of-range stays detectable"
        );
    }

    #[test]
    fn engine_db_missing_path_returns_err() {
        let result = import_engine_db(Path::new("/no/such/m.db"));
        assert!(result.is_err());
    }

    #[test]
    fn engine_db_minimal_v3_schema() {
        // Build a minimal Engine v3 DB with one playable track.
        // Path points at the test binary itself so .exists() passes.
        let this_exe = std::env::current_exe().unwrap();
        let exe_path = this_exe.to_string_lossy();

        // Unique temp path per test run to avoid collisions when
        // tests run in parallel (cargo test default).
        let unique = format!(
            "mixr-engine-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let db_dir = std::env::temp_dir().join(format!("{unique}/Engine Library/Database2"));
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(&unique));
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("m.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE Track (
                id INTEGER PRIMARY KEY, title TEXT, artist TEXT, album TEXT,
                genre TEXT, length REAL, bpmAnalyzed REAL, bpm REAL,
                key INTEGER, path TEXT
            );",
        )
        .unwrap();
        // Use absolute path so the resolver returns it unchanged + exists.
        conn.execute(
            "INSERT INTO Track VALUES (1, 'Test', 'Tester', 'Album', 'Techno', 360.0, 128.5, 128.0, 14, ?1)",
            [exe_path.as_ref()],
        ).unwrap();
        drop(conn);

        let tracks = import_engine_db(&db_path).unwrap();
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.title, "Test");
        assert_eq!(t.artists[0].name, "Tester");
        assert_eq!(t.bpm, Some(128.5)); // bpmAnalyzed wins over bpm
        assert_eq!(t.key.as_deref(), Some("8A"));

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn decode_utf16be_round_trip() {
        // "Hi" in UTF-16BE: 00 48 00 69
        assert_eq!(decode_utf16be(&[0x00, 0x48, 0x00, 0x69]), "Hi");
        // Empty input is empty string, not panic.
        assert_eq!(decode_utf16be(&[]), "");
        // Odd byte count: trailing byte ignored (chunks_exact).
        assert_eq!(decode_utf16be(&[0x00, 0x48, 0x00]), "H");
    }

    #[test]
    fn parse_serato_duration_handles_both_forms() {
        assert_eq!(parse_serato_duration("180"), Some(180.0));
        assert_eq!(parse_serato_duration("3:00"), Some(180.0));
        assert_eq!(parse_serato_duration("4:32"), Some(272.0));
        assert_eq!(parse_serato_duration("garbage"), None);
    }

    #[test]
    fn serato_minimal_database_round_trips() {
        // Build a tiny Serato database V2 by hand: one otrk entry
        // pointing at the test binary.
        let this_exe = std::env::current_exe().unwrap();
        let exe_path = this_exe.to_string_lossy().into_owned();

        // Encode a string as UTF-16BE bytes for nested tag values.
        fn utf16be(s: &str) -> Vec<u8> {
            s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
        }
        // Build a tag record: 4-byte tag + 4-byte BE length + payload.
        fn record(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(tag);
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(payload);
            out
        }

        let mut otrk_payload = Vec::new();
        otrk_payload.extend(record(b"pfil", &utf16be(&exe_path)));
        otrk_payload.extend(record(b"tsng", &utf16be("Test Track")));
        otrk_payload.extend(record(b"tart", &utf16be("Tester")));
        otrk_payload.extend(record(b"tbpm", &utf16be("128.5")));
        otrk_payload.extend(record(b"tkey", &utf16be("7A")));
        otrk_payload.extend(record(b"tlen", &utf16be("3:42")));

        let db = record(b"otrk", &otrk_payload);

        let unique = format!(
            "mixr-serato-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let db_path = std::env::temp_dir().join(unique);
        std::fs::write(&db_path, &db).unwrap();
        let tracks = import_serato_database(&db_path).unwrap();
        let _ = std::fs::remove_file(&db_path);

        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.title, "Test Track");
        assert_eq!(t.artists[0].name, "Tester");
        assert_eq!(t.bpm, Some(128.5));
        assert_eq!(t.key.as_deref(), Some("7A"));
        assert_eq!(t.duration, Some(222.0));
    }

    #[test]
    fn serato_skips_dead_links() {
        // Build a Serato DB with a track pointing at a missing file.
        fn utf16be(s: &str) -> Vec<u8> {
            s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
        }
        fn record(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(tag);
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(payload);
            out
        }
        let mut otrk = Vec::new();
        otrk.extend(record(b"pfil", &utf16be("/nope/missing.flac")));
        otrk.extend(record(b"tsng", &utf16be("Ghost")));
        let db = record(b"otrk", &otrk);

        let unique = format!(
            "mixr-serato-dead-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, &db).unwrap();
        let tracks = import_serato_database(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(tracks.is_empty());
    }

    #[test]
    fn dead_links_are_filtered() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS>
  <COLLECTION Entries="2">
    <TRACK TrackID="1" Name="Real" Location="file://localhost/this/path/missing.flac"/>
    <TRACK TrackID="2" Name="AlsoFake" Location="file://localhost/another/missing.flac"/>
  </COLLECTION>
</DJ_PLAYLISTS>"#;
        let tmp = std::env::temp_dir().join("mixr-rekordbox-deadlinks.xml");
        std::fs::write(&tmp, xml).unwrap();
        let tracks = import_rekordbox_xml(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(tracks.is_empty(), "dead links must be filtered out");
    }

    // ── Rekordbox PDB unit tests ────────────────────────────────────

    #[test]
    fn devicesql_short_ascii_decodes() {
        // header byte 0x09 = ((3+1) << 1) | 1 = 9 → 3-byte ASCII payload "foo"
        assert_eq!(
            decode_devicesql_string(&[0x09, b'f', b'o', b'o']),
            Some("foo".into())
        );
    }

    #[test]
    fn devicesql_long_ascii_decodes() {
        // flags=0x40, length=0x0008 (4 header + 4 body), padding=0, body="abcd"
        let bytes = [0x40, 0x08, 0x00, 0x00, b'a', b'b', b'c', b'd'];
        assert_eq!(decode_devicesql_string(&bytes), Some("abcd".into()));
    }

    #[test]
    fn devicesql_long_ucs2le_decodes() {
        // "Hi" in UCS-2 LE: 48 00 69 00. flags=0x90, length=8 (4 header + 4 body)
        let bytes = [0x90, 0x08, 0x00, 0x00, 0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_devicesql_string(&bytes), Some("Hi".into()));
    }

    #[test]
    fn devicesql_empty_returns_none() {
        assert_eq!(decode_devicesql_string(&[]), None);
    }

    #[test]
    fn pdb_header_round_trips() {
        // Build a 28-byte header + one 16-byte table entry.
        let mut bytes = vec![0u8; 0]; // unknown1=0
        bytes.extend_from_slice(&0u32.to_le_bytes()); // unknown1
        bytes.extend_from_slice(&4096u32.to_le_bytes()); // page_size
        bytes.extend_from_slice(&1u32.to_le_bytes()); // num_tables
        bytes.extend_from_slice(&50u32.to_le_bytes()); // next_unused
        bytes.extend_from_slice(&0u32.to_le_bytes()); // unknown
        bytes.extend_from_slice(&34u32.to_le_bytes()); // sequence
        bytes.extend_from_slice(&0u32.to_le_bytes()); // gap
        // Table: page_type=Tracks(0), empty_candidate=47, first_page=1, last_page=2
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&47u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());

        let header = parse_pdb_header(&bytes).expect("header parses");
        assert_eq!(header.page_size, 4096);
        assert_eq!(header.tables.len(), 1);
        assert_eq!(header.tables[0].page_type, PDB_PAGE_TYPE_TRACKS);
        assert_eq!(header.tables[0].first_page, 1);
        assert_eq!(header.tables[0].last_page, 2);
    }

    #[test]
    fn pdb_genre_row_parses() {
        // u32 id=42, then short-ASCII string "Techno"
        let mut bytes = 42u32.to_le_bytes().to_vec();
        // header byte for "Techno" (6 bytes): ((6+1) << 1) | 1 = 0x0F
        bytes.extend_from_slice(&[0x0F, b'T', b'e', b'c', b'h', b'n', b'o']);
        let (id, name) = parse_pdb_genre_row(&bytes).unwrap();
        assert_eq!(id, 42);
        assert_eq!(name, "Techno");
    }

    #[test]
    fn pdb_artist_row_parses_short_subtype() {
        // subtype=0x60 short, index_shift=0, id=7, unknown=0, ofs_near=10,
        // then string at offset 10 = "DJ"
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0060u16.to_le_bytes()); // subtype
        bytes.extend_from_slice(&0u16.to_le_bytes()); // index_shift
        bytes.extend_from_slice(&7u32.to_le_bytes()); // id
        bytes.push(0); // unknown1
        bytes.push(10); // ofs_near
        // String at offset 10: header for "DJ" = ((2+1) << 1) | 1 = 0x07
        bytes.extend_from_slice(&[0x07, b'D', b'J']);
        let (id, name) = parse_pdb_artist_row(&bytes).unwrap();
        assert_eq!(id, 7);
        assert_eq!(name, "DJ");
    }

    #[test]
    fn pdb_track_row_extracts_metadata() {
        // Build a minimal Track row: 94 numeric bytes, 21 u16 string
        // offsets, then a few short-ASCII strings packed in.
        let mut row = vec![0u8; 94];
        // tempo (offset 56) = 12800 centi-BPM → 128.00
        row[56..60].copy_from_slice(&12800u32.to_le_bytes());
        // genre_id (60) = 5
        row[60..64].copy_from_slice(&5u32.to_le_bytes());
        // artist_id (68) = 7
        row[68..72].copy_from_slice(&7u32.to_le_bytes());
        // key_id (32) = 11
        row[32..36].copy_from_slice(&11u32.to_le_bytes());
        // duration (84) = 240 seconds
        row[84..86].copy_from_slice(&240u16.to_le_bytes());

        // 21 string offsets from 94..136. We'll place strings starting
        // at offset 136. Layout: [title][file_path][release_date]
        let mut offsets = [0u16; 21];

        // Short-ASCII header = ((len+1)<<1)|1, content is `len` bytes.
        // "Test T" len=6 → header = (7<<1)|1 = 0x0F.
        let s_title = vec![0x0Fu8, b'T', b'e', b's', b't', b' ', b'T']; // 7 bytes
        // "/a.f" len=4 → header = (5<<1)|1 = 0x0B
        let s_file = vec![0x0Bu8, b'/', b'a', b'.', b'f']; // 5 bytes
        // "2024" len=4 → header = 0x0B
        let s_rel = vec![0x0Bu8, b'2', b'0', b'2', b'4']; // 5 bytes

        let mut strings_blob = Vec::new();
        let title_off = 136;
        offsets[17] = title_off as u16;
        strings_blob.extend_from_slice(&s_title);
        let file_off = title_off + s_title.len();
        offsets[20] = file_off as u16;
        strings_blob.extend_from_slice(&s_file);
        let rel_off = file_off + s_file.len();
        offsets[11] = rel_off as u16;
        strings_blob.extend_from_slice(&s_rel);

        // Append 21 string offsets after the numeric header.
        for o in offsets.iter() {
            row.extend_from_slice(&o.to_le_bytes());
        }
        assert_eq!(row.len(), 136);
        row.extend_from_slice(&strings_blob);

        let parsed = parse_pdb_track_row(&row).expect("row parses");
        assert_eq!(parsed.tempo, 12800);
        assert_eq!(parsed.duration, 240);
        assert_eq!(parsed.artist_id, 7);
        assert_eq!(parsed.key_id, 11);
        assert_eq!(parsed.genre_id, 5);
        assert_eq!(parsed.title, "Test T");
        assert_eq!(parsed.file_path, "/a.f");
        assert_eq!(parsed.release_date, "2024");
    }

    #[test]
    fn pdb_resolve_path_strips_leading_slash() {
        assert_eq!(
            resolve_pdb_path(Path::new("/Volumes/USB"), "/Contents/foo.flac"),
            PathBuf::from("/Volumes/USB/Contents/foo.flac")
        );
        assert_eq!(
            resolve_pdb_path(Path::new("/Volumes/USB"), "Contents/foo.flac"),
            PathBuf::from("/Volumes/USB/Contents/foo.flac")
        );
    }

    #[test]
    fn pdb_missing_file_returns_err() {
        let result = import_rekordbox_pdb(Path::new("/no/such/export.pdb"));
        assert!(result.is_err());
    }
}
