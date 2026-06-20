use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::audio::analyzer::Phrase;
use crate::audio::engine::{EngineState, NowPlayingInfo};

/// Overall dashboard layout. `Full` is the classic stacked view —
/// controller on top, then claude-dj / queue / history / browse / log
/// in that order. `Panel` keeps the controller pinned and renders just
/// one of the secondary sections below it, so the dashboard can stay
/// short (≈ 25 rows) on a smaller terminal or alongside another app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum DashLayout {
    #[default]
    Full,
    Panel,
}

/// Which secondary section the `Panel` layout shows below the
/// controller. Cycled via `v` from the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PanelSection {
    #[default]
    Queue,
    History,
    Browse,
    Log,
}

impl PanelSection {
    /// Order: Queue → History → Browse → Log → Queue (wraps).
    #[allow(dead_code)]
    pub fn next(self) -> Self {
        match self {
            Self::Queue => Self::History,
            Self::History => Self::Browse,
            Self::Browse => Self::Log,
            Self::Log => Self::Queue,
        }
    }
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::History => "history",
            Self::Browse => "browse",
            Self::Log => "log",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlSection {
    // Per-deck transport / tempo (left side first).
    TempoA,
    VolumeA,
    CueA,
    PlayA,
    JumpA,
    NudgeA,
    // Per-deck mixer (live in the integrated Controller+Mixer panel).
    EqLowA,
    EqMidA,
    EqHighA,
    FilterA,
    Crossfader,
    EqLowB,
    EqMidB,
    EqHighB,
    FilterB,
    NudgeB,
    JumpB,
    PlayB,
    CueB,
    VolumeB,
    TempoB,
}

impl CtrlSection {
    /// Human-readable label for the controller title bar.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::TempoA => "tempo A",
            Self::TempoB => "tempo B",
            Self::VolumeA => "volume A",
            Self::VolumeB => "volume B",
            Self::CueA => "cue A",
            Self::CueB => "cue B",
            Self::PlayA => "play A",
            Self::PlayB => "play B",
            Self::JumpA => "jump A",
            Self::JumpB => "jump B",
            Self::NudgeA => "nudge A",
            Self::NudgeB => "nudge B",
            Self::EqLowA => "EQ low A",
            Self::EqLowB => "EQ low B",
            Self::EqMidA => "EQ mid A",
            Self::EqMidB => "EQ mid B",
            Self::EqHighA => "EQ high A",
            Self::EqHighB => "EQ high B",
            Self::FilterA => "filter A",
            Self::FilterB => "filter B",
            Self::Crossfader => "crossfader",
        }
    }
}
impl CtrlSection {
    /// Cycle order is left-to-right, top-to-bottom — first all of A's
    /// transport and EQ rows, then crossfader, then mirror on B.
    pub fn next(self) -> Self {
        match self {
            Self::TempoA => Self::VolumeA,
            Self::VolumeA => Self::CueA,
            Self::CueA => Self::PlayA,
            Self::PlayA => Self::JumpA,
            Self::JumpA => Self::NudgeA,
            Self::NudgeA => Self::EqLowA,
            Self::EqLowA => Self::EqMidA,
            Self::EqMidA => Self::EqHighA,
            Self::EqHighA => Self::FilterA,
            Self::FilterA => Self::Crossfader,
            Self::Crossfader => Self::EqLowB,
            Self::EqLowB => Self::EqMidB,
            Self::EqMidB => Self::EqHighB,
            Self::EqHighB => Self::FilterB,
            Self::FilterB => Self::NudgeB,
            Self::NudgeB => Self::JumpB,
            Self::JumpB => Self::PlayB,
            Self::PlayB => Self::CueB,
            Self::CueB => Self::VolumeB,
            Self::VolumeB => Self::TempoB,
            Self::TempoB => Self::TempoA,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::TempoA => Self::TempoB,
            Self::VolumeA => Self::TempoA,
            Self::CueA => Self::VolumeA,
            Self::PlayA => Self::CueA,
            Self::JumpA => Self::PlayA,
            Self::NudgeA => Self::JumpA,
            Self::EqLowA => Self::NudgeA,
            Self::EqMidA => Self::EqLowA,
            Self::EqHighA => Self::EqMidA,
            Self::FilterA => Self::EqHighA,
            Self::Crossfader => Self::FilterA,
            Self::EqLowB => Self::Crossfader,
            Self::EqMidB => Self::EqLowB,
            Self::EqHighB => Self::EqMidB,
            Self::FilterB => Self::EqHighB,
            Self::NudgeB => Self::FilterB,
            Self::JumpB => Self::NudgeB,
            Self::PlayB => Self::JumpB,
            Self::CueB => Self::PlayB,
            Self::VolumeB => Self::CueB,
            Self::TempoB => Self::VolumeB,
        }
    }
    /// Stable name for IPC status output. Lets external scripts assert
    /// "are we currently focused on EqLowA?" so dashboard hotkey tests
    /// don't have to count blind ↑/↓ presses from an unknown start.
    pub fn label(self) -> &'static str {
        match self {
            Self::TempoA => "TempoA",
            Self::VolumeA => "VolumeA",
            Self::CueA => "CueA",
            Self::PlayA => "PlayA",
            Self::JumpA => "JumpA",
            Self::NudgeA => "NudgeA",
            Self::EqLowA => "EqLowA",
            Self::EqMidA => "EqMidA",
            Self::EqHighA => "EqHighA",
            Self::FilterA => "FilterA",
            Self::Crossfader => "Crossfader",
            Self::EqLowB => "EqLowB",
            Self::EqMidB => "EqMidB",
            Self::EqHighB => "EqHighB",
            Self::FilterB => "FilterB",
            Self::NudgeB => "NudgeB",
            Self::JumpB => "JumpB",
            Self::PlayB => "PlayB",
            Self::CueB => "CueB",
            Self::VolumeB => "VolumeB",
            Self::TempoB => "TempoB",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformMode {
    Phrase,
    Audio,
    Off,
}
impl WaveformMode {
    pub fn next(self) -> Self {
        match self {
            Self::Phrase => Self::Audio,
            Self::Audio => Self::Off,
            Self::Off => Self::Phrase,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Phrase => "phrase",
            Self::Audio => "audio",
            Self::Off => "off",
        }
    }
}

fn pc(ms: f64) -> Color {
    let a = ms.abs();
    if a < 5.0 {
        Color::Green
    } else if a < 15.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Phase color that also accounts for downbeat alignment. Green
/// requires BOTH within-beat phase under 5ms AND the 1s landing on
/// the same bar slot. If the downbeat is off the dots floor at
/// Yellow regardless of phase — a clean phase reading with wrong
/// 1s is exactly the "tight but flamming" trainwreck this catches.
fn pc_aligned(ms: f64, downbeat_aligned: bool) -> Color {
    if !downbeat_aligned {
        let a = ms.abs();
        if a < 15.0 { Color::Yellow } else { Color::Red }
    } else {
        pc(ms)
    }
}
fn ts(s: &str, w: usize) -> String {
    if w <= 2 {
        String::new()
    } else if s.chars().count() > w {
        let t: String = s.chars().take(w - 1).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}
fn ps(s: &str, w: usize) -> String {
    let l = s.chars().count();
    if l >= w {
        s.chars().take(w).collect()
    } else {
        format!("{s}{}", " ".repeat(w - l))
    }
}
fn ft(t: f64) -> String {
    let s = t.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
/// Dim-gray span. Accepts anything convertible to `Cow<'static, str>` so
/// string literals (which make up most callers) don't allocate — only
/// `format!()` / `String` callers pay the heap cost. Saves ~30 small
/// allocations per dashboard render.
fn dim(s: impl Into<std::borrow::Cow<'static, str>>) -> Span<'static> {
    Span::styled(s, Style::default().fg(Color::DarkGray))
}

/// Render a titled bordered two-column list. Items appear as
/// `● <text>` in two columns (first half left, second half right).
/// `overflow` is an optional extra row like "… N more"; `empty_msg` shows
/// when `items` is empty. Shared by QUEUE and HISTORY renders.
fn render_two_col_box<'a>(
    out: &mut Vec<Line<'a>>,
    title: &str,
    items: &[(Color, String)],
    overflow: Option<&str>,
    empty_msg: &str,
    w: usize,
    focused: bool,
) {
    let bxw = w.saturating_sub(2);
    let content_w = bxw.saturating_sub(2);
    let tr = bxw.saturating_sub(title.len() + 4).saturating_add(1);
    let bs = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let pipe_l = || Span::styled("│ ".to_string(), bs);
    let pipe_r = || Span::styled(" │".to_string(), bs);
    let pipe = || Span::styled("│".to_string(), bs);
    let pipe_mid = || Span::styled("│ ".to_string(), bs);
    out.push(Line::from(Span::styled(
        format!("┌─ {title} {}┐", "─".repeat(tr)),
        bs,
    )));

    // Per-column text width. An item row is laid out as
    //   "│ " + "● " + LEFT + "│ " + "● " + RIGHT + "│"
    // which is 9 fixed cells (borders + bullets) plus the two text
    // columns, and must come out to the same `w` as the top/bottom
    // border. So LEFT + RIGHT = w - 9; the odd char (when w - 9 is
    // odd) goes to the left column. Deriving these from `w` here is
    // what keeps every item row squared up with the border — the old
    // halved-`cw` split silently dropped 1–2 columns, leaving each
    // row's right `│` short of the `┐` / `┘`.
    let pair_w = w.saturating_sub(9);
    let right_w = pair_w / 2;
    let left_w = pair_w - right_w;
    const BULLET_W: usize = 2;

    if items.is_empty() {
        out.push(Line::from(vec![
            pipe_l(),
            Span::styled(ps(empty_msg, content_w), Style::default().fg(Color::Gray)),
            pipe_r(),
        ]));
    } else {
        let n = items.len();
        let half = n.div_ceil(2);
        for row in 0..half {
            let (lc, ref lt) = items[row];
            let right = if row + half < n {
                Some(&items[row + half])
            } else {
                None
            };
            let mut spans: Vec<Span> = vec![pipe_l()];
            spans.push(Span::styled("● ", Style::default().fg(lc)));
            spans.push(Span::styled(
                ps(lt, left_w),
                Style::default().fg(Color::Gray),
            ));
            if let Some((rc, rt)) = right {
                spans.push(pipe_mid());
                spans.push(Span::styled("● ", Style::default().fg(*rc)));
                spans.push(Span::styled(
                    ps(rt, right_w),
                    Style::default().fg(Color::Gray),
                ));
            } else {
                spans.push(pipe_mid());
                spans.push(Span::raw(" ".repeat(right_w + BULLET_W)));
            }
            spans.push(pipe());
            out.push(Line::from(spans));
        }
        if let Some(msg) = overflow {
            out.push(Line::from(vec![
                pipe_l(),
                Span::styled(ps(msg, content_w), Style::default().fg(Color::Gray)),
                pipe_r(),
            ]));
        }
    }
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(bxw)),
        bs,
    )));
}

fn camelot_dist(a: &str, b: &str) -> usize {
    fn p(k: &str) -> Option<(i32, u8)> {
        let k = k.trim();
        let l = *k.as_bytes().last()?;
        if l != b'A' && l != b'B' {
            return None;
        }
        k[..k.len() - 1].parse().ok().map(|n| (n, l))
    }
    let (na, la) = match p(a) {
        Some(v) => v,
        None => return 99,
    };
    let (nb, lb) = match p(b) {
        Some(v) => v,
        None => return 99,
    };
    if la == lb {
        let d = (na - nb).unsigned_abs() as usize;
        d.min(12 - d)
    } else {
        if na == nb {
            1
        } else {
            let d = (na - nb).unsigned_abs() as usize;
            d.min(12 - d) + 1
        }
    }
}

fn compat(cb: Option<f64>, ck: Option<&str>, nb: Option<f64>, nk: Option<&str>) -> (u32, Color) {
    let mut s = 0.0;
    if let (Some(c), Some(n)) = (cb, nb) {
        let r = c.max(n) / c.min(n);
        let h = if r > 1.8 { r / 2.0 } else { r };
        s += (50.0 - (h - 1.0).abs() * 500.0).max(0.0);
    }
    if let (Some(a), Some(b)) = (ck, nk) {
        let d = camelot_dist(a, b);
        s += match d {
            0 => 50.0,
            1 => 45.0,
            2 => 30.0,
            _ => (20.0 - d as f64 * 5.0).max(0.0),
        };
    }
    let p = (s as u32).min(100);
    (
        p,
        if p >= 80 {
            Color::Green
        } else if p >= 60 {
            Color::Yellow
        } else {
            Color::Red
        },
    )
}

/// Pick a border color: cyan when the section is focused, dim gray
/// otherwise. Shared by every section box so focus visuals match.
fn section_border(is_focused: bool) -> Style {
    if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// CLAUDE DJ panel — optional. Rendered only when there's log content
/// or a pending ask. No focus state (not a `DashFocus` variant).
fn render_claude_dj_section<'a>(
    out: &mut Vec<Line<'a>>,
    dj_log: &[String],
    dj_ask: Option<&str>,
    w: usize,
) {
    if dj_log.is_empty() && dj_ask.is_none() {
        return;
    }
    let mut dj_content: Vec<String> = dj_log.to_vec();
    if let Some(ask) = dj_ask {
        dj_content.push(format!("Ask: {ask}█"));
    }
    let dj_strs: Vec<&str> = dj_content.iter().map(|s| s.as_str()).collect();
    out.extend(boxed("CLAUDE DJ  [c] toggle  [/] ask", &dj_strs, w));
    out.push(Line::from(""));
}

/// QUEUE — two-column box with per-row BPM/key compat dot.
fn render_queue_section<'a>(
    out: &mut Vec<Line<'a>>,
    info: &NowPlayingInfo,
    download_in_flight: bool,
    w: usize,
    cw: usize,
    focused: bool,
) {
    let mut items: Vec<(Color, String)> = Vec::new();
    let sc = info.queue.len().min(10);
    let mut pb = info.playing_bpm;
    let mut pk = info.playing_track.as_ref().and_then(|t| t.key.clone());
    for idx in 0..sc {
        let t = &info.queue[idx].track;
        let (pct, clr) = compat(pb, pk.as_deref(), t.bpm, t.key.as_deref());
        let bk = format!(
            "{}/{}",
            t.bpm.map(|b| format!("{:.0}", b)).unwrap_or("?".into()),
            t.key.as_deref().unwrap_or("?")
        );
        let status = if idx == 0 && download_in_flight {
            " ⟳"
        } else if idx == 0 {
            " next"
        } else {
            ""
        };
        let nw = cw.saturating_sub(22 + status.len());
        let nm = ts(&format!("{} - {}", t.artist_name(), t.full_title()), nw);
        items.push((clr, format!("{:>2}  {pct:>3}% {nm} {bk}{status}", idx + 1)));
        pb = t.bpm;
        pk = t.key.clone();
    }
    let overflow = (info.queue.len() > sc).then(|| format!("… {} more", info.queue.len() - sc));
    render_two_col_box(
        out,
        &format!("QUEUE ({}) [q]", info.queue.len()),
        &items,
        overflow.as_deref(),
        "Empty",
        w,
        focused,
    );
    out.push(Line::from(""));
}

