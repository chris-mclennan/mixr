use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

use crate::beatport::models::BeatportTrack;

/// Truncate a string to max_len, adding "…" if truncated.
fn trunc(s: &str, max_len: usize) -> String {
    if max_len <= 2 {
        return String::new();
    }
    if s.chars().count() > max_len {
        let truncated: String = s.chars().take(max_len - 1).collect();
        format!("{truncated}…")
    } else {
        s.to_string()
    }
}

/// Pad a string to exactly `width` characters (right-pad with spaces).
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.chars().take(width).collect()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Build two spans for a column: text with `style`, then padding with `pad_style`.
/// This way underline only applies to the actual text, not trailing spaces.
fn col_spans(text: &str, width: usize, style: Style, pad_style: Style) -> Vec<Span<'static>> {
    let trimmed = text.trim_end();
    let pad_len = width.saturating_sub(trimmed.chars().count());
    let mut spans = vec![Span::styled(trimmed.to_string(), style)];
    if pad_len > 0 {
        spans.push(Span::styled(" ".repeat(pad_len), pad_style));
    }
    spans
}

/// Render a track list (no border — caller provides the outer block).
/// `selected_column`: -1 = whole row, 0=artist, 1=remixer, 2=label, 3=genre, 4=date
pub fn render_track_list(
    frame: &mut Frame,
    area: Rect,
    tracks: &[BeatportTrack],
    selected: usize,
    scroll_offset: usize,
    selected_column: i32,
    compact: bool,
) {
    // Adapter for call sites that already own a `Vec<BeatportTrack>`
    // (browse lists, search results). Builds a cheap `Vec<&_>` and hands off
    // to the ref-based renderer so there's only one real implementation.
    let refs: Vec<&BeatportTrack> = tracks.iter().collect();
    render_track_list_refs(
        frame,
        area,
        &refs,
        selected,
        scroll_offset,
        selected_column,
        compact,
    );
}

