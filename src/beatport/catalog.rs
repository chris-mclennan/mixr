use anyhow::Result;

use super::api::BeatportAPI;
use super::models::*;

/// A screen in the browse hierarchy. Each variant knows what to show
/// and what happens when you select an item.
#[derive(Debug, Clone)]
pub enum BrowseScreen {
    /// Static menu with named entries that lead to other screens.
    Menu { title: String, items: Vec<MenuItem> },
    /// List of tracks (loaded from API).
    TrackList {
        title: String,
        tracks: Vec<BeatportTrack>,
    },
    /// List of genres (each leads to genre detail menu).
    GenreList {
        title: String,
        genres: Vec<BeatportGenre>,
    },
    /// List of artists (each leads to artist detail menu).
    ArtistList {
        title: String,
        artists: Vec<BeatportArtist>,
    },
    /// List of labels (each leads to label detail menu).
    LabelList {
        title: String,
        labels: Vec<BeatportLabel>,
    },
    /// List of releases (each leads to release tracks).
    ReleaseList {
        title: String,
        releases: Vec<BeatportRelease>,
    },
    /// List of charts (each leads to chart tracks).
    ChartList {
        title: String,
        charts: Vec<BeatportChart>,
    },
}

impl BrowseScreen {
    pub fn title(&self) -> &str {
        match self {
            Self::Menu { title, .. } => title,
            Self::TrackList { title, .. } => title,
            Self::GenreList { title, .. } => title,
            Self::ArtistList { title, .. } => title,
            Self::LabelList { title, .. } => title,
            Self::ReleaseList { title, .. } => title,
            Self::ChartList { title, .. } => title,
        }
    }

    pub fn item_count(&self) -> usize {
        match self {
            Self::Menu { items, .. } => items.len(),
            Self::TrackList { tracks, .. } => tracks.len(),
            Self::GenreList { genres, .. } => genres.len(),
            Self::ArtistList { artists, .. } => artists.len(),
            Self::LabelList { labels, .. } => labels.len(),
            Self::ReleaseList { releases, .. } => releases.len(),
            Self::ChartList { charts, .. } => charts.len(),
        }
    }

    /// Get display text for item at index.
    pub fn item_label(&self, index: usize) -> String {
        match self {
            Self::Menu { items, .. } => items
                .get(index)
                .map(|i| i.label.clone())
                .unwrap_or_default(),
            Self::TrackList { tracks, .. } => tracks
                .get(index)
                .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
                .unwrap_or_default(),
            Self::GenreList { genres, .. } => genres
                .get(index)
                .map(|g| g.name.clone())
                .unwrap_or_default(),
            Self::ArtistList { artists, .. } => artists
                .get(index)
                .map(|a| a.name.clone())
                .unwrap_or_default(),
            Self::LabelList { labels, .. } => labels
                .get(index)
                .map(|l| l.name.clone())
                .unwrap_or_default(),
            Self::ReleaseList { releases, .. } => releases
                .get(index)
                .map(|r| {
                    if r.artist_name.is_empty() || r.artist_name == "Unknown Artist" {
                        r.name.clone()
                    } else {
                        format!("{} — {}", r.artist_name, r.name)
                    }
                })
                .unwrap_or_default(),
            Self::ChartList { charts, .. } => charts
                .get(index)
                .map(|c| match &c.owner_name {
                    Some(owner) => format!("{} — {}", c.name, owner),
                    None => c.name.clone(),
                })
                .unwrap_or_default(),
        }
    }

    /// Get all track items (for queue all).
    pub fn tracks(&self) -> Option<&[BeatportTrack]> {
        match self {
            Self::TrackList { tracks, .. } => Some(tracks),
            _ => None,
        }
    }