/// HISTORY — two-column box with per-row mix compat dot, plus a BPM
/// trend sparkline and total session time in the title bar.
fn render_history_section<'a>(
    out: &mut Vec<Line<'a>>,
    info: &NowPlayingInfo,
    w: usize,
    cw: usize,
    focused: bool,
) {
    let mut items: Vec<(Color, String)> = Vec::new();
    if info.history.len() >= 2 {
        let rec: Vec<_> = info.history.iter().take(10).collect();
        for i in 0..rec.len() {
            let e = rec[i];
            let pb = if i > 0 { rec[i - 1].track.bpm } else { None };
            let pk = if i > 0 {
                rec[i - 1].track.key.as_deref()
            } else {
                None
            };
            let (pct, clr) = compat(pb, pk, e.track.bpm, e.track.key.as_deref());
            let bk = format!(
                "{}/{}",
                e.track
                    .bpm
                    .map(|b| format!("{:.0}", b))
                    .unwrap_or("?".into()),
                e.track.key.as_deref().unwrap_or("?")
            );
            let mix = e.mix_score.map(|s| format!(" m{s}")).unwrap_or_default();
            let nw = cw.saturating_sub(26);
            let nm = ts(
                &format!("{} - {}", e.track.artist_name(), e.track.full_title()),
                nw,
            );
            items.push((clr, format!("{:>2}  {pct:>3}% {nm} {bk}{mix}", i + 1)));
        }
    }
    let bpms: Vec<f64> = info.history.iter().filter_map(|e| e.track.bpm).collect();
    let sparkline: String = if bpms.len() >= 3 {
        let spark = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let mn = bpms.iter().cloned().fold(f64::MAX, f64::min);
        let mx = bpms.iter().cloned().fold(0.0f64, f64::max).max(mn + 1.0);
        let rng = mx - mn;
        let trend: String = bpms
            .iter()
            .map(|b| {
                let i = ((b - mn) / rng * (spark.len() - 1) as f64) as usize;
                spark[i.min(spark.len() - 1)]
            })
            .collect();
        format!(" {trend}")
    } else {
        Default::default()
    };
    let session: String = if info.session_time_min > 0 {
        format!(" {}m", info.session_time_min)
    } else {
        Default::default()
    };
    render_two_col_box(
        out,
        &format!("HISTORY ({}{sparkline}{session}) [h]", info.history.len()),
        &items,
        None,
        "No history yet",
        w,
        focused,
    );
    out.push(Line::from(""));
}

/// BROWSE — mini track/file browser. Click targets are pushed using
/// the current `out.len()` so the rect Y values match the rendered row
/// positions. Returns immediately when `browse_items` is empty so the
/// section disappears in Full layout when nothing's loaded.
// TODO: bundle the 9 args into a BrowseSectionCtx struct — ripples to caller.
// Separate session.
#[allow(clippy::too_many_arguments)]
fn render_browse_section<'a>(
    out: &mut Vec<Line<'a>>,
    area: Rect,
    browse_items: &[String],
    browse_breadcrumb: &str,
    browse_selected: usize,
    browse_is_tracks: bool,
    w: usize,
    click_targets: &mut Vec<crate::tui::app::ClickTarget>,
    focused: bool,
) {
    if browse_items.is_empty() {
        return;
    }
    let item_count = browse_items.iter().take(8).count();
    let box_top_y = area.y + out.len() as u16;
    for i in 0..item_count {
        click_targets.push(crate::tui::app::ClickTarget::new(
            area.x + 1,
            box_top_y + 2 + i as u16,
            w.saturating_sub(2) as u16,
            1,
            crate::tui::app::ClickAction::DashBrowseSelect(i),
        ));
    }
    let bw = w.saturating_sub(2);
    let title = "BROWSE [b]";
    let tl = title.chars().count();
    let tr = bw.saturating_sub(tl + 4).saturating_add(1);
    let cw = bw.saturating_sub(2);
    let browse_bs = section_border(focused);
    out.push(Line::from(Span::styled(
        format!("┌─ {title} {}┐", "─".repeat(tr)),
        browse_bs,
    )));
    out.push(Line::from(vec![
        Span::styled("│ ".to_string(), browse_bs),
        Span::styled(
            ps(browse_breadcrumb, cw),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" │".to_string(), browse_bs),
    ]));
    for (i, item) in browse_items.iter().take(8).enumerate() {
        let marker = if i == browse_selected { "▸" } else { " " };
        let txt = if browse_is_tracks {
            format!("{marker} {:>2}. {item}", i + 1)
        } else {
            format!("{marker} {item}")
        };
        let truncated: String = txt.chars().take(cw).collect();
        let used_w = truncated.chars().count();
        let pad_w = cw.saturating_sub(used_w);
        let style = if i == browse_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::Gray)
        };
        out.push(Line::from(vec![
            Span::styled("│ ".to_string(), browse_bs),
            Span::styled(truncated, style),
            Span::raw(" ".repeat(pad_w)),
            Span::styled(" │".to_string(), browse_bs),
        ]));
    }
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(bw)),
        browse_bs,
    )));
    out.push(Line::from(""));
}

/// LOG — tails `~/.mixr/mixr.log` into a 3..=7-row box that fills any
/// remaining vertical space below the prior sections.
fn render_log_section<'a>(
    out: &mut Vec<Line<'a>>,
    log_scroll_offset: usize,
    w: usize,
    h: usize,
    focused: bool,
) {
    let rem = h.saturating_sub(out.len() + 3);
    let lc = rem.clamp(3, 7);
    let ll = read_logs_offset(lc, log_scroll_offset);
    let log_bs = section_border(focused);
    let log_bw = w.saturating_sub(2);
    let log_title = "LOG  [l] open";
    let log_tr = log_bw.saturating_sub(log_title.len() + 4).saturating_add(1);
    let log_cw = log_bw.saturating_sub(2);
    out.push(Line::from(Span::styled(
        format!("┌─ {log_title} {}┐", "─".repeat(log_tr)),
        log_bs,
    )));
    for line in ll.iter() {
        out.push(Line::from(vec![
            Span::styled("│ ".to_string(), log_bs),
            Span::styled(
                ps(line, log_cw).to_string(),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(" │".to_string(), log_bs),
        ]));
    }
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(log_bw)),
        log_bs,
    )));
}