/// Ref-based variant of `render_track_list` for call sites where the tracks
/// live behind `Arc<BeatportTrack>` (queue / history snapshots). Avoids
/// deep-cloning each entry just to satisfy the slice signature.
pub fn render_track_list_refs(
    frame: &mut Frame,
    area: Rect,
    tracks: &[&BeatportTrack],
    selected: usize,
    scroll_offset: usize,
    selected_column: i32,
    compact: bool,
) {
    if tracks.is_empty() {
        frame.render_widget(
            Paragraph::new("  No tracks").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let width = area.width as usize;

    if compact {
        render_track_list_compact(
            frame,
            area,
            tracks,
            selected,
            scroll_offset,
            selected_column,
            width,
        );
    } else {
        render_track_list_full(
            frame,
            area,
            tracks,
            selected,
            scroll_offset,
            selected_column,
            width,
        );
    }
}

fn render_track_list_compact(
    frame: &mut Frame,
    area: Rect,
    tracks: &[&BeatportTrack],
    selected: usize,
    scroll_offset: usize,
    selected_column: i32,
    width: usize,
) {
    let pfx_w = 5;
    let usable = width.saturating_sub(pfx_w);
    let col_title = (usable * 24 / 100).max(10);
    let col_artist = (usable * 16 / 100).max(8);
    let col_dur = 6;
    let col_bpm = 10;
    let col_label = (usable * 12 / 100).max(6);
    let col_genre = (usable * 12 / 100).max(6);
    let used_cols = col_title + col_artist + col_dur + col_bpm + col_label + col_genre;
    let col_date = usable.saturating_sub(used_cols).max(10);

    // Column header
    let hdr_indent = " ".repeat(pfx_w);
    let header = format!(
        "{hdr_indent}{}{}{}{}{}{}{}",
        pad("TITLE", col_title),
        pad("ARTIST", col_artist),
        pad("DUR", col_dur),
        pad("BPM/KEY", col_bpm),
        pad("LABEL", col_label),
        pad("GENRE", col_genre),
        pad("DATE", col_date),
    );
    let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(Span::styled(
        header,
        Style::default().fg(Color::DarkGray),
    )))];

    let visible_count = area.height.saturating_sub(1) as usize;
    let track_items: Vec<ListItem> = tracks
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_count)
        .map(|(i, track)| {
            let is_sel = i == selected;
            let arrow = if is_sel { "▸" } else { " " };
            let num = format!("{:>2}.", i + 1);

            let title_s = pad(&trunc(&track.full_title(), col_title - 1), col_title);
            let artist_s = pad(&trunc(&track.artist_name(), col_artist - 1), col_artist);
            let dur_s = pad(&track.formatted_duration(), col_dur);
            let bpm_key = format_bpm_key(track);
            let bpm_s = pad(&trunc(&bpm_key, col_bpm - 1), col_bpm);
            let label_s = pad(
                &trunc(track.label_name.as_deref().unwrap_or(""), col_label - 1),
                col_label,
            );
            let genre_s = pad(
                &trunc(track.genre_name.as_deref().unwrap_or(""), col_genre - 1),
                col_genre,
            );
            let date_s = pad(track.release_date.as_deref().unwrap_or(""), col_date);

            let col_sel = if is_sel { selected_column } else { -2 };
            let has_col = col_sel >= -1; // -1 = title, 0+ = other columns

            // Whole row highlighted only when col_sel == -2 (no column)
            let row_style = if is_sel && !has_col {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Gray)
            };
            // Selected row with column active: white text, no reverse
            let sel_row_style = if is_sel && has_col {
                Style::default().fg(Color::White)
            } else {
                row_style
            };
            let ul_style = Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::UNDERLINED);

            // (text, column_id): -1=title, 0=artist, 2=label, 3=genre, 4=date, -3=not selectable
            let cols: [(&str, i32); 7] = [
                (&title_s, -1),
                (&artist_s, 0),
                (&dur_s, -3),
                (&bpm_s, -3),
                (&label_s, 2),
                (&genre_s, 3),
                (&date_s, 4),
            ];

            let prefix = format!("{arrow} {num} ");
            let mut spans: Vec<Span> = vec![Span::styled(prefix, sel_row_style)];
            for (text, col_id) in &cols {
                if has_col && *col_id == col_sel {
                    // Underline only the text, not trailing spaces
                    spans.extend(col_spans(
                        text,
                        text.chars().count(),
                        ul_style,
                        sel_row_style,
                    ));
                } else {
                    spans.push(Span::styled(text.to_string(), sel_row_style));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    items.extend(track_items);
    frame.render_widget(List::new(items), area);
}

fn render_track_list_full(
    frame: &mut Frame,
    area: Rect,
    tracks: &[&BeatportTrack],
    selected: usize,
    scroll_offset: usize,
    selected_column: i32,
    width: usize,
) {
    let pfx_w = 7;
    let usable = width.saturating_sub(pfx_w);
    let col1 = (usable * 40 / 100).max(15); // title + dur
    let col2 = (usable * 20 / 100).max(8); // label or artist
    let col3 = (usable * 22 / 100).max(8); // genre or remixer

    // Full view: header + blank + 3 lines per track
    let visible_lines = area.height as usize;

    let mut items: Vec<ListItem> = Vec::new();

    // Column header
    let indent = " ".repeat(pfx_w);
    let header = format!(
        "{indent}{}{}{}RELEASED",
        pad("TITLE / ARTISTS", col1 + 2),
        pad("LABEL / REMIXERS", col2),
        pad("GENRE / BPM & KEY", col3),
    );
    items.push(ListItem::new(Line::from(Span::styled(
        header,
        Style::default().fg(Color::DarkGray),
    ))));
    items.push(ListItem::new(Line::from("")));

    let visible_tracks = visible_lines.saturating_sub(2) / 3 + 1;

    for idx in 0..visible_tracks {
        let track_idx = scroll_offset + idx;
        if track_idx >= tracks.len() {
            break;
        }
        let track = &tracks[track_idx];
        let is_sel = track_idx == selected;
        let col_sel = if is_sel { selected_column } else { -2 };
        let has_col = col_sel >= -1;

        let sel_style = if is_sel && !has_col {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if is_sel && has_col {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::Gray)
        };
        let dim_style = Style::default().fg(Color::DarkGray);
        let ul_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::UNDERLINED);
        let dim_ul = dim_style.add_modifier(Modifier::UNDERLINED);

        // Line 0: ▸ N. Title  Duration  Label  Genre  Date
        let arrow = if is_sel { " ▸" } else { "  " };
        let num = format!("{:>3}.", track_idx + 1);
        let title_dur = {
            let t = trunc(&track.full_title(), col1.saturating_sub(8));
            let d = track.formatted_duration();
            format!("{t}  {d}")
        };
        let label_s = trunc(
            track.label_name.as_deref().unwrap_or(""),
            col2.saturating_sub(2),
        );
        let genre_s = trunc(
            track.genre_name.as_deref().unwrap_or(""),
            col3.saturating_sub(2),
        );
        let date_s = track.release_date.as_deref().unwrap_or("").to_string();

        let cols0: Vec<(String, i32)> = vec![
            (pad(&title_dur, col1 + 2), -1), // title
            (pad(&label_s, col2), 2),        // label
            (pad(&genre_s, col3), 3),        // genre
            (date_s, 4),                     // date
        ];
        let prefix0 = format!("{arrow}{num} ");
        let mut spans0: Vec<Span> = vec![Span::styled(prefix0, sel_style)];
        for (text, col_id) in &cols0 {
            if has_col && *col_id == col_sel {
                spans0.extend(col_spans(text, text.chars().count(), ul_style, sel_style));
            } else {
                spans0.push(Span::styled(text.clone(), sel_style));
            }
        }
        items.push(ListItem::new(Line::from(spans0)));

        // Line 1:        Artist  Remixer  BPM / Key  (dimmed)
        let indent = " ".repeat(pfx_w);
        let artist_s = pad(&trunc(&track.artist_name(), col1), col1 + 2);
        let remixer_s = if track.remixers.is_empty() {
            pad("", col2)
        } else {
            let names: Vec<&str> = track.remixers.iter().map(|r| r.name.as_str()).collect();
            pad(&trunc(&names.join(", "), col2.saturating_sub(2)), col2)
        };
        let bpm_key = format_bpm_key(track);

        let mut spans1: Vec<Span> = vec![Span::styled(indent, dim_style)];
        if col_sel == 0 {
            spans1.extend(col_spans(
                &artist_s,
                artist_s.chars().count(),
                dim_ul,
                dim_style,
            ));
        } else {
            spans1.push(Span::styled(artist_s, dim_style));
        }
        if col_sel == 1 {
            spans1.extend(col_spans(
                &remixer_s,
                remixer_s.chars().count(),
                dim_ul,
                dim_style,
            ));
        } else {
            spans1.push(Span::styled(remixer_s, dim_style));
        }
        spans1.push(Span::styled(bpm_key, dim_style));
        items.push(ListItem::new(Line::from(spans1)));

        // Line 2: blank separator
        items.push(ListItem::new(Line::from("")));
    }

    frame.render_widget(List::new(items), area);
}