    pub fn track_at(&self, index: usize) -> Option<&BeatportTrack> {
        match self {
            Self::TrackList { tracks, .. } => tracks.get(index),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
}

/// What happens when a menu item is selected.
#[derive(Debug, Clone)]
pub enum MenuAction {
    // Root level
    PushDiscover,
    PushGenres,
    PushDecades,
    PushMyBeatport,
    PushMyLibrary,
    PushFavorites,
    /// User's local-files library — set by Settings → Local Library
    /// Directory. Resolves to a TrackList of files found by
    /// `local_library::scan_library`. Hidden from the root menu when
    /// the config field is empty.
    PushLocalLibrary,
    /// Tracks imported from a rekordbox XML export. Hidden from the
    /// root menu when `config.rekordbox_xml` is empty. Loaded by
    /// `library_import::import_rekordbox_xml`.
    PushRekordbox,
    /// Tracks imported from an Engine DJ database (Numark/Denon).
    /// Hidden when `config.engine_dj_db` is empty. Loaded by
    /// `library_import::import_engine_db`.
    PushEngineDj,
    /// Tracks imported from a Serato `database V2` file. Hidden
    /// when `config.serato_db` is empty.
    PushSerato,
    /// Tracks from an auto-detected USB DJ stick. Each variant in
    /// `usb_libraries::detected_sticks()` adds its own entry.
    PushUsbStick(std::path::PathBuf),

    // Discover
    LoadGlobalTop100,
    LoadHypeTop100,
    PushTrending,
    LoadGlobalTop10,
    LoadHypeTop10,
    LoadTrendingArtists,
    LoadTrendingLabels,
    LoadTrendingGenres,

    // Genre
    PushGenreTrending(i64, String),
    LoadGenreTop10(i64),
    LoadGenrePlaylists(i64),
    LoadGenreTop100(i64),
    LoadGenreTracks(i64),
    LoadGenreCharts(i64),
    LoadGenreReleases(i64),
    LoadGenreExclusives(i64),
    LoadGenreHype(i64),
    LoadGenreArtists(i64),
    LoadGenreLabels(i64),
    PushGenreDecades(i64),

    // Decades
    PushDecade(String, String, Option<i64>), // name, date range, genre_id
    PushDecadeYears(String, String, Option<i64>), // decade_name, decade_range, genre_id
    PushYear(String, String, Option<i64>),   // year_name, year_range, genre_id
    LoadDecadeTracks(String, Option<i64>),
    LoadDecadeReleases(String, Option<i64>),
    LoadDecadeCharts(String, Option<i64>),

    // My Beatport
    LoadMyTracks,
    LoadMyArtists,
    LoadMyLabels,
    LoadRecommendations,

    // My Library
    LoadMyDownloads,
    LoadMyCart,
    LoadMyPlaylists,

    // Detail
    LoadArtistTop100(i64),
    LoadArtistTracks(i64),
    LoadArtistReleases(i64),
    LoadLabelTop100(i64),
    LoadLabelTracks(i64),
    LoadLabelReleases(i64),
    LoadChartTracks(i64),
    LoadReleaseTracks(i64),
    FollowArtist(i64),
    FollowLabel(i64),
}

/// Build the root menu. Pass `local_library_present = true` when the
/// user has configured a local library directory; the menu adds a
/// "Local Library" entry that pushes a TrackList of those files.
/// `rekordbox_present = true` adds a "Rekordbox" entry that loads
/// tracks from the configured rekordbox.xml export.
pub fn root_screen_with_local(local_library_present: bool) -> BrowseScreen {
    root_screen_full(local_library_present, false)
}

/// Build the root menu with optional local + rekordbox + Engine DJ
/// entries. USB sticks are detected automatically and added as
/// dynamic entries — caller doesn't need to know about them.
pub fn root_screen_full(local_library_present: bool, rekordbox_present: bool) -> BrowseScreen {
    root_screen_v2(local_library_present, rekordbox_present, false)
}

pub fn root_screen_v2(
    local_library_present: bool,
    rekordbox_present: bool,
    engine_dj_present: bool,
) -> BrowseScreen {
    root_screen_v3(
        local_library_present,
        rekordbox_present,
        engine_dj_present,
        false,
    )
}

pub fn root_screen_v3(
    local_library_present: bool,
    rekordbox_present: bool,
    engine_dj_present: bool,
    serato_present: bool,
) -> BrowseScreen {
    let mut items = vec![
        MenuItem {
            label: "Discover".into(),
            action: MenuAction::PushDiscover,
        },
        MenuItem {
            label: "Genres".into(),
            action: MenuAction::PushGenres,
        },
        MenuItem {
            label: "Decades".into(),
            action: MenuAction::PushDecades,
        },
        MenuItem {
            label: "My Beatport".into(),
            action: MenuAction::PushMyBeatport,
        },
        MenuItem {
            label: "My Library".into(),
            action: MenuAction::PushMyLibrary,
        },
        MenuItem {
            label: "Favorites".into(),
            action: MenuAction::PushFavorites,
        },
    ];
    if local_library_present {
        items.push(MenuItem {
            label: "Local Library".into(),
            action: MenuAction::PushLocalLibrary,
        });
    }
    if rekordbox_present {
        items.push(MenuItem {
            label: "Rekordbox".into(),
            action: MenuAction::PushRekordbox,
        });
    }
    if engine_dj_present {
        items.push(MenuItem {
            label: "Engine DJ".into(),
            action: MenuAction::PushEngineDj,
        });
    }
    if serato_present {
        items.push(MenuItem {
            label: "Serato".into(),
            action: MenuAction::PushSerato,
        });
    }
    // Auto-detected USB sticks → dynamic entries. Cheap (cached
    // 2s scan); rebuilt every time the menu is constructed.
    for stick in crate::usb_libraries::detected_sticks() {
        let name = stick
            .mount
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("USB");
        let label = format!("USB: {name} ({})", stick.kind.label());
        items.push(MenuItem {
            label,
            action: MenuAction::PushUsbStick(stick.mount),
        });
    }
    BrowseScreen::Menu {
        title: "Beatport".into(),
        items,
    }
}

/// Backwards-compatible: root menu without local library entry.
/// Kept for tests and any future caller that doesn't have config
/// access. Production paths use `root_screen_with_local`.
#[allow(dead_code)]
pub fn root_screen() -> BrowseScreen {
    root_screen_with_local(false)
}

pub fn discover_screen() -> BrowseScreen {
    BrowseScreen::Menu {
        title: "Discover".into(),
        items: vec![
            MenuItem {
                label: "Trending".into(),
                action: MenuAction::PushTrending,
            },
            MenuItem {
                label: "Global Top 100".into(),
                action: MenuAction::LoadGlobalTop100,
            },
            MenuItem {
                label: "Hype Top 100".into(),
                action: MenuAction::LoadHypeTop100,
            },
        ],
    }
}

pub fn trending_screen() -> BrowseScreen {
    BrowseScreen::Menu {
        title: "Trending".into(),
        items: vec![
            MenuItem {
                label: "Global Top 10".into(),
                action: MenuAction::LoadGlobalTop10,
            },
            MenuItem {
                label: "Hype Top 10".into(),
                action: MenuAction::LoadHypeTop10,
            },
            MenuItem {
                label: "Trending Artists".into(),
                action: MenuAction::LoadTrendingArtists,
            },
            MenuItem {
                label: "Trending Labels".into(),
                action: MenuAction::LoadTrendingLabels,
            },
            MenuItem {
                label: "Trending Genres".into(),
                action: MenuAction::LoadTrendingGenres,
            },
        ],
    }
}

pub fn genre_detail_screen(genre_id: i64, name: &str) -> BrowseScreen {
    BrowseScreen::Menu {
        title: name.into(),
        items: vec![
            MenuItem {
                label: "Trending".into(),
                action: MenuAction::PushGenreTrending(genre_id, name.into()),
            },
            MenuItem {
                label: "Top 100".into(),
                action: MenuAction::LoadGenreTop100(genre_id),
            },
            MenuItem {
                label: "Charts".into(),
                action: MenuAction::LoadGenreCharts(genre_id),
            },
            MenuItem {
                label: "Tracks".into(),
                action: MenuAction::LoadGenreTracks(genre_id),
            },
            MenuItem {
                label: "Releases".into(),
                action: MenuAction::LoadGenreReleases(genre_id),
            },
            MenuItem {
                label: "Exclusives".into(),
                action: MenuAction::LoadGenreExclusives(genre_id),
            },
            MenuItem {
                label: "Hype".into(),
                action: MenuAction::LoadGenreHype(genre_id),
            },
            MenuItem {
                label: "Decades".into(),
                action: MenuAction::PushGenreDecades(genre_id),
            },
            MenuItem {
                label: "Artists".into(),
                action: MenuAction::LoadGenreArtists(genre_id),
            },
            MenuItem {
                label: "Labels".into(),
                action: MenuAction::LoadGenreLabels(genre_id),
            },
        ],
    }
}

pub fn genre_trending_screen(genre_id: i64, _name: &str) -> BrowseScreen {
    BrowseScreen::Menu {
        title: "Trending".into(),
        items: vec![
            MenuItem {
                label: "Top 10".into(),
                action: MenuAction::LoadGenreTop10(genre_id),
            },
            MenuItem {
                label: "Playlists".into(),
                action: MenuAction::LoadGenrePlaylists(genre_id),
            },
            MenuItem {
                label: "Trending Artists".into(),
                action: MenuAction::LoadGenreArtists(genre_id),
            },
            MenuItem {
                label: "Trending Labels".into(),
                action: MenuAction::LoadGenreLabels(genre_id),
            },
        ],
    }
}

pub fn decades_screen(genre_id: Option<i64>) -> BrowseScreen {
    let decades = vec![
        ("2020s", "2020-01-01:2029-12-31"),
        ("2010s", "2010-01-01:2019-12-31"),
        ("2000s", "2000-01-01:2009-12-31"),
        ("1990s", "1990-01-01:1999-12-31"),
        ("1980s", "1980-01-01:1989-12-31"),
    ];
    BrowseScreen::Menu {
        title: "Decades".into(),
        items: decades
            .into_iter()
            .map(|(name, range)| MenuItem {
                label: name.into(),
                action: MenuAction::PushDecade(range.into(), name.into(), genre_id),
            })
            .collect(),
    }
}

pub fn decade_detail_screen(name: &str, range: &str, genre_id: Option<i64>) -> BrowseScreen {
    BrowseScreen::Menu {
        title: name.into(),
        items: vec![
            MenuItem {
                label: "Tracks".into(),
                action: MenuAction::LoadDecadeTracks(range.into(), genre_id),
            },
            MenuItem {
                label: "Releases".into(),
                action: MenuAction::LoadDecadeReleases(range.into(), genre_id),
            },
            MenuItem {
                label: "Charts".into(),
                action: MenuAction::LoadDecadeCharts(range.into(), genre_id),
            },
            MenuItem {
                label: "Years".into(),
                action: MenuAction::PushDecadeYears(name.into(), range.into(), genre_id),
            },
        ],
    }
}

pub fn decade_years_screen(
    _decade_name: &str,
    decade_range: &str,
    genre_id: Option<i64>,
) -> BrowseScreen {
    // Parse decade start year from range like "2020-01-01:2029-12-31"
    let start_year: u32 = decade_range[..4].parse().unwrap_or(2020);
    let end_year = start_year + 9;
    // Current year cap
    let current_year = 2026u32;
    let end_year = end_year.min(current_year);

    let items: Vec<MenuItem> = (start_year..=end_year)
        .rev()
        .map(|year| {
            let year_range = format!("{year}-01-01:{year}-12-31");
            MenuItem {
                label: year.to_string(),
                action: MenuAction::PushYear(year.to_string(), year_range, genre_id),
            }
        })
        .collect();

    BrowseScreen::Menu {
        title: "Years".into(),
        items,
    }
}

pub fn year_detail_screen(name: &str, range: &str, genre_id: Option<i64>) -> BrowseScreen {
    BrowseScreen::Menu {
        title: name.into(),
        items: vec![
            MenuItem {
                label: "Tracks".into(),
                action: MenuAction::LoadDecadeTracks(range.into(), genre_id),
            },
            MenuItem {
                label: "Releases".into(),
                action: MenuAction::LoadDecadeReleases(range.into(), genre_id),
            },
            MenuItem {
                label: "Charts".into(),
                action: MenuAction::LoadDecadeCharts(range.into(), genre_id),
            },
        ],
    }
}

pub fn my_beatport_screen() -> BrowseScreen {
    BrowseScreen::Menu {
        title: "My Beatport".into(),
        items: vec![
            MenuItem {
                label: "Tracks".into(),
                action: MenuAction::LoadMyTracks,
            },
            MenuItem {
                label: "Artists".into(),
                action: MenuAction::LoadMyArtists,
            },
            MenuItem {
                label: "Labels".into(),
                action: MenuAction::LoadMyLabels,
            },
            MenuItem {
                label: "Recommendations".into(),
                action: MenuAction::LoadRecommendations,
            },
        ],
    }
}

pub fn my_library_screen() -> BrowseScreen {
    BrowseScreen::Menu {
        title: "My Library".into(),
        items: vec![
            MenuItem {
                label: "Collection".into(),
                action: MenuAction::LoadMyDownloads,
            },
            MenuItem {
                label: "Cart".into(),
                action: MenuAction::LoadMyCart,
            },
            MenuItem {
                label: "Playlists".into(),
                action: MenuAction::LoadMyPlaylists,
            },
        ],
    }
}

pub fn artist_detail_screen(artist_id: i64, name: &str) -> BrowseScreen {
    BrowseScreen::Menu {
        title: name.into(),
        items: vec![
            MenuItem {
                label: "Top 100".into(),
                action: MenuAction::LoadArtistTop100(artist_id),
            },
            MenuItem {
                label: "Tracks".into(),
                action: MenuAction::LoadArtistTracks(artist_id),
            },
            MenuItem {
                label: "Releases".into(),
                action: MenuAction::LoadArtistReleases(artist_id),
            },
            MenuItem {
                label: "Follow / Unfollow".into(),
                action: MenuAction::FollowArtist(artist_id),
            },
        ],
    }
}

pub fn label_detail_screen(label_id: i64, name: &str) -> BrowseScreen {
    BrowseScreen::Menu {
        title: name.into(),
        items: vec![
            MenuItem {
                label: "Top 100".into(),
                action: MenuAction::LoadLabelTop100(label_id),
            },
            MenuItem {
                label: "Tracks".into(),
                action: MenuAction::LoadLabelTracks(label_id),
            },
            MenuItem {
                label: "Releases".into(),
                action: MenuAction::LoadLabelReleases(label_id),
            },
            MenuItem {
                label: "Follow / Unfollow".into(),
                action: MenuAction::FollowLabel(label_id),
            },
        ],
    }
}

/// Execute a menu action — loads data from the API and returns a new screen.
/// Returns None for actions that push a static menu (no API call needed).
pub async fn execute_action(
    action: &MenuAction,
    api: &mut BeatportAPI,
) -> Result<Option<BrowseScreen>> {
    match action {
        // Static menus (no API call)
        MenuAction::PushDiscover => Ok(Some(discover_screen())),
        MenuAction::PushTrending => Ok(Some(trending_screen())),
        MenuAction::PushGenreTrending(id, name) => Ok(Some(genre_trending_screen(*id, name))),
        MenuAction::PushDecades => Ok(Some(decades_screen(None))),
        MenuAction::PushGenreDecades(gid) => Ok(Some(decades_screen(Some(*gid)))),
        MenuAction::PushDecade(range, name, gid) => {
            Ok(Some(decade_detail_screen(name, range, *gid)))
        }
        MenuAction::PushDecadeYears(name, range, gid) => {
            Ok(Some(decade_years_screen(name, range, *gid)))
        }
        MenuAction::PushYear(name, range, gid) => Ok(Some(year_detail_screen(name, range, *gid))),
        MenuAction::PushMyBeatport => Ok(Some(my_beatport_screen())),
        MenuAction::PushMyLibrary => Ok(Some(my_library_screen())),
        // Genres
        MenuAction::PushGenres => {
            let genres = api.genres().await?;
            Ok(Some(BrowseScreen::GenreList {
                title: "Genres".into(),
                genres,
            }))
        }
        MenuAction::LoadTrendingGenres => {
            let genres = api.trending_genres().await?;
            Ok(Some(BrowseScreen::GenreList {
                title: "Trending Genres".into(),
                genres,
            }))
        }

        // Track lists
        MenuAction::LoadGlobalTop100 => {
            let tracks = api.global_top_100().await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Global Top 100".into(),
                tracks,
            }))
        }
        MenuAction::LoadHypeTop100 => {
            let tracks = api.hype_top_100().await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Hype Top 100".into(),
                tracks,
            }))
        }
        MenuAction::LoadGlobalTop10 => {
            let mut tracks = api.global_top_100().await?;
            tracks.truncate(10);
            Ok(Some(BrowseScreen::TrackList {
                title: "Global Top 10".into(),
                tracks,
            }))
        }
        MenuAction::LoadHypeTop10 => {
            let mut tracks = api.hype_top_100().await?;
            tracks.truncate(10);
            Ok(Some(BrowseScreen::TrackList {
                title: "Hype Top 10".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenreTop10(gid) => {
            let mut tracks = api.genre_top_100(*gid).await?;
            tracks.truncate(10);
            Ok(Some(BrowseScreen::TrackList {
                title: "Top 10".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenrePlaylists(gid) => {
            let charts = api.editorial_playlists(*gid).await?;
            Ok(Some(BrowseScreen::ChartList {
                title: "Playlists".into(),
                charts,
            }))
        }
        MenuAction::LoadGenreTop100(gid) => {
            let tracks = api.genre_top_100(*gid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Top 100".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenreTracks(gid) => {
            let tracks = api.genre_tracks(*gid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenreExclusives(gid) => {
            let tracks = api.genre_exclusives(*gid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Exclusives".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenreHype(gid) => {
            let tracks = api.genre_hype(*gid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Hype".into(),
                tracks,
            }))
        }
        MenuAction::LoadChartTracks(cid) => {
            let tracks = api.chart_tracks(*cid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Chart".into(),
                tracks,
            }))
        }
        MenuAction::LoadReleaseTracks(rid) => {
            let tracks = api.release_tracks(*rid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Release".into(),
                tracks,
            }))
        }
        MenuAction::LoadArtistTop100(aid) => {
            let tracks = api.artist_top_100(*aid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Top 100".into(),
                tracks,
            }))
        }
        MenuAction::LoadArtistTracks(aid) => {
            let tracks = api.artist_tracks(*aid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadLabelTop100(lid) => {
            let tracks = api.label_top_100(*lid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Top 100".into(),
                tracks,
            }))
        }
        MenuAction::LoadLabelTracks(lid) => {
            let tracks = api.label_tracks(*lid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadDecadeTracks(range, gid) => {
            let tracks = api.tracks_by_date_range(range, *gid).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadMyTracks => {
            let tracks = api.my_tracks().await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "My Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadRecommendations => {
            let tracks = api.recommendations().await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Recommendations".into(),
                tracks,
            }))
        }
        MenuAction::LoadMyDownloads => {
            let tracks = api.my_downloads().await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Collection".into(),
                tracks,
            }))
        }
        MenuAction::LoadMyCart => {
            let tracks = api.my_cart().await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Cart".into(),
                tracks,
            }))
        }

        // Charts / Releases / Artists / Labels
        MenuAction::LoadGenreCharts(gid) => {
            let charts = api.genre_charts(*gid).await?;
            Ok(Some(BrowseScreen::ChartList {
                title: "Charts".into(),
                charts,
            }))
        }
        MenuAction::LoadGenreReleases(gid) => {
            let releases = api.genre_releases(*gid).await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases,
            }))
        }
        MenuAction::LoadGenreArtists(gid) => {
            let artists = api.trending_artists(Some(*gid)).await?;
            Ok(Some(BrowseScreen::ArtistList {
                title: "Artists".into(),
                artists,
            }))
        }
        MenuAction::LoadGenreLabels(gid) => {
            let labels = api.trending_labels(Some(*gid)).await?;
            Ok(Some(BrowseScreen::LabelList {
                title: "Labels".into(),
                labels,
            }))
        }
        MenuAction::LoadTrendingArtists => {
            let artists = api.trending_artists(None).await?;
            Ok(Some(BrowseScreen::ArtistList {
                title: "Trending Artists".into(),
                artists,
            }))
        }
        MenuAction::LoadTrendingLabels => {
            let labels = api.trending_labels(None).await?;
            Ok(Some(BrowseScreen::LabelList {
                title: "Trending Labels".into(),
                labels,
            }))
        }
        MenuAction::LoadArtistReleases(aid) => {
            let releases = api.artist_releases(*aid).await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases,
            }))
        }
        MenuAction::LoadLabelReleases(lid) => {
            let releases = api.label_releases(*lid).await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases,
            }))
        }
        MenuAction::LoadDecadeReleases(range, gid) => {
            let releases = api.releases_by_date_range(range, *gid).await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases,
            }))
        }
        MenuAction::LoadDecadeCharts(range, gid) => {
            let charts = api.charts_by_date_range(range, *gid).await?;
            Ok(Some(BrowseScreen::ChartList {
                title: "Charts".into(),
                charts,
            }))
        }
        MenuAction::LoadMyArtists => {
            let artists = api.my_artists().await?;
            Ok(Some(BrowseScreen::ArtistList {
                title: "My Artists".into(),
                artists,
            }))
        }
        MenuAction::LoadMyLabels => {
            let labels = api.my_labels().await?;
            Ok(Some(BrowseScreen::LabelList {
                title: "My Labels".into(),
                labels,
            }))
        }
        MenuAction::LoadMyPlaylists => {
            let charts = api.my_playlists().await?;
            Ok(Some(BrowseScreen::ChartList {
                title: "Playlists".into(),
                charts,
            }))
        }
        MenuAction::FollowArtist(aid) => {
            // Toggle: try follow, if already following it'll succeed anyway
            api.follow_artist(*aid).await?;
            Ok(Some(BrowseScreen::Menu {
                title: "Followed!".into(),
                items: vec![MenuItem {
                    label: "✓ Artist followed".into(),
                    action: MenuAction::PushMyBeatport,
                }],
            }))
        }
        MenuAction::FollowLabel(lid) => {
            api.follow_label(*lid).await?;
            Ok(Some(BrowseScreen::Menu {
                title: "Followed!".into(),
                items: vec![MenuItem {
                    label: "✓ Label followed".into(),
                    action: MenuAction::PushMyBeatport,
                }],
            }))
        }
        MenuAction::PushFavorites => {
            Ok(Some(BrowseScreen::TrackList {
                title: "Favorites".into(),
                tracks: Vec::new(),
            })) // TODO
        }
        MenuAction::PushLocalLibrary => {
            // Handled in app::execute_menu_action — needs config access
            // for the library directory. This shouldn't be reached
            // since the dispatcher returns early for it.
            Ok(None)
        }
        MenuAction::PushRekordbox => {
            // Same — config-driven, handled in app::execute_menu_action.
            Ok(None)
        }
        MenuAction::PushEngineDj => Ok(None),
        MenuAction::PushSerato => Ok(None),
        MenuAction::PushUsbStick(_) => Ok(None),
    }
}