// TODO: bundle these 19 args into a `DashboardCtx<'a>` struct — ripples to
// every caller and changes the lifetime-of-borrow shape for the ratatui
// Frame mutable borrow. Its own focused session.
#[allow(clippy::too_many_arguments)]
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    info: &NowPlayingInfo,
    wf_mode: WaveformMode,
    browse_items: &[String],
    browse_breadcrumb: &str,
    browse_selected: usize,
    browse_is_tracks: bool,
    show_help: bool,
    sel_section: Option<CtrlSection>,
    download_in_flight: bool,
    dj_log: &[String],
    dj_ask: Option<&str>,
    click_targets: &mut Vec<crate::tui::app::ClickTarget>,
    dash_focus: crate::tui::app::DashFocus,
    log_scroll_offset: usize,
    waveform_zoom: Option<bool>,
    dash_layout: DashLayout,
    panel_section: PanelSection,
) {
    use crate::tui::app::DashFocus;
    let border_style = section_border;
    let ctrl_bs = border_style(dash_focus == DashFocus::Controller);
    let w = area.width as usize;
    let h = area.height as usize;
    if w < 40 || h < 10 {
        return;
    }

    let mut out: Vec<Line> = Vec::new();
    let bw = w.saturating_sub(2); // border width
    // Overhead budget (non-deck-content columns): 25 baseline + 8 for the
    // HMLF EQ/filter strips (4 each side). Increase this whenever you add
    // a new vertical strip or the row layout will overflow bar_w.
    let bar_w = ((bw.saturating_sub(33)) / 2).max(13);
    let dw = bar_w + 2;

    let has_a = info.deck_a_track.is_some();
    let has_b = info.deck_b_track.is_some();
    // Gate on actual playing state, not RMS level — during a mix the
    // incoming's intro often sits near the level threshold and flickers
    // on/off every frame, which made the beat dots flash as they jumped
    // between the `!playing` static pattern and the scrolling one.
    let a_on = has_a && info.deck_a_is_playing;
    let b_on = has_b && info.deck_b_is_playing;
    let both = a_on && b_on;
    let aph = info.phase_offset_ms.abs();
    // Boost factor maps the deck's RMS level (typically 0.3-0.6 on
    // modern loudness-mastered dance tracks) into the meter's 0-1
    // visual range without redlining on every kick. 1.2× lands a
    // typical hot track in the upper-mid (yellow), reserves the red
    // top cell for genuinely peaking signals (RMS > 0.83 ≈ -1.6 dBFS).
    let la = (info.deck_a_level as f64 * 1.2).min(1.0);
    let lb = (info.deck_b_level as f64 * 1.2).min(1.0);

    let dl_a = deck_lines_styled(info, true, dw, bar_w);
    let dl_b = deck_lines_styled(info, false, dw, bar_w);
    let ml = dl_a.len().max(dl_b.len());

    // ┌─ CONTROLLER ────────────── active: tempo A ──┐
    // Right-hand side shows whichever section Tab nav / click has
    // focused. Dim styling so the label fades into the border.
    let ctrl_title = "CONTROLLER";
    let active_label = sel_section
        .map(|s| format!("active: {}", s.display_name()))
        .unwrap_or_default();
    // Total line = bw + 2 chars (outer borders included). Fixed chars:
    //   "┌─ {title} " = 3 + title_len + 1  (trailing space)
    //   "─┐"          = 2
    //   " {active_label} " when active = 2 + label_len
    // Sum (no active): 6 + title_len.
    // Sum (active):    8 + title_len + label_len.
    let fixed = 6
        + ctrl_title.chars().count()
        + if active_label.is_empty() {
            0
        } else {
            active_label.chars().count() + 2
        };
    let fill = (bw + 2).saturating_sub(fixed).max(1);
    let mut spans: Vec<Span> = vec![
        Span::styled(format!("┌─ {ctrl_title} "), ctrl_bs),
        Span::styled("─".repeat(fill), ctrl_bs),
    ];
    if !active_label.is_empty() {
        spans.push(Span::styled(
            format!(" {active_label} "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::styled("─┐".to_string(), ctrl_bs));
    out.push(Line::from(spans));
    out.push(Line::from(Span::styled(
        format!("│{}│", " ".repeat(bw)),
        ctrl_bs,
    )));

    // Click + drag targets for the vertical controls in the CONTROLLER
    // row. Tempo and Volume stay as upfaders (continuous vertical
    // strips). EQ and Filter are text readouts stacked 4-high next
    // to each volume fader:
    //     H+0     (row 0)
    //     M+0     (row 1)
    //     L+0     (row 2)
    //     F+0     (row 3)
    // Each readout cell focuses its EQ band on click; scroll wheel
    // adjusts once focused. Row layout:
    //     │ T  [deckA]   EEEE V VU d─d VU V EEEE   [deckB]  T │
    // where EEEE is the 4-char readout block.
    {
        use crate::tui::app::{ClickAction, ClickTarget, RangeControl};
        use CtrlSection::*;
        let y0 = area.y + out.len() as u16; // first ml-row
        let y_min = y0;
        let y_max = y0 + ml as u16; // exclusive

        let ta_x = area.x + 2; // "│ " then T
        let tb_x = area.x + area.width - 3; // T then " │"
        // Layout rebalanced: 2-space outer gap (deck↔EQ) + 1-space inner
        // gap (EQ↔V) on both sides. Was 3+0; shifted 1 outward per side.
        //   "│ T  [deckA bar_w]  EEEE " = 2+1+2+bar_w+2 = 7+bar_w
        let eq_a_x = area.x + 7 + bar_w as u16;
        let va_x = eq_a_x + 4 + 1; // area.x + 12 + bar_w
        // Mirror on the B side.
        let vb_x = area.x + area.width - 13 - bar_w as u16;
        let eq_b_x = vb_x + 2; // 1-space gap then readout

        let vert = |x: u16,
                    control: RangeControl,
                    label: &'static str,
                    midi: crate::midi::Action|
         -> ClickTarget {
            ClickTarget::new(
                x,
                y_min,
                1,
                ml as u16,
                ClickAction::SetVerticalRange {
                    control,
                    y_min,
                    y_max,
                },
            )
            .labeled(label)
            .bindable(midi)
        };
        click_targets.push(vert(
            ta_x,
            RangeControl::TempoA,
            "tempo_a",
            crate::midi::Action::Tempo { is_a: true },
        ));
        click_targets.push(vert(
            va_x,
            RangeControl::VolumeA,
            "volume_a",
            crate::midi::Action::ChannelFader { is_a: true },
        ));
        click_targets.push(vert(
            vb_x,
            RangeControl::VolumeB,
            "volume_b",
            crate::midi::Action::ChannelFader { is_a: false },
        ));
        click_targets.push(vert(
            tb_x,
            RangeControl::TempoB,
            "tempo_b",
            crate::midi::Action::Tempo { is_a: false },
        ));

        // EQ/filter readout cells: 4 chars wide, 1 row tall. Rows
        // 0/2/4/6 hold H/M/L/F; the blank rows between (1/3/5) are
        // also hit-targets for the nearest band so clicking the gap
        // still does something sensible. Click focuses the section;
        // scroll wheel (= arrow keys) adjusts.
        let cell = |x: u16,
                    row_y: u16,
                    h: u16,
                    section: CtrlSection,
                    label: &'static str,
                    midi: crate::midi::Action|
         -> ClickTarget {
            ClickTarget::new(x, row_y, 4, h, ClickAction::FocusDashSection(section))
                .labeled(label)
                .bindable(midi)
        };
        if ml >= 7 {
            // 2-row band per readout (text row + separator below it)
            // keeps hit-boxes generous at 121-col terminals.
            click_targets.push(cell(
                eq_a_x,
                y0,
                2,
                EqHighA,
                "eq_high_a",
                crate::midi::Action::EqHigh { is_a: true },
            ));
            click_targets.push(cell(
                eq_a_x,
                y0 + 2,
                2,
                EqMidA,
                "eq_mid_a",
                crate::midi::Action::EqMid { is_a: true },
            ));
            click_targets.push(cell(
                eq_a_x,
                y0 + 4,
                2,
                EqLowA,
                "eq_low_a",
                crate::midi::Action::EqLow { is_a: true },
            ));
            click_targets.push(cell(
                eq_a_x,
                y0 + 6,
                1,
                FilterA,
                "filter_a",
                crate::midi::Action::Filter { is_a: true },
            ));
            click_targets.push(cell(
                eq_b_x,
                y0,
                2,
                EqHighB,
                "eq_high_b",
                crate::midi::Action::EqHigh { is_a: false },
            ));
            click_targets.push(cell(
                eq_b_x,
                y0 + 2,
                2,
                EqMidB,
                "eq_mid_b",
                crate::midi::Action::EqMid { is_a: false },
            ));
            click_targets.push(cell(
                eq_b_x,
                y0 + 4,
                2,
                EqLowB,
                "eq_low_b",
                crate::midi::Action::EqLow { is_a: false },
            ));
            click_targets.push(cell(
                eq_b_x,
                y0 + 6,
                1,
                FilterB,
                "filter_b",
                crate::midi::Action::Filter { is_a: false },
            ));
        }
    }

    let empty_line: (Vec<Span>, usize) = (vec![], 0);
    for i in 0..ml {
        let (l_spans, l_w) = dl_a.get(i).unwrap_or(&empty_line);
        let (r_spans, r_w) = dl_b.get(i).unwrap_or(&empty_line);
        let rfb = ml.saturating_sub(1).saturating_sub(i);
        let cr = ml / 2;

        // Apply underline to the fader column whose section is focused
        // so the user gets a visual of "this is the active control" that
        // matches the button-label underline.
        let with_underline = |sp: Span<'static>, section: CtrlSection| -> Span<'static> {
            if sel_section == Some(section) {
                Span::styled(sp.content, sp.style.add_modifier(Modifier::UNDERLINED))
            } else {
                sp
            }
        };
        let ta = with_underline(fader_ch(i, ml, info.deck_a_rate), CtrlSection::TempoA);
        let tb = with_underline(fader_ch(i, ml, info.deck_b_rate), CtrlSection::TempoB);
        let va = with_underline(
            fader_ch_vol(i, ml, info.channel_fader_a),
            CtrlSection::VolumeA,
        );
        let vb = with_underline(
            fader_ch_vol(i, ml, info.channel_fader_b),
            CtrlSection::VolumeB,
        );
        let ua = vu_ch(rfb, (la * ml as f64) as usize, ml);
        let ub = vu_ch(rfb, (lb * ml as f64) as usize, ml);
        let da = bdot(
            i,
            cr,
            a_on,
            has_a,
            both,
            aph,
            info.deck_a_beat_pos,
            info.downbeat_aligned,
        );
        let db = bdot(
            i,
            cr,
            b_on,
            has_b,
            both,
            aph,
            info.deck_b_beat_pos,
            info.downbeat_aligned,
        );
        let cl = if i == cr {
            if both {
                Span::styled(
                    "─",
                    Style::default().fg(pc_aligned(info.phase_offset_ms, info.downbeat_aligned)),
                )
            } else {
                dim("─")
            }
        } else {
            Span::raw(" ")
        };

        // Pad deck content to bar_w with trailing spaces
        let l_pad = bar_w.saturating_sub(*l_w);
        let r_pad = bar_w.saturating_sub(*r_w);

        // EQ + filter readouts: vertical stack next to each volume
        // fader, 4 chars wide × 7 rows tall. H/M/L/F on even rows
        // (0, 2, 4, 6) with blank separator rows between. Rows 7+
        // are blank. Deck A left-aligns the text (trailing space sits
        // adjacent to V); deck B right-aligns (leading space sits
        // adjacent to V) so both sides read symmetrically.
        let eq_cell = |row_idx: usize, is_a: bool| -> String {
            let (l, m, h, f) = if is_a {
                (
                    info.deck_a_eq_low_db,
                    info.deck_a_eq_mid_db,
                    info.deck_a_eq_high_db,
                    info.deck_a_filter_pos,
                )
            } else {
                (
                    info.deck_b_eq_low_db,
                    info.deck_b_eq_mid_db,
                    info.deck_b_eq_high_db,
                    info.deck_b_filter_pos,
                )
            };
            // Filter text: scale −1..+1 to ±9 so it fits 3 chars like EQ.
            let f_scaled = (f as f64 * 9.0).round() as i32;
            let raw = match row_idx {
                0 => format!("H{h:+.0}"),
                2 => format!("M{m:+.0}"),
                4 => format!("L{l:+.0}"),
                6 => format!("F{f_scaled:+}"),
                _ => String::new(),
            };
            if is_a {
                format!("{raw:<4}")
            } else {
                format!("{raw:>4}")
            }
        };
        let eq_a_txt = eq_cell(i, true);
        let eq_b_txt = eq_cell(i, false);

        // Underline the EQ/filter cell whose section is currently
        // focused — matches the sl() underline pattern applied to
        // CUE / JUMP / NUDGE / PLAY buttons.
        let eq_style = |is_a: bool, row_idx: usize| -> Style {
            use CtrlSection::*;
            let section = match (is_a, row_idx) {
                (true, 0) => Some(EqHighA),
                (true, 2) => Some(EqMidA),
                (true, 4) => Some(EqLowA),
                (true, 6) => Some(FilterA),
                (false, 0) => Some(EqHighB),
                (false, 2) => Some(EqMidB),
                (false, 4) => Some(EqLowB),
                (false, 6) => Some(FilterB),
                _ => None,
            };
            let base = Style::default().fg(Color::Gray);
            if section == sel_section && section.is_some() {
                base.add_modifier(Modifier::UNDERLINED)
            } else {
                base
            }
        };

        // Row: │ T  [deckA]   <eq_A>V VU d─d VU V<eq_B>   [deckB]  T │
        let mut row: Vec<Span> = vec![Span::styled("│ ".to_string(), ctrl_bs), ta, Span::raw("  ")];
        row.extend(l_spans.iter().cloned());
        if l_pad > 0 {
            row.push(Span::raw(" ".repeat(l_pad)));
        }
        row.extend([
            Span::raw("  "),
            Span::styled(eq_a_txt, eq_style(true, i)),
            Span::raw(" "),
            va,
            Span::raw(" "),
            ua,
            Span::raw(" "),
            da,
            cl,
            db,
            Span::raw(" "),
            ub,
            Span::raw(" "),
            vb,
            Span::raw(" "),
            Span::styled(eq_b_txt, eq_style(false, i)),
            Span::raw("  "),
        ]);
        row.extend(r_spans.iter().cloned());
        if r_pad > 0 {
            row.push(Span::raw(" ".repeat(r_pad)));
        }
        // Fixed before end_pad: 2 (│ ) + 1 (T) + 2 + bar_w + 25 middle (incl. 2×4 EQ) + bar_w = 30 + 2*bar_w.
        // After end_pad: 2 + 1 + 2 (" │") = 5.
        let end_pad = (bw + 2).saturating_sub(30 + 2 * bar_w + 5);
        if end_pad > 0 {
            row.push(Span::raw(" ".repeat(end_pad)));
        }
        row.extend([Span::raw("  "), tb, Span::styled(" │".to_string(), ctrl_bs)]);

        out.push(Line::from(row));
    }

    out.push(Line::from(Span::styled(
        format!("│{}│", " ".repeat(bw)),
        ctrl_bs,
    )));

    // Stacked waveform comparison (inside box, when both decks loaded and mode != off)
    if has_a && has_b && wf_mode != WaveformMode::Off {
        let wfw = bw.saturating_sub(8).max(10);
        let zoom_wfw = bw.saturating_sub(6).max(10); // wider when zoomed
        let ea = info
            .deck_a_bpm
            .map(|b| format!("{:.1}", b))
            .unwrap_or("—".into());
        let eb = info
            .deck_b_bpm
            .map(|b| format!("{:.1}", b))
            .unwrap_or("—".into());
        let ph_s = format!("{:+.1}ms", info.phase_offset_ms);
        let zoom_label = match waveform_zoom {
            Some(true) => "  [zoomed A — click/Esc to close]",
            Some(false) => "  [zoomed B — click/Esc to close]",
            None => "",
        };
        let hdr = format!(
            " [{}]  A({ea}) → B({eb})  phase: {ph_s}  [w] mode{zoom_label}",
            wf_mode.label()
        );
        out.push(boxed_row("", bw, ctrl_bs));
        out.push(boxed_row(&hdr, bw, ctrl_bs));

        // Track the Y position of waveform rows for click targets
        let wf_row_y = area.y + out.len() as u16;

        if let Some(is_a) = waveform_zoom {
            // Zoomed view: show one deck's waveform at full width
            let (time, dur, label) = if is_a {
                (info.deck_a_time, info.deck_a_duration, "A")
            } else {
                (info.deck_b_time, info.deck_b_duration, "B")
            };
            match wf_mode {
                WaveformMode::Phrase => {
                    let empty_phr: Vec<Phrase> = Vec::new();
                    let phr = if is_a {
                        info.deck_a_analysis
                            .as_ref()
                            .map(|a| &a.phrases[..])
                            .unwrap_or(&empty_phr)
                    } else {
                        info.deck_b_analysis
                            .as_ref()
                            .map(|a| &a.phrases[..])
                            .unwrap_or(&empty_phr)
                    };
                    let sp = render_sparkline(phr, time, dur, zoom_wfw, label);
                    out.push(boxed_row(&format!("  {sp}"), bw, ctrl_bs));
                }
                WaveformMode::Audio => {
                    let empty_wf: Vec<f32> = Vec::new();
                    let wf = if is_a {
                        info.deck_a_analysis
                            .as_ref()
                            .map(|a| &a.waveform_peaks[..])
                            .unwrap_or(&empty_wf)
                    } else {
                        info.deck_b_analysis
                            .as_ref()
                            .map(|a| &a.waveform_peaks[..])
                            .unwrap_or(&empty_wf)
                    };
                    let rendered = render_ascii_waveform(wf, time, dur, zoom_wfw, label);
                    out.push(boxed_row(&format!("  {rendered}"), bw, ctrl_bs));
                }
                WaveformMode::Off => {}
            }
            // Click target for the zoomed row (closes zoom)
            click_targets.push(
                crate::tui::app::ClickTarget::new(
                    area.x,
                    wf_row_y,
                    area.width,
                    1,
                    crate::tui::app::ClickAction::WaveformZoom(is_a),
                )
                .labeled(if is_a {
                    "waveform_zoom_a"
                } else {
                    "waveform_zoom_b"
                }),
            );
        } else {
            // Normal stacked view
            match wf_mode {
                WaveformMode::Phrase => {
                    let empty_phr: Vec<Phrase> = Vec::new();
                    let a_phr = info
                        .deck_a_analysis
                        .as_ref()
                        .map(|a| &a.phrases[..])
                        .unwrap_or(&empty_phr);
                    let b_phr = info
                        .deck_b_analysis
                        .as_ref()
                        .map(|a| &a.phrases[..])
                        .unwrap_or(&empty_phr);
                    let sp_a =
                        render_sparkline(a_phr, info.deck_a_time, info.deck_a_duration, wfw, "A");
                    let sp_b =
                        render_sparkline(b_phr, info.deck_b_time, info.deck_b_duration, wfw, "B");
                    out.push(boxed_row(&format!("  {sp_a}"), bw, ctrl_bs));
                    out.push(boxed_row(&format!("  {sp_b}"), bw, ctrl_bs));
                }
                WaveformMode::Audio => {
                    let empty_wf: Vec<f32> = Vec::new();
                    let a_wf = info
                        .deck_a_analysis
                        .as_ref()
                        .map(|a| &a.waveform_peaks[..])
                        .unwrap_or(&empty_wf);
                    let b_wf = info
                        .deck_b_analysis
                        .as_ref()
                        .map(|a| &a.waveform_peaks[..])
                        .unwrap_or(&empty_wf);
                    let wf_a = render_ascii_waveform(
                        a_wf,
                        info.deck_a_time,
                        info.deck_a_duration,
                        wfw,
                        "A",
                    );
                    let wf_b = render_ascii_waveform(
                        b_wf,
                        info.deck_b_time,
                        info.deck_b_duration,
                        wfw,
                        "B",
                    );
                    out.push(boxed_row(&format!("  {wf_a}"), bw, ctrl_bs));
                    out.push(boxed_row(&format!("  {wf_b}"), bw, ctrl_bs));
                }
                WaveformMode::Off => {}
            }
            // Click targets for waveform rows A and B
            if wf_mode != WaveformMode::Off {
                click_targets.push(
                    crate::tui::app::ClickTarget::new(
                        area.x,
                        wf_row_y,
                        area.width,
                        1,
                        crate::tui::app::ClickAction::WaveformZoom(true),
                    )
                    .labeled("waveform_zoom_a"),
                );
                click_targets.push(
                    crate::tui::app::ClickTarget::new(
                        area.x,
                        wf_row_y + 1,
                        area.width,
                        1,
                        crate::tui::app::ClickAction::WaveformZoom(false),
                    )
                    .labeled("waveform_zoom_b"),
                );
            }
        }
    }

    out.push(Line::from(Span::styled(
        format!("│{}│", " ".repeat(bw)),
        ctrl_bs,
    )));

    // === Crossfader + Phase (centered in box) ===
    let mw = (w / 3).clamp(10, 28);
    // Visual position tracks the transition volume curves so EchoOut snaps fast
    // at the cut and again when the incoming comes in; BeatMatched sweeps smoothly.
    let xfv = info.crossfader_visual;
    let xf_pos = ((mw as f64 * xfv) as usize).min(mw.saturating_sub(1));

    let content_w = bw.saturating_sub(2); // inside │ … │

    // The EQ band's visual centre (the `─` between the two channels) sits at the
    // midpoint of the two volume faders: `(va_x + vb_x) / 2 = area.x +
    // (area.width-1)/2` (see the CONTROLLER click-target layout — va_x/vb_x).
    // Anchor the crossfader + phase meters on that SAME column so they line up
    // with the EQ centre at every window width, instead of on `content_w/2`
    // (a separate rounding that only coincided at some widths). Content column 0
    // maps to absolute `area.x + 2` (the "│ " border), so subtract 2.
    let eq_center_col = ((area.width as usize).saturating_sub(1) / 2).saturating_sub(2);

    // Crossfader and phase meter — both just the meter line (no A/B labels in center)
    // A/B labels go into left/right control strings so the meter itself is centered
    let mut xf_meter = String::new();
    for j in 0..mw {
        xf_meter.push(if j == xf_pos { '║' } else { '─' });
    }

    let cx = mw / 2;
    let on = (info.phase_offset_ms / 30.0).clamp(-1.0, 1.0);
    let needle = ((cx as f64 + on * cx as f64) as usize).min(mw.saturating_sub(1));
    let mut ph_meter = String::new();
    for j in 0..mw {
        if j == cx {
            ph_meter.push('│');
        } else if j == needle && info.phase_offset_ms.abs() > 3.0 {
            ph_meter.push('▼');
        } else {
            ph_meter.push('─');
        }
    }
    let pv = format!("{:+.1}ms", info.phase_offset_ms);

    // Include A/B and phase value as part of left/right controls
    // Section highlight helper
    let ss = sel_section;
    let sl = |s: CtrlSection, text: &str| -> String {
        if ss == Some(s) {
            format!("[{text}]")
        } else {
            text.to_string()
        }
    };

    // Hot-cue dots (● 1  ○ 2  ○ 3  ● 4) live on each side of the
    // crossfader — deck A to the left, deck B to the right. Click
    // a dot to jump to that cue; Shift-click to set.
    let cue_dots = |cues: [bool; 4]| -> String {
        let mut s = String::new();
        for (i, has_cue) in cues.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            } // 2 spaces between slots
            s.push(if *has_cue { '●' } else { '○' });
            s.push(' '); // gap between glyph and digit
            s.push(char::from_digit(i as u32 + 1, 10).unwrap());
        }
        s
    };
    let cue_dots_a = cue_dots(info.deck_a_cues);
    let cue_dots_b = cue_dots(info.deck_b_cues);
    // Left side: CUE label — JUMP button — cue dots (inside, nearest crossfader).
    // Right side: mirror.
    // Embed current jump-bars value in the label so the dashboard
    // shows "◀ JUMP 8 ▶". Click the middle (the number / "JUMP"
    // text) to cycle 4 → 8 → 16 → 32 → 4.
    let jump_label = format!("◀ JUMP {} ▶", info.jump_bars);
    let ctrl_l1 = format!(
        "{}   {}   {}",
        sl(CtrlSection::CueA, "CUE"),
        sl(CtrlSection::JumpA, &jump_label),
        cue_dots_a
    );
    let ctrl_r1 = format!(
        "{}   {}   {}",
        cue_dots_b,
        sl(CtrlSection::JumpB, &jump_label),
        sl(CtrlSection::CueB, "CUE")
    );
    let xf_center = format!("A {xf_meter} B");

    // Loop button strip per deck — appended INSIDE the NUDGE buttons
    // (between NUDGE and the center). Format kept fixed so we can
    // push click targets at known column offsets after the row is
    // built. Layout: "1  2  4  8  16  off" = 19 chars.
    let loop_strip = "1  2  4  8  16  off";
    let ctrl_l2 = format!(
        "{}   {}   {}",
        sl(CtrlSection::PlayA, "PLAY"),
        sl(CtrlSection::NudgeA, "◀ NUDGE ▶"),
        loop_strip
    );
    let ctrl_r2 = format!(
        "{}   {}   {}",
        loop_strip,
        sl(CtrlSection::NudgeB, "◀ NUDGE ▶"),
        sl(CtrlSection::PlayB, "PLAY")
    );
    let ph_center = ph_meter.clone();

    // Row above the crossfader: percent when mixing, countdown when armed.
    // State → Crossfading shows "XX%"; otherwise, when a mix trigger is
    // pending (teleport armed or approaching end-of-track), show
    // "MIX IN N bars" so the user knows how long till the crossfade fires.
    if info.state == EngineState::Crossfading {
        // Transition type + on-beat bar.beat countdown anchored to the
        // visual fader sweep (not raw crossfade_progress). For EchoOut
        // the fader completes at 40% progress, so counting off the
        // total controller window left "5.1 bars remaining" showing
        // while the fader was already parked on B. Using fader sweep
        // progress makes the countdown hit 0 exactly when the fader
        // lands — matches what the user sees.
        let total_beats = info.transition_bars * 4;
        let elapsed_beats = (info.fader_sweep_progress * total_beats as f64)
            .floor()
            .clamp(0.0, total_beats as f64) as u32;
        let remaining = total_beats.saturating_sub(elapsed_beats);
        let bars = remaining / 4;
        let beats = remaining % 4;
        let label = format!("{} {bars}.{beats}", info.transition_type_name);
        out.push(lit_boxed_row(
            &build_ctrl_row_at("", &label, "", content_w, eq_center_col),
            bw,
            ctrl_bs,
        ));
    } else if let Some(trigger) = info.mix_point_time {
        let remaining = trigger - info.playing_time;
        if remaining > 0.0 {
            let bar_dur = info.playing_bpm.map(|b| 60.0 / b * 4.0).unwrap_or(1.8);
            let bars = (remaining / bar_dur).ceil() as i32;
            let label = if bars > 1 {
                format!("MIX IN {bars} bars")
            } else if remaining > 1.0 {
                "MIX NEXT BAR".into()
            } else {
                "MIX NOW".into()
            };
            out.push(lit_boxed_row(
                &build_ctrl_row_at("", &label, "", content_w, eq_center_col),
                bw,
                ctrl_bs,
            ));
        }
    }
    // Push a click target for the crossfader meter so the user can
    // click anywhere along the bar to set position. xf_center is
    // "A {meter} B" (mw + 4 chars), centered in content_w. Box border
    // takes columns area.x (│) + 1 (space). Click X within the meter
    // span maps linearly to crossfader [-1, +1].
    {
        let xf_row = out.len() as u16;
        // Meter is `mw` wide and centred on `eq_center_col` (matches the render
        // below), so it starts mw/2 to the left of that centre.
        let meter_start_in_content = (eq_center_col as u16).saturating_sub(mw as u16 / 2);
        let x_min = area.x + 2 + meter_start_in_content; // 2 = "│ "
        let x_max = x_min + mw as u16;
        click_targets.push(
            crate::tui::app::ClickTarget::new(
                x_min,
                area.y + xf_row,
                mw as u16,
                1,
                crate::tui::app::ClickAction::SetCrossfaderRange { x_min, x_max },
            )
            .labeled("crossfader")
            .bindable(crate::midi::Action::Crossfader),
        );
    }
    let row1 = build_ctrl_row_at(&ctrl_l1, &xf_center, &ctrl_r1, content_w, eq_center_col);
    let row2 = build_ctrl_row_at(&ctrl_l2, &ph_center, &ctrl_r2, content_w, eq_center_col);

    // Push click targets for the dashboard control labels in row1/row2.
    // Each target maps to a synthetic key press that reuses the
    // existing keyboard handler — no parallel logic. Coordinates are
    // (label_position_in_row + box_left_padding) measured against
    // area.x; row index = current out.len().
    use crate::tui::app::{ClickAction, ClickTarget};
    use crossterm::event::KeyCode;
    let row_x_base = area.x + 2; // "│ " border
    // First-occurrence push: labeled clickable target at the first byte-
    // match of `label` in `row_str`. Returns the byte position past the
    // match so callers can hunt for the second occurrence (right deck).
    let push_first = |targets: &mut Vec<ClickTarget>,
                      row_str: &str,
                      label: &str,
                      y: u16,
                      key: KeyCode,
                      tag: &'static str|
     -> Option<usize> {
        let byte_pos = row_str.find(label)?;
        let char_col: usize = row_str[..byte_pos].chars().count();
        let label_w = label.chars().count() as u16;
        targets.push(
            ClickTarget::new(
                row_x_base + char_col as u16,
                y,
                label_w.max(1),
                1,
                ClickAction::SimulateKey(key),
            )
            .labeled(tag),
        );
        Some(byte_pos + label.len())
    };
    let push_second = |targets: &mut Vec<ClickTarget>,
                       row_str: &str,
                       label: &str,
                       y: u16,
                       key: KeyCode,
                       tag: &'static str,
                       after: usize| {
        if let Some(rel) = row_str[after..].find(label) {
            let byte_pos = after + rel;
            let char_col: usize = row_str[..byte_pos].chars().count();
            let label_w = label.chars().count() as u16;
            targets.push(
                ClickTarget::new(
                    row_x_base + char_col as u16,
                    y,
                    label_w.max(1),
                    1,
                    ClickAction::SimulateKey(key),
                )
                .labeled(tag),
            );
        }
    };
    let row1_y = area.y + out.len() as u16;
    let row2_y = area.y + out.len() as u16 + 1;
    // Row 1: CUE_A / JUMP_A / xf / JUMP_B / CUE_B
    if let Some(after) = push_first(click_targets, &row1, "CUE", row1_y, KeyCode::Null, "cue_a") {
        // Override action: CUE labels use FocusDashSection, not SimulateKey.
        // Right-click maps the main cue set/jump button (slot 0).
        if let Some(last) = click_targets.last_mut() {
            last.action = ClickAction::FocusDashSection(CtrlSection::CueA);
            last.midi_action = Some(crate::midi::Action::CueSet {
                is_a: true,
                slot: 0,
            });
        }
        push_second(
            click_targets,
            &row1,
            "CUE",
            row1_y,
            KeyCode::Null,
            "cue_b",
            after,
        );
        if let Some(last) = click_targets.last_mut()
            && last.label == Some("cue_b")
        {
            last.action = ClickAction::FocusDashSection(CtrlSection::CueB);
            last.midi_action = Some(crate::midi::Action::CueSet {
                is_a: false,
                slot: 0,
            });
        }
    }
    // JUMP labels are split into 3 sub-targets: ◀ (back) / middle
    // (cycle bars) / ▶ (forward). Search for the full label string,
    // then derive sub-rects from char positions.
    {
        let label_str = jump_label.clone(); // "◀ JUMP 8 ▶"
        let label_chars = label_str.chars().count() as u16;
        let push_jump_targets = |targets: &mut Vec<ClickTarget>, byte_pos: usize, is_a: bool| {
            let char_col = row1[..byte_pos].chars().count() as u16;
            let x_left = row_x_base + char_col; // ◀
            let x_mid = row_x_base + char_col + 1; // " JUMP N "
            let mid_w = label_chars.saturating_sub(2);
            let x_right = row_x_base + char_col + label_chars - 1; // ▶
            // Jump keys are global — same `<`/`>` for both decks.
            // Right-click each arrow to MIDI-bind the per-deck jump.
            targets.push(
                ClickTarget::new(
                    x_left,
                    row1_y,
                    1,
                    1,
                    ClickAction::SimulateKey(KeyCode::Char('<')),
                )
                .labeled(if is_a { "jump_back_a" } else { "jump_back_b" })
                .bindable(crate::midi::Action::JumpBarsDeck { is_a, bars: -8 }),
            );
            targets.push(
                ClickTarget::new(x_mid, row1_y, mid_w, 1, ClickAction::CycleJumpBars)
                    .labeled(if is_a { "jump_cycle_a" } else { "jump_cycle_b" }),
            );
            targets.push(
                ClickTarget::new(
                    x_right,
                    row1_y,
                    1,
                    1,
                    ClickAction::SimulateKey(KeyCode::Char('>')),
                )
                .labeled(if is_a { "jump_fwd_a" } else { "jump_fwd_b" })
                .bindable(crate::midi::Action::JumpBarsDeck { is_a, bars: 8 }),
            );
        };
        if let Some(byte_pos_a) = row1.find(label_str.as_str()) {
            push_jump_targets(click_targets, byte_pos_a, true);
            let after = byte_pos_a + label_str.len();
            if let Some(rel) = row1[after..].find(label_str.as_str()) {
                push_jump_targets(click_targets, after + rel, false);
            }
        }
    }
    // Row 2: PLAY_A / NUDGE_A / phase / NUDGE_B / PLAY_B
    if let Some(after) = push_first(
        click_targets,
        &row2,
        "PLAY",
        row2_y,
        KeyCode::Char('p'),
        "play_a",
    ) {
        if let Some(last) = click_targets.last_mut() {
            last.midi_action = Some(crate::midi::Action::PlayPauseDeck { is_a: true });
        }
        push_second(
            click_targets,
            &row2,
            "PLAY",
            row2_y,
            KeyCode::Char('p'),
            "play_b",
            after,
        );
        if let Some(last) = click_targets.last_mut()
            && last.label == Some("play_b")
        {
            last.midi_action = Some(crate::midi::Action::PlayPauseDeck { is_a: false });
        }
    }
    // NUDGE labels span "◀ NUDGE ▶". Split into 3 sub-targets like
    // JUMP — left arrow back-nudge, middle label inert (no MIDI
    // action; click would focus the section if we wanted), right
    // arrow forward-nudge. Per-deck NudgeDeck binding on each arrow
    // so the user can map ◀ and ▶ separately.
    {
        let label_str = "◀ NUDGE ▶";
        let label_chars = label_str.chars().count() as u16;
        let push_nudge_targets = |targets: &mut Vec<ClickTarget>, byte_pos: usize, is_a: bool| {
            let char_col = row2[..byte_pos].chars().count() as u16;
            let x_left = row_x_base + char_col;
            let x_mid = row_x_base + char_col + 1;
            let mid_w = label_chars.saturating_sub(2);
            let x_right = row_x_base + char_col + label_chars - 1;
            // Left arrow → back-nudge. `[` and `]` are the global
            // nudge keys; the simulate-key path keeps left-click
            // behavior. Right-click maps NudgeDeck per direction.
            targets.push(
                ClickTarget::new(
                    x_left,
                    row2_y,
                    1,
                    1,
                    ClickAction::SimulateKey(KeyCode::Char('[')),
                )
                .labeled(if is_a { "nudge_back_a" } else { "nudge_back_b" })
                .bindable(crate::midi::Action::NudgeDeck {
                    is_a,
                    direction: -1,
                }),
            );
            // Middle "NUDGE" label is inert — no obvious left-click
            // semantic and no MIDI action. Skip pushing entirely so
            // right-clicking it does nothing rather than something
            // surprising.
            let _ = (x_mid, mid_w);
            targets.push(
                ClickTarget::new(
                    x_right,
                    row2_y,
                    1,
                    1,
                    ClickAction::SimulateKey(KeyCode::Char(']')),
                )
                .labeled(if is_a { "nudge_fwd_a" } else { "nudge_fwd_b" })
                .bindable(crate::midi::Action::NudgeDeck { is_a, direction: 1 }),
            );
        };
        if let Some(byte_pos_a) = row2.find(label_str) {
            push_nudge_targets(click_targets, byte_pos_a, true);
            let after = byte_pos_a + label_str.len();
            if let Some(rel) = row2[after..].find(label_str) {
                push_nudge_targets(click_targets, after + rel, false);
            }
        }
    }

    // Hot-cue dot click targets on row1 — ●N / ○N patterns appear
    // twice (left deck, right deck). Walk forward through the string
    // so the second occurrence of each slot maps to deck B.
    // Cue glyphs are now "● 1" / "○ 1" (3 chars wide — glyph + gap +
    // digit) so click targets need to match the new pattern and
    // span 3 columns.
    for slot in 1..=4u32 {
        let key = char::from_digit(slot, 10).unwrap();
        let filled = format!("● {key}");
        let empty = format!("○ {key}");
        let mut cursor = 0;
        for is_a in [true, false] {
            let byte_pos = {
                let haystack = &row1[cursor..];
                let f = haystack.find(&filled).map(|p| (p, filled.len()));
                let e = haystack.find(&empty).map(|p| (p, empty.len()));
                match (f, e) {
                    (Some((pf, lf)), Some((pe, _))) if pf <= pe => Some((cursor + pf, lf)),
                    (Some((pf, lf)), None) => Some((cursor + pf, lf)),
                    (_, Some((pe, le))) => Some((cursor + pe, le)),
                    _ => None,
                }
            };
            if let Some((bp, len)) = byte_pos {
                let col = row1[..bp].chars().count() as u16;
                let tag: &'static str = match (is_a, slot) {
                    (true, 1) => "hot_cue_1_a",
                    (true, 2) => "hot_cue_2_a",
                    (true, 3) => "hot_cue_3_a",
                    (true, 4) => "hot_cue_4_a",
                    (false, 1) => "hot_cue_1_b",
                    (false, 2) => "hot_cue_2_b",
                    (false, 3) => "hot_cue_3_b",
                    (false, 4) => "hot_cue_4_b",
                    _ => "hot_cue_unknown",
                };
                click_targets.push(
                    ClickTarget::new(
                        row_x_base + col,
                        row1_y,
                        3,
                        1,
                        ClickAction::SimulateKey(KeyCode::Char(key)),
                    )
                    .labeled(tag)
                    .bindable(crate::midi::Action::Cue {
                        is_a,
                        slot: (slot - 1) as u8,
                    }),
                );
                cursor = bp + len;
            }
        }
    }

    out.push(lit_boxed_row(&row1, bw, ctrl_bs));
    out.push(lit_boxed_row(&row2, bw, ctrl_bs));

    // Loop button click targets on row2. The strip "1  2  4  8  16
    // off" sits inside the NUDGE button on each deck (after a 3-space
    // gap). Layout in ctrl_l2:
    //   "PLAY   ◀ NUDGE ▶   1  2  4  8  16  off"
    // chars: 4 + 3 + "◀ NUDGE ▶"(9) + 3 = 19 prefix; loop strip 19.
    {
        let row2_y = area.y + out.len() as u16 - 1; // row2 was just pushed
        let row_x_base = area.x + 2; // "│ "
        // Per-button offsets within "1  2  4  8  16  off" (19 chars):
        //   1:0(w1)  2:3(w1)  4:6(w1)  8:9(w1)  16:12(w2)  off:16(w3)
        let buttons: [(f64, u16, u16); 5] = [
            (1.0, 0, 1),
            (2.0, 3, 1),
            (4.0, 6, 1),
            (8.0, 9, 1),
            (16.0, 12, 2),
        ];
        let off_off: u16 = 16;
        let off_w: u16 = 3;
        // Deck A loop strip starts inside ctrl_l2 at column 19 (PLAY=4
        // + 3 + "◀ NUDGE ▶"=9 + 3 = 19).
        let a_strip_x = row_x_base + 19;
        for (beats, off_in_strip, w) in buttons {
            click_targets.push(
                crate::tui::app::ClickTarget::new(
                    a_strip_x + off_in_strip,
                    row2_y,
                    w,
                    1,
                    crate::tui::app::ClickAction::LoopEngageDeck { is_a: true, beats },
                )
                .labeled(match beats as u32 {
                    1 => "loop_1_a",
                    2 => "loop_2_a",
                    4 => "loop_4_a",
                    8 => "loop_8_a",
                    16 => "loop_16_a",
                    _ => "loop_n_a",
                }),
            );
        }
        click_targets.push(
            crate::tui::app::ClickTarget::new(
                a_strip_x + off_off,
                row2_y,
                off_w,
                1,
                crate::tui::app::ClickAction::LoopOffDeck { is_a: true },
            )
            .labeled("loop_off_a"),
        );

        // Deck B loop strip is the FIRST 19 chars of ctrl_r2 (since
        // r2 = "{loop_strip}   ◀ NUDGE ▶   PLAY"). build_ctrl_row
        // right-aligns r2: its start = right_end - rl. ctrl_r2 char
        // count = 19 + 3 + 9 + 3 + 4 = 38.
        let r2_len: u16 = 38;
        let b_strip_x = area.x + area.width - 2 /* " │" */ - r2_len;
        for (beats, off_in_strip, w) in buttons {
            click_targets.push(
                crate::tui::app::ClickTarget::new(
                    b_strip_x + off_in_strip,
                    row2_y,
                    w,
                    1,
                    crate::tui::app::ClickAction::LoopEngageDeck { is_a: false, beats },
                )
                .labeled(match beats as u32 {
                    1 => "loop_1_b",
                    2 => "loop_2_b",
                    4 => "loop_4_b",
                    8 => "loop_8_b",
                    16 => "loop_16_b",
                    _ => "loop_n_b",
                }),
            );
        }
        click_targets.push(
            crate::tui::app::ClickTarget::new(
                b_strip_x + off_off,
                row2_y,
                off_w,
                1,
                crate::tui::app::ClickAction::LoopOffDeck { is_a: false },
            )
            .labeled("loop_off_b"),
        );
    }
    // Phase value on its own line, '.' at the EQ centre column (matches the
    // crossfader/phase meters above, not the independent content_w/2).
    let dot_pos = pv.find('.').unwrap_or(pv.len() / 2);
    let center_col = eq_center_col;
    let phase_pad = center_col.saturating_sub(dot_pos);
    out.push(lit_boxed_row(
        &format!("{}{pv}", " ".repeat(phase_pad)),
        bw,
        ctrl_bs,
    ));

    // Help legend (inside controller box)
    if show_help {
        let help = " p:Play  n:Skip  t:Teleport  T:Rewind  m:Mix  </>:Jump  [/]:Nudge  uUiIO:Loop  ;':Grid  Tab:Focus  ?:Close";
        out.push(boxed_row(help, bw, ctrl_bs));
    }

    // └── bottom ──┘
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(bw)),
        ctrl_bs,
    )));
    out.push(Line::from(""));

    // Section dispatch — Full renders all secondary sections stacked;
    // Panel renders just the one picked by `panel_section` below the
    // controller (so the dashboard stays short).
    //
    // `cw` is the per-column text width QUEUE / HISTORY format their
    // rows to fit. It must match the column width `render_two_col_box`
    // actually renders (`(w - 9) / 2`) — otherwise rows get formatted
    // wider than the column and `ps` clips their right edge.
    let cw = w.saturating_sub(9) / 2;
    match dash_layout {
        DashLayout::Full => {
            render_claude_dj_section(&mut out, dj_log, dj_ask, w);
            render_queue_section(
                &mut out,
                info,
                download_in_flight,
                w,
                cw,
                dash_focus == DashFocus::Queue,
            );
            render_history_section(&mut out, info, w, cw, dash_focus == DashFocus::History);
            render_browse_section(
                &mut out,
                area,
                browse_items,
                browse_breadcrumb,
                browse_selected,
                browse_is_tracks,
                w,
                click_targets,
                dash_focus == DashFocus::Browse,
            );
            render_log_section(
                &mut out,
                log_scroll_offset,
                w,
                h,
                dash_focus == DashFocus::Log,
            );
        }
        DashLayout::Panel => {
            // CLAUDE DJ is always-on when there's content — it's a
            // notification surface, not a panel section. Keep it
            // visible even in compact layout.
            render_claude_dj_section(&mut out, dj_log, dj_ask, w);
            match panel_section {
                PanelSection::Queue => {
                    render_queue_section(
                        &mut out,
                        info,
                        download_in_flight,
                        w,
                        cw,
                        dash_focus == DashFocus::Queue,
                    );
                }
                PanelSection::History => {
                    render_history_section(&mut out, info, w, cw, dash_focus == DashFocus::History);
                }
                PanelSection::Browse => {
                    render_browse_section(
                        &mut out,
                        area,
                        browse_items,
                        browse_breadcrumb,
                        browse_selected,
                        browse_is_tracks,
                        w,
                        click_targets,
                        dash_focus == DashFocus::Browse,
                    );
                }
                PanelSection::Log => {
                    render_log_section(
                        &mut out,
                        log_scroll_offset,
                        w,
                        h,
                        dash_focus == DashFocus::Log,
                    );
                }
            }
        }
    }

    frame.render_widget(Paragraph::new(out), area);
}