fn format_bpm_key(track: &BeatportTrack) -> String {
    match (track.bpm, track.key.as_deref()) {
        (Some(b), Some(k)) => format!("{:.0} / {k}", b),
        (Some(b), None) => format!("{:.0}", b),
        (None, Some(k)) => k.to_string(),
        (None, None) => String::new(),
    }
}

/// Render a simple menu list (no border).
pub fn render_menu(
    frame: &mut Frame,
    area: Rect,
    items: &[String],
    selected: usize,
    scroll_offset: usize,
) {
    let visible_count = area.height as usize;
    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_count)
        .map(|(i, item)| {
            let is_sel = i == selected;
            let arrow = if is_sel { "▸" } else { " " };
            let text = format!("{arrow} {item}");

            if is_sel {
                ListItem::new(Line::from(Span::styled(
                    text,
                    Style::default().add_modifier(Modifier::REVERSED),
                )))
            } else {
                ListItem::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::Gray),
                )))
            }
        })
        .collect();

    frame.render_widget(List::new(list_items), area);
}

/// Render the now-playing bar.
pub fn render_now_playing(
    frame: &mut Frame,
    area: Rect,
    track_name: Option<&str>,
    bpm: Option<f64>,
    time_remaining: f64,
    _progress: f64,
) {
    let track_text = match track_name {
        Some(name) => {
            let bpm_str = bpm.map(|b| format!("  {:.0} BPM", b)).unwrap_or_default();
            let mins = time_remaining as u64 / 60;
            let secs = time_remaining as u64 % 60;
            format!("▶ {name}{bpm_str}  -{mins}:{secs:02}")
        }
        None => "  No track playing".into(),
    };

    let style = if track_name.is_some() {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(Paragraph::new(track_text).style(style), area);
}

/// Help lines data — a flat list of `(chord, description)` rows
/// (and section headers, where `description` is empty).
///
/// **Migration in flight (#59):** rows for chords that have been
/// migrated to `crate::tui::command::registry` come from there
/// (auto-generated, can't drift). Rows for unmigrated chords stay
/// in the hand-maintained `legacy_help_lines()` below. As more
/// chords migrate, the hand-list shrinks. See
/// `docs/COMMAND_MIGRATION.md`.
pub fn help_lines() -> Vec<(String, String)> {
    use crate::tui::command::help_rows;

    let registry_rows = help_rows();

    // Set of *individual* chords already covered by the registry
    // (split out of the `"+ / ="` form). Used to skip duplicate
    // rows in the hand-maintained legacy section.
    let migrated_chords: std::collections::HashSet<String> = registry_rows
        .iter()
        .flat_map(|(keys, _, _)| keys.split(" / ").map(str::trim).map(String::from))
        .collect();

    let mut out: Vec<(String, String)> = Vec::new();

    // ── Registry-driven section ────────────────────────────────
    // Walk commands group-by-group, emitting a section header on
    // each group change. Order is `builtin_commands` order.
    let mut last_group: &str = "";
    for (keys, title, group) in &registry_rows {
        if *group != last_group {
            if !last_group.is_empty() {
                out.push((String::new(), String::new()));
            }
            out.push((group.to_string(), String::new()));
            last_group = group;
        }
        out.push((keys.clone(), title.to_string()));
    }
    if !registry_rows.is_empty() {
        out.push((String::new(), String::new()));
    }

    // ── Legacy hand-maintained section ─────────────────────────
    for (key, desc) in legacy_help_lines() {
        let is_section = desc.is_empty() && !key.is_empty();
        let is_blank = key.is_empty();
        // Section / blank rows pass through (layout). Data rows
        // are skipped if their chord is already in the registry —
        // checked by looking for an exact match against any
        // individual chord we extracted above.
        if !is_section && !is_blank && migrated_chords.contains(key) {
            continue;
        }
        out.push((key.to_string(), desc.to_string()));
    }

    out
}

/// The pre-#59 hand-maintained help table. Still the source of
/// truth for unmigrated chords. New bindings should add a `Command`
/// to `tui::command::builtin_commands` instead of editing this list.
fn legacy_help_lines() -> Vec<(&'static str, &'static str)> {
    vec![
        ("BROWSING", ""),
        ("↑ / ↓", "Navigate"),
        ("Enter / →", "Select / drill into"),
        ("Esc", "Back (quit at root)"),
        ("/ or s", "Search"),
        ("", ""),
        ("PLAYBACK", ""),
        ("Space", "Preview track (toggle)"),
        ("Enter", "Queue track"),
        ("a", "Queue all tracks on screen"),
        ("p", "Pause / resume"),
        ("n", "Skip / next track"),
        ("t", "Teleport to mix point"),
        ("T", "Rewind last mix (replay/experiment)"),
        ("G", "Toggle analyzer engine + re-grid"),
        ("m", "Mix now (force crossfade)"),
        ("A", "AI analyze mix alignment"),
        (
            "< / >",
            "Jump back/forward N bars (click middle of ◀ JUMP N ▶ label to cycle N)",
        ),
        ("[ / ]", "Nudge incoming deck (hold to keep nudging)"),
        ("; / '", "Shift beat grid ±2ms (phase fix)"),
        ("( / )", "Shift beat grid ±1 beat (downbeat fix)"),
        (":", "Open command prompt (e.g. :queue 12345, :tx echoout)"),
        (
            "u / U / i / I / O",
            "Loop 1 / 2 / 4 / 8 / 16 beats (quantized, toggle)",
        ),
        ("S", "Split cue (A=left, B=right)"),
        ("M", "Metronome"),
        (
            "B",
            "Bail (manual panic — switch in-progress crossfade to EchoOut)",
        ),
        ("", ""),
        ("HOT CUES (playing deck)", ""),
        ("1..4", "Jump to hot cue 1–4"),
        ("Shift+1..4 (!@#$)", "Set hot cue 1–4 at current position"),
        ("", ""),
        ("MIXER OVERLAY (z / Z)", ""),
        ("Tab", "Switch deck A ↔ B"),
        ("↑ / ↓", "Select row (EQ low/mid/high, filter, fader)"),
        ("← / →", "Adjust selected row"),
        ("r / R", "Reset row / reset all mixer controls"),
        ("Esc", "Close overlay"),
        ("", ""),
        ("RULES EDITOR (Settings → Edit Transition Rules)", ""),
        ("↑ / ↓", "Navigate rules"),
        ("Enter", "Edit rule (or cycle Default)"),
        ("i", "Insert new rule"),
        ("D", "Delete rule / remove choice"),
        ("{ / }", "Reorder rules"),
        ("Tab", "Switch When / Then / Choices panes"),
        ("+", "Add transition to cycle/weighted action"),
        ("Esc", "Save + close"),
        ("", ""),
        ("QUEUE & TRACKS", ""),
        ("q", "View queue"),
        ("x", "Smart shuffle (BPM + key)"),
        ("X", "Clear queue"),
        ("{ / }", "Grab / drop (reorder queue)"),
        (
            "+ / - (history view)",
            "Rate selected mix good / bad (saves to DJ memory)",
        ),
        ("+ / - (dashboard)", "Rate the most-recent mix good / bad"),
        ("f / *", "Toggle favorite"),
        ("&", "Add track to Beatport cart"),
        ("K", "MIDI learn — bind controller controls to mixr actions"),
        ("r", "Sync favorites audio"),
        ("o", "Open in browser (column-aware)"),
        ("+", "Add to playlist"),
        ("w / W", "Follow / unfollow artist/label"),
        ("e", "Export history"),
        ("y", "Copy screen to clipboard"),
        (
            "L",
            "Load more (pagination) / Load Next from dashboard mini-browse",
        ),
        ("Ctrl+F", "Filter current list"),
        ("", ""),
        ("VIEWS", ""),
        ("d", "Dashboard (live mix view)"),
        ("C", "Toggle Claude DJ on/off"),
        ("/ (dashboard)", "Ask Claude DJ (type prompt)"),
        ("Tab", "Dashboard focus (Controller→Queue→History→Browse)"),
        ("b", "Browse (jump to library)"),
        ("h", "Play history"),
        (",", "Settings"),
        ("v", "Compact / Full view"),
        ("w", "Waveform mode (phrase/audio/off)"),
        ("?", "This help"),
        ("Ctrl+C", "Quit"),
        ("", ""),
        ("CLI OPTIONS", ""),
        ("--play", "Queue default genre chart"),
        ("--play \"Genre\"", "Queue specific genre chart"),
        ("--shuffle", "Smart shuffle on startup"),
        ("--quality flac|256k|128k", "Set audio quality"),
        ("--search \"query\"", "Jump to search"),
        ("--browse \"path\"", "Navigate to path"),
        ("--dashboard", "Start on dashboard view"),
        ("--claude-dj \"prompt\"", "Enable Claude DJ"),
        ("--claude-key KEY", "Store API key"),
        ("--status", "Print current status"),
        ("--command JSON", "Send IPC command"),
        ("--favorites", "List favorites"),
        ("--version", "Show version"),
        ("--logout", "Clear credentials"),
        ("", ""),
        ("FILES", ""),
        ("~/.mixr/config.json", "Settings"),
        ("~/.mixr/auth.json", "Stored credentials"),
        ("~/.mixr/claude_key", "Claude DJ API key"),
        ("~/.mixr/mixr.log", "Engine log"),
        ("~/.mixr/favorites.json", "Favorited tracks"),
        ("~/.mixr/status.json", "Live status (auto-updated)"),
        ("~/.mixr/quick.txt", "Quick status"),
        ("~/.mixr/command", "Remote control (JSON)"),
        ("~/.mixr/screen.txt", "Screen dump"),
        (
            "(memory only)",
            "Tracks live in RAM during playback, no disk cache",
        ),
        ("", ""),
        ("BEATPORT NAVIGATION", ""),
        ("Discover", "Trending, Global Top 100, Hype Top 100"),
        ("Genres", "All genres, favorites first"),
        ("  [Genre]", "Top 100, Charts, Tracks, Releases, Hype"),
        ("Decades", "2020s–1980s → Tracks, Releases, Charts"),
        ("My Beatport", "Tracks, Artists, Labels, Recommendations"),
        ("My Library", "Collection, Cart, Playlists"),
        ("Favorites", "Locally saved tracks"),
    ]
}

/// Render help screen (no border). When `filter` is non-empty, only
/// shows lines whose key or description contains the filter substring
/// (case-insensitive). Section headers are hidden during filtering so
/// only the matched bindings remain. `scroll` is the row offset for
/// vertical scrolling — clamped to keep at least one line visible.
/// Returns nothing; mutates frame.
pub fn render_help(frame: &mut Frame, area: Rect, filter: &str, scroll: &mut u16) {
    let lines = help_lines();
    let f = filter.to_ascii_lowercase();
    let mut text: Vec<Line> = Vec::new();
    // Always show the filter prompt — even when empty — so users can
    // see "I can type to narrow this list." Hint text fades when a
    // filter is active so it doesn't fight the live query for
    // attention. Mention scroll keys explicitly because the help
    // overflows most terminals and scrollability isn't obvious.
    if filter.is_empty() {
        text.push(Line::from(vec![
            Span::styled("  filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled("_  ", Style::default().fg(Color::Cyan)),
            Span::styled(
                "(type to search · ↑↓ PgUp/PgDn scroll · Esc close)",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    } else {
        text.push(Line::from(vec![
            Span::styled("  filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                filter.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("_  ", Style::default().fg(Color::Cyan)),
            Span::styled("(Esc to clear)", Style::default().fg(Color::DarkGray)),
        ]));
    }
    text.push(Line::from(""));
    for (key, desc) in lines.iter() {
        let is_section_header = desc.is_empty() && !key.is_empty();
        let is_blank = key.is_empty();
        if !filter.is_empty() {
            // Hide section headers + blanks while filtering — only
            // matching bindings render.
            if is_section_header || is_blank {
                continue;
            }
            let k = key.to_ascii_lowercase();
            let d = desc.to_ascii_lowercase();
            if !k.contains(&f) && !d.contains(&f) {
                continue;
            }
        }
        text.push(if is_section_header {
            Line::from(Span::styled(
                format!("  {key}"),
                Style::default().add_modifier(Modifier::BOLD),
            ))
        } else if is_blank {
            Line::from("")
        } else {
            Line::from(vec![
                Span::raw(format!("    {:<28}", key)),
                Span::styled(desc.to_string(), Style::default().fg(Color::DarkGray)),
            ])
        });
    }
    if !filter.is_empty() && text.len() <= 2 {
        text.push(Line::from(Span::styled(
            "    (no matches)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Clamp scroll so the user can't end up looking at empty rows
    // below the last entry. `area.height` is the rendered viewport;
    // the first 2 lines (filter prompt + blank) stay visible since
    // they're part of `text` and the scroll is applied to the whole
    // paragraph uniformly.
    let max_scroll = (text.len() as u16).saturating_sub(area.height.max(1));
    if *scroll > max_scroll {
        *scroll = max_scroll;
    }

    frame.render_widget(Paragraph::new(text).scroll((*scroll, 0)), area);
}

#[cfg(test)]
mod tests {
    use super::help_lines;
    use crate::tui::command::registry;

    /// Drift-prevention: every `Command` in the registry that's bound
    /// to at least one default chord must show up in `help_lines()`
    /// with its joined key form (e.g. `"+ / ="` for a command bound
    /// to both `+` and `=`).
    #[test]
    fn every_migrated_command_has_a_help_row() {
        let lines = help_lines();
        for cmd in registry().all() {
            if cmd.keys.is_empty() {
                continue;
            }
            let expected = cmd.keys.join(" / ");
            let present = lines.iter().any(|(k, _)| k == &expected);
            assert!(
                present,
                "migrated command {:?} (keys {:?}) missing from help_lines() — expected row with key {:?}",
                cmd.id, cmd.keys, expected,
            );
        }
    }
}