/// Execute an action for a specific page (for Load More pagination).
/// Only supports track-loading actions.
pub async fn execute_action_page(
    action: &MenuAction,
    api: &mut BeatportAPI,
    page: u32,
) -> Result<Option<BrowseScreen>> {
    let pg = page.to_string();
    match action {
        MenuAction::LoadGenreTracks(gid) => {
            let tracks = api.genre_tracks_page(*gid, page).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenreExclusives(gid) => {
            let gid_str = gid.to_string();
            let tracks = api
                .paginated_tracks(
                    &[
                        ("genre_id", &gid_str),
                        ("was_ever_exclusive", "true"),
                        ("preorder", "false"),
                        ("order_by", "-publish_date"),
                    ],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Exclusives".into(),
                tracks,
            }))
        }
        MenuAction::LoadGenreHype(gid) => {
            let gid_str = gid.to_string();
            let tracks = api
                .paginated_tracks(
                    &[
                        ("genre_id", &gid_str),
                        ("is_hype", "true"),
                        ("preorder", "false"),
                        ("order_by", "-publish_date"),
                    ],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Hype".into(),
                tracks,
            }))
        }
        MenuAction::LoadArtistTracks(aid) => {
            let aid_str = aid.to_string();
            let tracks = api
                .paginated_tracks(
                    &[("artist_id", &aid_str), ("order_by", "-publish_date")],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadLabelTracks(lid) => {
            let lid_str = lid.to_string();
            let tracks = api
                .paginated_tracks(
                    &[("label_id", &lid_str), ("order_by", "-publish_date")],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadDecadeTracks(range, gid) => {
            let tracks = api.tracks_by_date_range_page(range, *gid, page).await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "Tracks".into(),
                tracks,
            }))
        }
        MenuAction::LoadMyTracks => {
            let tracks = api
                .paginated_tracks(
                    &[("preorder", "false"), ("order_by", "-publish_date")],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::TrackList {
                title: "My Tracks".into(),
                tracks,
            }))
        }
        // Charts
        MenuAction::LoadGenreCharts(gid) => {
            let charts = api.genre_charts_page(*gid, page).await?;
            Ok(Some(BrowseScreen::ChartList {
                title: "Charts".into(),
                charts,
            }))
        }
        MenuAction::LoadDecadeCharts(range, gid) => {
            let gid_str = gid.map(|g| g.to_string());
            let mut params = vec![
                ("publish_date", range.as_str()),
                ("per_page", "50"),
                ("page", &pg),
            ];
            if let Some(ref gid) = gid_str {
                params.push(("genre_id", gid));
            }
            let data = api.paginated_charts(&params).await?;
            Ok(Some(BrowseScreen::ChartList {
                title: "Charts".into(),
                charts: data,
            }))
        }
        // Releases
        MenuAction::LoadGenreReleases(gid) => {
            let gid_str = gid.to_string();
            let data = api
                .paginated_releases(
                    &[
                        ("genre_id", &gid_str),
                        ("enabled", "true"),
                        ("preorder", "false"),
                        ("order_by", "-publish_date"),
                    ],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases: data,
            }))
        }
        MenuAction::LoadArtistReleases(aid) => {
            let aid_str = aid.to_string();
            let data = api
                .paginated_releases(
                    &[("artist_id", &aid_str), ("order_by", "-publish_date")],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases: data,
            }))
        }
        MenuAction::LoadLabelReleases(lid) => {
            let lid_str = lid.to_string();
            let data = api
                .paginated_releases(
                    &[("label_id", &lid_str), ("order_by", "-publish_date")],
                    page,
                )
                .await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases: data,
            }))
        }
        MenuAction::LoadDecadeReleases(range, gid) => {
            let gid_str = gid.map(|g| g.to_string());
            let mut params = vec![
                ("publish_date", range.as_str()),
                ("enabled", "true"),
                ("preorder", "false"),
                ("order_by", "-publish_date"),
            ];
            if let Some(ref gid) = gid_str {
                params.push(("genre_id", gid));
            }
            let data = api.paginated_releases(&params, page).await?;
            Ok(Some(BrowseScreen::ReleaseList {
                title: "Releases".into(),
                releases: data,
            }))
        }
        // Non-paginated actions just re-execute normally
        _ => execute_action(action, api).await,
    }
}