/// Each deck line is a Vec<Span> so we can color individual characters.
/// Returns (styled_lines, plain_widths) — widths used for padding in the main loop.
fn deck_lines_styled<'a>(
    info: &NowPlayingInfo,
    is_a: bool,
    dw: usize,
    bw: usize,
) -> Vec<(Vec<Span<'a>>, usize)> {
    // is_a now means "physical deck A" — the always-left deck.
    let track = if is_a {
        &info.deck_a_track
    } else {
        &info.deck_b_track
    };
    let bpm = if is_a {
        info.deck_a_bpm
    } else {
        info.deck_b_bpm
    };
    let time = if is_a {
        info.deck_a_time
    } else {
        info.deck_b_time
    };
    let dur = if is_a {
        info.deck_a_duration
    } else {
        info.deck_b_duration
    };
    let is_playing = if is_a {
        info.deck_a_is_playing
    } else {
        info.deck_b_is_playing
    };
    let kick = if is_a {
        info.deck_a_kick
    } else {
        info.deck_b_kick
    };
    let lbl = if is_a { "DECK A" } else { "DECK B" };
    let live_ch = if is_playing { "▶" } else { "⏸" };
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let white = Style::default().fg(Color::White);
    let gray = Style::default().fg(Color::Gray);
    let dk = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow);
    let green = Style::default().fg(Color::Green);

    let mut lines: Vec<(Vec<Span>, usize)> = Vec::new();

    // Helper: push a simple single-span line
    macro_rules! text_line {
        ($text:expr, $style:expr) => {{
            let t: String = $text;
            let w = t.chars().count();
            lines.push((vec![Span::styled(t, $style)], w));
        }};
    }

    match track {
        Some(t) => {
            // Line 1: DECK A ▶  ○ 132.0 (+0.0%)
            let eff = bpm.map(|b| format!("{:.1}", b)).unwrap_or("—".into());
            let nat = t.bpm.unwrap_or(128.0);
            let pct = bpm
                .map(|b| format!("({:+.1}%)", (b / nat - 1.0) * 100.0))
                .unwrap_or_default();
            let pct_w = pct.chars().count();
            let l1 = vec![
                Span::styled(format!("{lbl} "), bold),
                Span::styled(live_ch, if is_playing { green } else { dk }),
                Span::styled(format!("  {} {eff} ", if kick { "●" } else { "○" }), yellow),
                Span::styled(pct, dk),
            ];
            let w1 = lbl.len() + 1 + live_ch.chars().count() + 4 + eff.len() + 1 + pct_w;
            lines.push((l1, w1));

            // Line 2: Track name
            let name = ts(&format!("{} - {}", t.artist_name(), t.full_title()), dw - 2);
            let w2 = name.chars().count();
            lines.push((vec![Span::styled(name, white)], w2));

            // Line 3: 132 BPM / 6A
            let info_str = format!(
                "{} BPM / {}",
                t.bpm.map(|b| format!("{:.0}", b)).unwrap_or("?".into()),
                t.key.as_deref().unwrap_or("?")
            );
            text_line!(info_str, dk);

            // Line 4: time / duration  bar N/M (only on the currently-playing deck)
            if is_playing && dur > 0.0 {
                let bi = bpm.map(|b| 60.0 / b * 4.0).unwrap_or(0.0);
                let cb = if bi > 0.0 { (time / bi) as u32 + 1 } else { 0 };
                let tb = if bi > 0.0 { (dur / bi) as u32 } else { 0 };
                let l4 = vec![
                    Span::styled(ft(time), white),
                    Span::styled(format!(" / {}", ft(dur)), dk),
                    Span::styled(format!("  bar {cb}/{tb}"), dk),
                ];
                let w4 = ft(time).len() + 3 + ft(dur).len() + 6 + format!("{cb}/{tb}").len();
                lines.push((l4, w4));
            } else if dur > 0.0 {
                text_line!(format!("ready  {}", ft(dur)), dk);
            } else {
                text_line!("ready".into(), dk);
            }

            // Line 5: blank
            lines.push((vec![], 0));

            // Line 6: Waveform (colored) — physical deck's buffer
            let empty_wf: Vec<f32> = Vec::new();
            let empty_phr: Vec<Phrase> = Vec::new();
            let analysis = if is_a {
                &info.deck_a_analysis
            } else {
                &info.deck_b_analysis
            };
            let peaks = analysis
                .as_ref()
                .map(|a| &a.waveform_peaks[..])
                .unwrap_or(&empty_wf);
            lines.push(render_waveform_spans(peaks, time, dur, bw));

            // Line 7: Sparkline (colored, with mix point on the playing deck only)
            let phrases = analysis
                .as_ref()
                .map(|a| &a.phrases[..])
                .unwrap_or(&empty_phr);
            let mix_pt = if is_playing {
                info.mix_point_time
            } else {
                None
            };
            lines.push(render_sparkline_spans(phrases, time, dur, bw, mix_pt));
        }
        None => {
            // Empty deck
            let l1 = vec![
                Span::styled(format!("{lbl} "), bold),
                Span::styled(live_ch, dk),
            ];
            lines.push((l1, lbl.len() + 1 + live_ch.chars().count()));
            text_line!("—".into(), dk);
            text_line!("— BPM / —".into(), dk);
            text_line!("—:— / —:—".into(), dk);
            lines.push((vec![], 0));
            // Placeholder bars with labels
            let wave_label = "waveform";
            let wl = wave_label.len();
            let wp = bw.saturating_sub(wl) / 2;
            lines.push((
                vec![
                    Span::styled("░".repeat(wp), dk),
                    Span::styled(wave_label, gray),
                    Span::styled("░".repeat(bw.saturating_sub(wp + wl)), dk),
                ],
                bw,
            ));
            let spark_label = "phrase";
            let sl = spark_label.len();
            let sp = bw.saturating_sub(sl) / 2;
            lines.push((
                vec![
                    Span::styled("░".repeat(sp), dk),
                    Span::styled(spark_label, gray),
                    Span::styled("░".repeat(bw.saturating_sub(sp + sl)), dk),
                ],
                bw,
            ));
        }
    }
    lines
}

fn render_waveform_spans<'a>(
    peaks: &[f32],
    cur: f64,
    total: f64,
    w: usize,
) -> (Vec<Span<'a>>, usize) {
    let wf = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let dk = Style::default().fg(Color::DarkGray);
    let white = Style::default().fg(Color::White);
    let cyan = Style::default().fg(Color::Cyan);

    if peaks.is_empty() {
        return (vec![Span::styled("░".repeat(w), dk)], w);
    }
    let cursor = if total > 0.0 {
        (cur / total * w as f64) as usize
    } else {
        0
    };
    let mut spans: Vec<Span> = Vec::new();
    // Build runs of same style
    let mut cur_style = dk;
    let mut cur_text = String::new();
    for col in 0..w {
        let pi = (col as f64 / w as f64 * peaks.len() as f64) as usize;
        let p = if pi < peaks.len() { peaks[pi] } else { 0.0 };
        let ci = (p * wf.len() as f32).min(wf.len() as f32 - 1.0) as usize;
        let style = if col == cursor {
            cyan
        } else if col < cursor {
            dk
        } else {
            white
        };
        let ch = wf[ci];
        if style != cur_style && !cur_text.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut cur_text), cur_style));
        }
        cur_style = style;
        cur_text.push(ch);
    }
    if !cur_text.is_empty() {
        spans.push(Span::styled(cur_text, cur_style));
    }
    (spans, w)
}

fn render_sparkline_spans<'a>(
    phrases: &[Phrase],
    cur: f64,
    total: f64,
    w: usize,
    mix_point: Option<f64>,
) -> (Vec<Span<'a>>, usize) {
    let sp = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let dk = Style::default().fg(Color::DarkGray);
    let white = Style::default().fg(Color::White);
    let green = Style::default().fg(Color::Green);
    let yellow = Style::default().fg(Color::Yellow);

    if phrases.is_empty() {
        // Progress bar fallback
        if total <= 0.0 {
            return (vec![Span::styled("░".repeat(w), dk)], w);
        }
        let pct = (cur / total).clamp(0.0, 1.0);
        let filled = (pct * w as f64) as usize;
        let mix_col = mix_point.map(|mp| (mp / total * w as f64) as usize);
        let mut spans: Vec<Span> = Vec::new();
        if filled > 0 {
            spans.push(Span::styled("█".repeat(filled), green));
        }
        if let Some(mc) = mix_col {
            if mc > filled {
                spans.push(Span::styled("░".repeat(mc - filled), dk));
                spans.push(Span::styled("░", yellow)); // mix point in yellow
                if w > mc + 1 {
                    spans.push(Span::styled("░".repeat(w - mc - 1), dk));
                }
            } else {
                if w > filled {
                    spans.push(Span::styled("░".repeat(w - filled), dk));
                }
            }
        } else {
            if w > filled {
                spans.push(Span::styled("░".repeat(w - filled), dk));
            }
        }
        return (spans, w);
    }

    let cursor = if total > 0.0 {
        (cur / total * w as f64) as usize
    } else {
        0
    };
    let mix_col = mix_point.and_then(|mp| {
        if total > 0.0 {
            Some((mp / total * w as f64) as usize)
        } else {
            None
        }
    });
    let mut spans: Vec<Span> = Vec::new();
    let mut cur_style = dk;
    let mut cur_text = String::new();

    for col in 0..w {
        let t = col as f64 / w as f64 * total;
        let pi = phrases
            .partition_point(|p| p.start_time <= t)
            .saturating_sub(1);
        let energy = if pi < phrases.len() {
            phrases[pi].energy
        } else {
            0.0
        };
        let ci = (energy * sp.len() as f64)
            .min(sp.len() as f64 - 1.0)
            .max(0.0) as usize;

        let style = if col == cursor {
            green
        } else if mix_col == Some(col) {
            yellow
        } else if col < cursor {
            dk
        } else {
            white
        };
        let ch = sp[ci];

        if style != cur_style && !cur_text.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut cur_text), cur_style));
        }
        cur_style = style;
        cur_text.push(ch);
    }
    if !cur_text.is_empty() {
        spans.push(Span::styled(cur_text, cur_style));
    }
    (spans, w)
}

fn render_sparkline(phrases: &[Phrase], cur: f64, total: f64, w: usize, label: &str) -> String {
    let chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if phrases.is_empty() {
        return format!("{label}: {}", "░".repeat(w));
    }
    let cursor = if total > 0.0 {
        ((cur / total) * w as f64) as usize
    } else {
        usize::MAX
    };
    let mut s = format!("{label}: ");
    for col in 0..w {
        if col == cursor {
            s.push('▎');
            continue;
        }
        let t = col as f64 / w as f64 * total;
        let pi = phrases
            .partition_point(|p| p.start_time <= t)
            .saturating_sub(1);
        let energy = if pi < phrases.len() {
            phrases[pi].energy
        } else {
            0.0
        };
        let ci = (energy * chars.len() as f64)
            .min(chars.len() as f64 - 1.0)
            .max(0.0) as usize;
        s.push(chars[ci]);
    }
    s
}

/// Full-track ASCII waveform with a `▎` playhead at `cur/total`.
/// `cur`/`total` are in the source-track time domain (seconds). Without
/// the cursor the render was static, which is what the user sees today
/// — the shape doesn't change, so nothing appears to move.
fn render_ascii_waveform(peaks: &[f32], cur: f64, total: f64, w: usize, label: &str) -> String {
    let chars = ['_', '.', '-', '~', '^', '/', '\\', '|'];
    if peaks.is_empty() {
        return format!("{label}: {}", "_".repeat(w));
    }
    let step = (peaks.len() / w).max(1);
    let cursor = if total > 0.0 {
        ((cur / total) * w as f64) as usize
    } else {
        usize::MAX
    };
    let mut s = format!("{label}: ");
    for col in 0..w {
        if col == cursor {
            s.push('▎');
            continue;
        }
        let start = col * step;
        let end = (start + step).min(peaks.len());
        if start >= end {
            s.push('_');
            continue;
        }
        let mut peak: f32 = 0.0;
        for p in peaks.iter().take(end).skip(start) {
            peak = peak.max(p.abs());
        }
        let ci = (peak * chars.len() as f32).min(chars.len() as f32 - 1.0) as usize;
        s.push(chars[ci]);
    }
    s
}

/// Tempo upfader cell. `rate` is 1.0 ± up to ~16%. Center row = unity,
/// row 0 = +range (fast), row ml-1 = −range (slow). Visible range is
/// fixed at ±10% so a ±8% tempo_range shows meaningful movement
/// without the indicator pegging to an edge.
fn fader_ch(row: usize, ml: usize, rate: f64) -> Span<'static> {
    const VIS_RANGE: f64 = 0.10;
    let norm = ((rate - 1.0) / VIS_RANGE).clamp(-1.0, 1.0);
    // row 0 = top = +range; row ml-1 = bottom = -range.
    let last = ml.saturating_sub(1) as f64;
    let pos = ((1.0 - (norm + 1.0) / 2.0) * last).round() as usize;
    if row == pos {
        Span::styled("═", Style::default().fg(Color::White))
    } else {
        dim("│")
    }
}

/// Channel-volume upfader cell. `value` in 0..=1. Row 0 = full, row
/// ml-1 = silence.
fn fader_ch_vol(row: usize, ml: usize, value: f32) -> Span<'static> {
    let v = value.clamp(0.0, 1.0) as f64;
    let last = ml.saturating_sub(1) as f64;
    let pos = ((1.0 - v) * last).round() as usize;
    if row == pos {
        Span::styled("═", Style::default().fg(Color::White))
    } else {
        dim("│")
    }
}

fn vu_ch(rfb: usize, thresh: usize, ml: usize) -> Span<'static> {
    if rfb < thresh {
        let top = ml.saturating_sub(1);
        let yel = ml.saturating_sub(2);
        if rfb == top && thresh > top {
            Span::styled("█", Style::default().fg(Color::Red))
        } else if rfb == yel && thresh > yel {
            Span::styled("█", Style::default().fg(Color::Yellow))
        } else {
            Span::styled("█", Style::default().fg(Color::White))
        }
    } else {
        dim("░")
    }
}

#[allow(clippy::too_many_arguments)] // beat-dot renderer; bundling won't help
fn bdot(
    row: usize,
    center: usize,
    playing: bool,
    loaded: bool,
    both: bool,
    aph: f64,
    beat_pos: f64,
    downbeat_aligned: bool,
) -> Span<'static> {
    if !loaded {
        return Span::raw(" ");
    }
    if !playing {
        let bib = ((row as i32 - center as i32).rem_euclid(4)) as usize;
        let ch = if bib == 0 { "●" } else { "○" };
        return dim(ch);
    }
    let offset = (row as f64 - center as f64) + beat_pos;
    let bib = ((offset.floor() as i32).rem_euclid(4)) as usize;
    let ch = if bib == 0 { "●" } else { "○" };
    if !both {
        return dim(ch);
    }
    Span::styled(ch, Style::default().fg(pc_aligned(aph, downbeat_aligned)))
}

/// Build a control row with center anchored at the midpoint of `total_w`.
/// Left gap flexes to push center to the middle. Right gap fills the rest.
fn build_ctrl_row(left: &str, center: &str, right: &str, total_w: usize) -> String {
    build_ctrl_row_at(left, center, right, total_w, total_w / 2)
}

/// Like [`build_ctrl_row`] but anchors `center`'s midpoint at an explicit
/// column `mid` (content-relative) instead of `total_w / 2`. Used by the
/// crossfader / phase rows so their meter centers land on the SAME column as
/// the EQ band's center (`(va_x+vb_x)/2`), keeping them aligned at any width
/// instead of only when the two independent `*/2` roundings happen to match.
fn build_ctrl_row_at(left: &str, center: &str, right: &str, total_w: usize, mid: usize) -> String {
    let ll = left.chars().count();
    let cl = center.chars().count();
    let rl = right.chars().count();
    let center_start = mid.saturating_sub(cl / 2);
    let gap_l = center_start.saturating_sub(ll);
    let gap_r = total_w.saturating_sub(ll + gap_l + cl + rl).max(1);
    format!(
        "{left}{}{center}{}{right}",
        " ".repeat(gap_l),
        " ".repeat(gap_r)
    )
}

/// Box row with lighter text (Gray) — for controls, meters, values.
/// Border style is explicit so a focused panel lights its side pipes.
/// `[bracketed]` substrings in `content` (from `sl()`) render with the
/// brackets replaced by spaces and the inner text underlined — keeps
/// column widths stable while swapping the "selected" visual from
/// brackets to an underline.
fn lit_boxed_row(content: &str, bw: usize, border: Style) -> Line<'static> {
    let cw = bw.saturating_sub(2);
    let padded = ps(content, cw).to_string();
    let base = Style::default().fg(Color::Gray);
    let hl = base.add_modifier(Modifier::UNDERLINED);
    let mut spans: Vec<Span<'static>> = vec![Span::styled("│ ".to_string(), border)];
    let mut cursor = 0;
    while cursor < padded.len() {
        let rest = &padded[cursor..];
        match rest.find('[') {
            Some(rel_start) => {
                if rel_start > 0 {
                    spans.push(Span::styled(rest[..rel_start].to_string(), base));
                }
                let abs_start = cursor + rel_start;
                let after_open = abs_start + 1;
                match padded[after_open..].find(']') {
                    Some(rel_end) => {
                        let abs_end = after_open + rel_end;
                        let inner = &padded[after_open..abs_end];
                        // Keep column count: bracket chars render as spaces.
                        spans.push(Span::styled(" ".to_string(), base));
                        spans.push(Span::styled(inner.to_string(), hl));
                        spans.push(Span::styled(" ".to_string(), base));
                        cursor = abs_end + 1;
                    }
                    None => {
                        // Unmatched '[' — emit the rest as-is and stop.
                        spans.push(Span::styled(padded[abs_start..].to_string(), base));
                        cursor = padded.len();
                    }
                }
            }
            None => {
                spans.push(Span::styled(rest.to_string(), base));
                cursor = padded.len();
            }
        }
    }
    spans.push(Span::styled(" │".to_string(), border));
    Line::from(spans)
}

fn boxed_row(content: &str, bw: usize, border: Style) -> Line<'static> {
    let cw = bw.saturating_sub(2);
    Line::from(vec![
        Span::styled("│ ".to_string(), border),
        Span::styled(
            ps(content, cw).to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" │".to_string(), border),
    ])
}

fn boxed<'a>(title: &str, content: &[&str], w: usize) -> Vec<Line<'a>> {
    let bw = w.saturating_sub(2);
    let tl = title.chars().count();
    let tr = bw.saturating_sub(tl + 4).saturating_add(1);
    let cw = bw.saturating_sub(2); // │ + space + content + space + │
    let mut o: Vec<Line> = Vec::new();
    o.push(Line::from(dim(format!("┌─ {title} {}┐", "─".repeat(tr)))));
    for line in content {
        o.push(Line::from(vec![
            dim("│ "),
            Span::styled(ps(line, cw), Style::default().fg(Color::Gray)),
            dim(" │"),
        ]));
    }
    o.push(Line::from(dim(format!("└{}┘", "─".repeat(bw)))));
    o
}

/// Generate plain-text dashboard lines for screen dump (no ANSI, no ratatui).
pub fn render_dashboard_text(
    info: &NowPlayingInfo,
    wf_mode: WaveformMode,
    width: usize,
) -> Vec<String> {
    let bw = width.saturating_sub(2); // dashes between ┌ and ┐
    let cw = bw.saturating_sub(2); // content width inside "│ … │"
    let mw = (width / 3).clamp(10, 28);

    // Each half of the deck row (before the central divider "  ")
    let half = cw.saturating_sub(2) / 2;

    let mut out: Vec<String> = Vec::new();

    let top_border = |title: &str| -> String {
        let tl = title.chars().count();
        let tr = bw.saturating_sub(tl + 4).saturating_add(1);
        format!("┌─ {title} {}┐", "─".repeat(tr))
    };
    let bot_border = || format!("└{}┘", "─".repeat(bw));
    let row = |s: &str| format!("│ {} │", ps(s, cw));

    // === Controller ===
    let a_info = deck_text_line(info, true, half);
    let b_info = deck_text_line(info, false, half);
    out.push(top_border("CONTROLLER"));
    out.push(row(""));
    for (a, b) in a_info.iter().zip(b_info.iter()) {
        out.push(row(&format!("{}  {}", ps(a, half), ps(b, half))));
    }

    // Crossfader
    let xf_pos = ((mw as f64 * info.crossfader_visual) as usize).min(mw.saturating_sub(1));
    let mut xf_bar = String::from("A ");
    for j in 0..mw {
        xf_bar.push(if j == xf_pos { '║' } else { '─' });
    }
    xf_bar.push_str(" B");
    out.push(row(&build_ctrl_row(
        "CUE  ◀ JUMP ▶",
        &xf_bar,
        "◀ JUMP ▶  CUE",
        cw,
    )));

    // Phase
    let cx = mw / 2;
    let on = (info.phase_offset_ms / 30.0).clamp(-1.0, 1.0);
    let needle = ((cx as f64 + on * cx as f64) as usize).min(mw.saturating_sub(1));
    let mut ph_bar = String::from("  ");
    for j in 0..mw {
        if j == cx {
            ph_bar.push('│');
        } else if j == needle && info.phase_offset_ms.abs() > 3.0 {
            ph_bar.push('▼');
        } else {
            ph_bar.push('─');
        }
    }
    let pv2 = format!("{:+.1}ms", info.phase_offset_ms);
    ph_bar.push_str(&format!("  {:<7}", pv2));
    out.push(row(&build_ctrl_row(
        "PLAY  ◀ NUDGE ▶",
        &ph_bar,
        "◀ NUDGE ▶  PLAY",
        cw,
    )));
    // Waveform rows (mirror the TUI behaviour so screen.txt can be
    // used to smoke-test cursor movement via get_screen).
    let has_a = info.deck_a_analysis.is_some();
    let has_b = info.deck_b_analysis.is_some();
    if has_a && has_b && wf_mode != WaveformMode::Off {
        let wfw = cw.saturating_sub(4);
        let empty_wf: Vec<f32> = Vec::new();
        let empty_phr: Vec<Phrase> = Vec::new();
        let a_wf = info
            .deck_a_analysis
            .as_ref()
            .map(|a| &a.waveform_peaks[..])
            .unwrap_or(&empty_wf);
        let b_wf = info
            .deck_b_analysis
            .as_ref()
            .map(|a| &a.waveform_peaks[..])
            .unwrap_or(&empty_wf);
        let a_phr = info
            .deck_a_analysis
            .as_ref()
            .map(|a| &a.phrases[..])
            .unwrap_or(&empty_phr);
        let b_phr = info
            .deck_b_analysis
            .as_ref()
            .map(|a| &a.phrases[..])
            .unwrap_or(&empty_phr);
        match wf_mode {
            WaveformMode::Audio => {
                out.push(row(&render_ascii_waveform(
                    a_wf,
                    info.deck_a_time,
                    info.deck_a_duration,
                    wfw,
                    "A",
                )));
                out.push(row(&render_ascii_waveform(
                    b_wf,
                    info.deck_b_time,
                    info.deck_b_duration,
                    wfw,
                    "B",
                )));
            }
            WaveformMode::Phrase => {
                out.push(row(&render_sparkline(
                    a_phr,
                    info.deck_a_time,
                    info.deck_a_duration,
                    wfw,
                    "A",
                )));
                out.push(row(&render_sparkline(
                    b_phr,
                    info.deck_b_time,
                    info.deck_b_duration,
                    wfw,
                    "B",
                )));
            }
            WaveformMode::Off => {}
        }
    }

    out.push(bot_border());

    // === Queue ===
    out.push(top_border(&format!("QUEUE ({})", info.queue.len())));
    if info.queue.is_empty() {
        out.push(row("Empty"));
    } else {
        for (i, e) in info.queue.iter().take(10).enumerate() {
            let bk = format!(
                "{}/{}",
                e.track
                    .bpm
                    .map(|b| format!("{:.0}", b))
                    .unwrap_or_else(|| "?".into()),
                e.track.key.as_deref().unwrap_or("?")
            );
            out.push(row(&format!(
                "{:>2}  {} - {}  {bk}",
                i + 1,
                e.track.artist_name(),
                e.track.full_title()
            )));
        }
        if info.queue.len() > 10 {
            out.push(row(&format!("… {} more", info.queue.len() - 10)));
        }
    }
    out.push(bot_border());

    // === History (matches the live TUI section so screen.txt is
    // representative of what the user sees, not just the top half.)
    out.push(top_border(&format!("HISTORY ({})", info.history.len())));
    if info.history.is_empty() {
        out.push(row("No history yet"));
    } else {
        for (i, e) in info.history.iter().take(10).enumerate() {
            let bk = format!(
                "{}/{}",
                e.track
                    .bpm
                    .map(|b| format!("{:.0}", b))
                    .unwrap_or_else(|| "?".into()),
                e.track.key.as_deref().unwrap_or("?")
            );
            let mix = e.mix_score.map(|s| format!(" m{s}")).unwrap_or_default();
            out.push(row(&format!(
                "{:>2}  {} - {}  {bk}{mix}",
                i + 1,
                e.track.artist_name(),
                e.track.full_title()
            )));
        }
        if info.history.len() > 10 {
            out.push(row(&format!("… {} more", info.history.len() - 10)));
        }
    }
    out.push(bot_border());

    // === State ===
    out.push(format!(
        "State: {:?}  Phase: {:+.1}ms  Crossfade: {:.0}%",
        info.state,
        info.phase_offset_ms,
        info.crossfade_progress * 100.0
    ));
    out.push(format!(
        "Playing:  {} BPM  {}",
        info.playing_bpm
            .map(|b| format!("{:.1}", b))
            .unwrap_or("—".into()),
        info.playing_track
            .as_ref()
            .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
            .unwrap_or("—".into())
    ));
    out.push(format!(
        "Incoming: {} BPM  {}",
        info.incoming_bpm
            .map(|b| format!("{:.1}", b))
            .unwrap_or("—".into()),
        info.incoming_track
            .as_ref()
            .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
            .unwrap_or("—".into())
    ));

    out
}

fn deck_text_line(info: &NowPlayingInfo, is_a: bool, dw: usize) -> Vec<String> {
    // is_a means physical deck A here too.
    let track = if is_a {
        &info.deck_a_track
    } else {
        &info.deck_b_track
    };
    let bpm = if is_a {
        info.deck_a_bpm
    } else {
        info.deck_b_bpm
    };
    let time = if is_a {
        info.deck_a_time
    } else {
        info.deck_b_time
    };
    let dur = if is_a {
        info.deck_a_duration
    } else {
        info.deck_b_duration
    };
    let label = if is_a { "DECK A" } else { "DECK B" };
    match track {
        Some(t) => vec![
            format!(
                "{label} {} BPM",
                bpm.map(|b| format!("{:.1}", b)).unwrap_or("—".into())
            ),
            ts(&format!("{} - {}", t.artist_name(), t.full_title()), dw),
            format!("{} / {}", ft(time), ft(dur)),
        ],
        None => vec![format!("{label} —"), "No track".into(), "—:— / —:—".into()],
    }
}

/// Like `read_logs` but skips `offset_back` lines from the bottom
/// before taking `count`. Used by the dashboard's Log panel
/// scrollback: scroll up advances the offset toward older lines.
fn read_logs_offset(count: usize, offset_back: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::Mutex;
    use std::time::Instant;

    struct Cache {
        last_len: u64,
        lines: Vec<String>,
        last_stat: Option<Instant>,
    }
    static CACHE: Mutex<Cache> = Mutex::new(Cache {
        last_len: 0,
        lines: Vec::new(),
        last_stat: None,
    });

    let mut cache = CACHE.lock().unwrap();

    // Rate-limit the stat syscall to every 500ms — at 60Hz this avoids
    // 59 out of 60 metadata() calls per second.
    let now = Instant::now();
    let should_stat = match cache.last_stat {
        Some(t) => now.duration_since(t).as_millis() >= 500,
        None => true,
    };

    if !should_stat {
        let n = cache.lines.len();
        let end = n.saturating_sub(offset_back);
        let start = end.saturating_sub(count);
        return cache.lines[start..end].to_vec();
    }

    cache.last_stat = Some(now);
    let p = dirs::home_dir().unwrap_or_default().join(".mixr/mixr.log");
    let len = match std::fs::metadata(&p) {
        Ok(m) => m.len(),
        Err(_) => return vec!["No log file".into()],
    };

    if len != cache.last_len {
        let mut f = match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(_) => return vec!["No log file".into()],
        };
        const WINDOW: u64 = 64 * 1024;
        let start = len.saturating_sub(WINDOW);
        let _ = f.seek(SeekFrom::Start(start));
        let mut buf = String::new();
        let _ = f.take(WINDOW).read_to_string(&mut buf);
        cache.lines = buf
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                if let Some(b) = l.find(']') {
                    l[b + 1..].trim().to_string()
                } else {
                    l.to_string()
                }
            })
            .collect();
        cache.last_len = len;
    }

    let n = cache.lines.len();
    let end = n.saturating_sub(offset_back);
    let start = end.saturating_sub(count);
    cache.lines[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line `render_two_col_box` emits — top border, item rows
    /// (with and without a right column), the overflow row, the empty
    /// row, the bottom border — must be exactly `w` display cells wide
    /// so the box squares up. Regression for the two-column item rows
    /// landing 1–2 cells short of the border (the right `│` stopping
    /// before the `┐` / `┘`).
    #[test]
    fn two_col_box_rows_match_border_width() {
        // Even and odd widths — the odd remainder of `w - 9` is the
        // case the old even-split mishandled.
        for w in [40usize, 41, 58, 60, 61, 80, 119, 200] {
            // Even and odd item counts so the no-right-column path
            // (rendered for the last row when the count is odd) runs.
            for n in [0usize, 1, 3, 4, 7] {
                let items: Vec<(Color, String)> = (0..n)
                    .map(|i| {
                        (
                            Color::Green,
                            format!("track {i} — a fairly long label here"),
                        )
                    })
                    .collect();
                let overflow = (n > 0).then_some("… 3 more");
                let mut out: Vec<Line> = Vec::new();
                render_two_col_box(
                    &mut out,
                    "QUEUE (9) [q]",
                    &items,
                    overflow,
                    "Empty",
                    w,
                    false,
                );
                assert!(!out.is_empty(), "w={w} n={n}: nothing rendered");
                for (row, line) in out.iter().enumerate() {
                    assert_eq!(
                        line.width(),
                        w,
                        "w={w} n={n}: row {row} is {} cells, expected {w}",
                        line.width(),
                    );
                }
            }
        }
    }

    #[test]
    fn ts_truncates_with_an_ellipsis() {
        // Fits within the width — returned unchanged.
        assert_eq!(ts("hello", 10), "hello");
        // Too long — cut to w-1 chars + an ellipsis (w chars total).
        let t = ts("hello world", 8);
        assert_eq!(t, "hello w\u{2026}");
        assert_eq!(t.chars().count(), 8);
        // Width 2 or less ⇒ nothing (no room for content + ellipsis).
        assert_eq!(ts("anything", 2), "");
    }

    #[test]
    fn ps_pads_or_truncates_to_exact_width() {
        assert_eq!(ps("ab", 5), "ab   ");
        assert_eq!(ps("abcdef", 3), "abc");
        assert_eq!(ps("ab", 2), "ab");
        // Char-counted — a multi-byte string still lands at exactly `w`.
        assert_eq!(ps("\u{273d}\u{273d}", 5).chars().count(), 5);
    }
}
