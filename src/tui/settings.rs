use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::config::*;

/// Build the settings list from current config.
/// One row in the Settings overlay. `key` is the stable identifier
/// for `apply_setting`'s match arm — never displayed, used only for
/// dispatch. Renaming a key breaks dispatch; reordering rows in
/// `build_settings` is free as long as keys stay unique.
#[derive(Debug, Clone)]
pub struct SettingRow {
    pub key: &'static str,
    pub label: &'static str,
    pub options: Vec<String>,
    pub current_idx: usize,
    /// Index this row would carry under `AppConfig::default()`. Used
    /// to paint the `*` modified marker + by `r` / reset-row to know
    /// where to snap the value back to.
    pub default_idx: usize,
}

/// One item in the rendered settings list — section header, discrete-
/// choice row, or text-input row (path / freeform string). Matches
/// the family settings UI convention shared with mnml + tmnl (see
/// `CLAUDE.md`). The `TextRow` is the v2 extension over the original
/// discrete-only schema; enables editing paths like `local_library_dir`
/// in-app instead of forcing config-file edits.
#[derive(Debug, Clone)]
pub enum SettingItem {
    Section(&'static str),
    Row(SettingRow),
    TextRow(TextSettingRow),
}

/// A text-input settings row — for paths, freeform strings, anything
/// that doesn't fit a fixed list of choices.
#[derive(Debug, Clone)]
pub struct TextSettingRow {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    pub default: String,
    /// User-facing placeholder shown when `value` is empty.
    pub placeholder: &'static str,
}

impl TextSettingRow {
    pub fn modified(&self) -> bool {
        self.value != self.default
    }
}

impl SettingRow {
    fn new(
        key: &'static str,
        label: &'static str,
        options: Vec<String>,
        current_idx: usize,
        default_idx: usize,
    ) -> Self {
        Self {
            key,
            label,
            options,
            current_idx,
            default_idx,
        }
    }
    /// `true` when the live value differs from the `AppConfig::default()`
    /// slot — drives the `*` modified marker.
    pub fn modified(&self) -> bool {
        self.current_idx != self.default_idx
    }
}

/// Sentinel `key` value for the "Reset all to defaults" row. Treated
/// specially by both the renderer (no choice list) and `apply_setting`
/// (wipes the live config back to `AppConfig::default()`).
pub const RESET_ALL_KEY: &str = "__reset_all__";

/// Audio-quality index helpers — defaults are AudioQuality::High for
/// audio_quality (idx 0) and AudioQuality::High for preview_quality
/// (idx 1). Captured once so the row builder + default-idx logic
/// don't drift.
fn audio_quality_idx(q: &AudioQuality) -> usize {
    match q {
        AudioQuality::Standard => 1,
        _ => 0,
    }
}
fn preview_quality_idx(q: &AudioQuality) -> usize {
    match q {
        AudioQuality::Standard => 0,
        _ => 1,
    }
}

pub fn build_settings(config: &AppConfig) -> Vec<SettingItem> {
    let d = AppConfig::default();
    let mut out: Vec<SettingItem> = Vec::with_capacity(64);

    // ── Audio ──────────────────────────────────────────────────────
    out.push(SettingItem::Section("Audio"));
    // FLAC is omitted — the dj.beatport.com web app's OAuth scope
    // doesn't include the /download/ endpoint, so picking Lossless
    // would silently fall back to 256k anyway. Branches with
    // partner-scope auth re-add it.
    out.push(SettingItem::Row(SettingRow::new(
        "audio_quality",
        "Audio Quality",
        vec!["256k".into(), "128k".into()],
        audio_quality_idx(&config.audio_quality),
        audio_quality_idx(&d.audio_quality),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "preview_quality",
        "Preview Quality",
        vec!["128k".into(), "256k".into()],
        preview_quality_idx(&config.preview_quality),
        preview_quality_idx(&d.preview_quality),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "output_device",
        "Output Device",
        {
            let mut opts: Vec<String> = vec!["System Default".into()];
            opts.extend(crate::audio::output_device_names());
            opts
        },
        output_device_idx(&config.output_device),
        output_device_idx(&d.output_device),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "monitor_device",
        "Monitor Device",
        {
            let mut opts: Vec<String> = vec!["Off".into()];
            opts.extend(crate::audio::output_device_names());
            opts
        },
        monitor_device_idx(&config.monitor_device),
        monitor_device_idx(&d.monitor_device),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "master_limiter",
        "Master Limiter",
        vec!["Off".into(), "Soft Knee".into()],
        match config.master_limiter {
            LimiterMode::Off => 0,
            LimiterMode::SoftKnee => 1,
        },
        match d.master_limiter {
            LimiterMode::Off => 0,
            LimiterMode::SoftKnee => 1,
        },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "pitch_stretch",
        "Pitch Stretch",
        vec![
            "Off".into(),
            if cfg!(feature = "rubberband") {
                "Rubberband".into()
            } else {
                "Rubberband (build --features rubberband)".into()
            },
            if cfg!(feature = "timestretch") {
                "Timestretch".into()
            } else {
                "Timestretch (build --features timestretch)".into()
            },
        ],
        pitch_stretch_idx(&config.pitch_stretch_engine),
        pitch_stretch_idx(&d.pitch_stretch_engine),
    )));

    // ── Mixing ─────────────────────────────────────────────────────
    out.push(SettingItem::Section("Mixing"));
    out.push(SettingItem::Row(SettingRow::new(
        "bpm_mode",
        "BPM Mode",
        vec!["Glide".into(), "Lock".into()],
        match config.bpm_mode {
            BpmMode::Glide => 0,
            BpmMode::Lock => 1,
        },
        match d.bpm_mode {
            BpmMode::Glide => 0,
            BpmMode::Lock => 1,
        },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "split_cue",
        "Split Cue",
        vec!["Off".into(), "On".into()],
        if config.split_cue { 1 } else { 0 },
        if d.split_cue { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "crossfade_bars",
        "Crossfade Bars",
        vec![
            "8".into(),
            "16".into(),
            "32".into(),
            "64".into(),
            "Auto".into(),
        ],
        crossfade_bars_idx(config.crossfade_bars, config.crossfade_bars_auto),
        crossfade_bars_idx(d.crossfade_bars, d.crossfade_bars_auto),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "glide_bars",
        "Glide Bars",
        vec![
            "8".into(),
            "16".into(),
            "32".into(),
            "64".into(),
            "Max".into(),
        ],
        glide_bars_idx(config.glide_bars),
        glide_bars_idx(d.glide_bars),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "jump_bars",
        "Jump Bars",
        vec!["4".into(), "8".into(), "16".into(), "32".into()],
        jump_bars_idx(config.jump_bars),
        jump_bars_idx(d.jump_bars),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "quantize_on",
        "Quantize",
        vec!["Off".into(), "On".into()],
        if config.quantize_on { 1 } else { 0 },
        if d.quantize_on { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "quantize_beats",
        "Quantize Beats",
        vec![
            "1/8".into(),
            "1/4".into(),
            "1/2".into(),
            "1".into(),
            "2".into(),
            "4".into(),
            "8".into(),
        ],
        quantize_beats_idx(config.quantize_beats),
        quantize_beats_idx(d.quantize_beats),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "tempo_range",
        "Tempo Range",
        vec![
            "±4%".into(),
            "±6%".into(),
            "±8%".into(),
            "±10%".into(),
            "±16%".into(),
        ],
        tempo_range_idx(config.tempo_range),
        tempo_range_idx(d.tempo_range),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "nudge_percent",
        "Nudge %",
        vec![
            "1%".into(),
            "2%".into(),
            "3%".into(),
            "4%".into(),
            "5%".into(),
        ],
        nudge_percent_idx(config.nudge_percent),
        nudge_percent_idx(d.nudge_percent),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "mix_in_point",
        "Mix In Point",
        vec!["First Beat".into(), "Drop".into(), "Middle".into()],
        mix_in_point_idx(&config.mix_in_point),
        mix_in_point_idx(&d.mix_in_point),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "smart_mix_out",
        "Smart Mix Out",
        vec!["Off".into(), "On".into()],
        if config.smart_mix_out { 1 } else { 0 },
        if d.smart_mix_out { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "train_wreck",
        "Train Wreck",
        vec!["Off".into(), "Detect".into(), "Auto Bail".into()],
        match config.train_wreck_mode {
            crate::config::TrainWreckMode::Off => 0,
            crate::config::TrainWreckMode::Detect => 1,
            crate::config::TrainWreckMode::AutoBail => 2,
        },
        match d.train_wreck_mode {
            crate::config::TrainWreckMode::Off => 0,
            crate::config::TrainWreckMode::Detect => 1,
            crate::config::TrainWreckMode::AutoBail => 2,
        },
    )));

    // ── Playback ───────────────────────────────────────────────────
    out.push(SettingItem::Section("Playback"));
    out.push(SettingItem::Row(SettingRow::new(
        "shuffle_on_play",
        "Shuffle on Play",
        vec!["Off".into(), "On".into()],
        if config.shuffle_on_play { 1 } else { 0 },
        if d.shuffle_on_play { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "compact_view",
        "View Mode",
        vec!["Compact".into(), "Full".into()],
        if config.compact_view { 0 } else { 1 },
        if d.compact_view { 0 } else { 1 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "resume_session",
        "Resume Session",
        vec!["Never".into(), "Ask".into(), "Always".into()],
        match config.resume_behavior {
            ResumeBehavior::Never => 0,
            ResumeBehavior::Ask => 1,
            ResumeBehavior::Always => 2,
        },
        match d.resume_behavior {
            ResumeBehavior::Never => 0,
            ResumeBehavior::Ask => 1,
            ResumeBehavior::Always => 2,
        },
    )));

    // ── Analysis ───────────────────────────────────────────────────
    out.push(SettingItem::Section("Analysis"));
    out.push(SettingItem::Row(SettingRow::new(
        "ai_beat_detection",
        "AI Beat Detection",
        vec!["Off".into(), "On".into()],
        if config.ai_beat_detection { 1 } else { 0 },
        if d.ai_beat_detection { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "ai_grid_validation",
        "AI Grid Validation",
        vec!["Off".into(), "On".into()],
        if config.ai_grid_validation { 1 } else { 0 },
        if d.ai_grid_validation { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "ai_phrase_detection",
        "AI Phrase Detection",
        vec!["Off".into(), "On".into()],
        if config.ai_phrase_detection { 1 } else { 0 },
        if d.ai_phrase_detection { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "analyzer_engine",
        "Analyzer Engine",
        vec![
            "Built-in".into(),
            if cfg!(feature = "stratum") {
                "Stratum".into()
            } else {
                "Stratum (build --features stratum)".into()
            },
        ],
        match config.analyzer_engine {
            AnalyzerEngine::Builtin => 0,
            AnalyzerEngine::Stratum => 1,
        },
        match d.analyzer_engine {
            AnalyzerEngine::Builtin => 0,
            AnalyzerEngine::Stratum => 1,
        },
    )));

    // ── Transitions ────────────────────────────────────────────────
    out.push(SettingItem::Section("Transitions"));
    for (key, name, label) in [
        ("tx_beatmatched", "BeatMatched", "Transition: BeatMatched"),
        ("tx_echoout", "EchoOut", "Transition: EchoOut"),
        ("tx_bassswap", "BassSwap", "Transition: BassSwap"),
        ("tx_filtersweep", "FilterSweep", "Transition: FilterSweep"),
        ("tx_looproll", "LoopRoll", "Transition: LoopRoll"),
    ] {
        let cur = if config.enabled_transitions.iter().any(|s| s == name) {
            1
        } else {
            0
        };
        let def = if d.enabled_transitions.iter().any(|s| s == name) {
            1
        } else {
            0
        };
        out.push(SettingItem::Row(SettingRow::new(
            key,
            label,
            vec!["Off".into(), "On".into()],
            cur,
            def,
        )));
    }
    out.push(SettingItem::Row(SettingRow::new(
        "edit_rules",
        "Edit Transition Rules",
        vec!["Open".into()],
        0,
        0,
    )));

    // ── Claude DJ ──────────────────────────────────────────────────
    out.push(SettingItem::Section("Claude DJ"));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_mode",
        "DJ Mode",
        vec!["Auto".into(), "Assist".into(), "Manual".into()],
        match config.claude_dj.mode {
            ClaudeDjMode::Auto => 0,
            ClaudeDjMode::Assist => 1,
            ClaudeDjMode::Manual => 2,
        },
        match d.claude_dj.mode {
            ClaudeDjMode::Auto => 0,
            ClaudeDjMode::Assist => 1,
            ClaudeDjMode::Manual => 2,
        },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_camelot",
        "DJ Camelot",
        vec!["Strict".into(), "Prefer".into(), "Off".into()],
        strictness_idx(&config.claude_dj.camelot_strictness),
        strictness_idx(&d.claude_dj.camelot_strictness),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_bpm_gap",
        "DJ BPM Gap",
        vec!["Strict".into(), "Prefer".into(), "Off".into()],
        strictness_idx(&config.claude_dj.bpm_gap_strictness),
        strictness_idx(&d.claude_dj.bpm_gap_strictness),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_transitions",
        "DJ Transitions",
        vec!["Engine".into(), "Claude".into()],
        match config.claude_dj.transition_picker {
            TransitionPicker::Engine => 0,
            TransitionPicker::Claude => 1,
        },
        match d.claude_dj.transition_picker {
            TransitionPicker::Engine => 0,
            TransitionPicker::Claude => 1,
        },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_style",
        "DJ Style",
        vec![
            "Underground".into(),
            "Mainstream".into(),
            "Exploratory".into(),
        ],
        match config.claude_dj.style {
            DjStyle::Underground => 0,
            DjStyle::Mainstream => 1,
            DjStyle::Exploratory => 2,
        },
        match d.claude_dj.style {
            DjStyle::Underground => 0,
            DjStyle::Mainstream => 1,
            DjStyle::Exploratory => 2,
        },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_quick_mix",
        "DJ Quick Mix",
        vec!["Off".into(), "On".into()],
        if config.claude_dj.quick_mix { 1 } else { 0 },
        if d.claude_dj.quick_mix { 1 } else { 0 },
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "dj_memory",
        "DJ Memory",
        vec!["Off".into(), "On".into()],
        if config.claude_dj.memory_enabled {
            1
        } else {
            0
        },
        if d.claude_dj.memory_enabled { 1 } else { 0 },
    )));

    // ── Browser ────────────────────────────────────────────────────
    out.push(SettingItem::Section("Browser"));
    out.push(SettingItem::Row(SettingRow::new(
        "browser",
        "Browser",
        vec![
            "Google Chrome".into(),
            "Safari".into(),
            "Firefox".into(),
            "Arc".into(),
        ],
        browser_idx(&config.browser),
        browser_idx(&d.browser),
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "default_genre",
        "Default Genre",
        vec![config.default_genre.clone()],
        0,
        0,
    )));
    out.push(SettingItem::Row(SettingRow::new(
        "favorite_genres",
        "Favorite Genres",
        vec![format!("{} selected", config.favorite_genres.len())],
        0,
        0,
    )));

    // ── Library ────────────────────────────────────────────────────
    // Path inputs — Enter to edit, type to modify, Esc to cancel,
    // Enter to save. v2 of the family settings UI convention.
    out.push(SettingItem::Section("Library"));
    out.push(SettingItem::TextRow(TextSettingRow {
        key: "local_library_dir",
        label: "Local Library Directory",
        value: config.local_library_dir.clone(),
        default: d.local_library_dir.clone(),
        placeholder: "/path/to/music — Enter to edit",
    }));
    out.push(SettingItem::TextRow(TextSettingRow {
        key: "rekordbox_xml",
        label: "Rekordbox XML",
        value: config.rekordbox_xml.clone(),
        default: d.rekordbox_xml.clone(),
        placeholder: "/path/to/rekordbox.xml",
    }));
    out.push(SettingItem::TextRow(TextSettingRow {
        key: "engine_dj_db",
        label: "Engine DJ Database",
        value: config.engine_dj_db.clone(),
        default: d.engine_dj_db.clone(),
        placeholder: "/path/to/m.db",
    }));
    out.push(SettingItem::TextRow(TextSettingRow {
        key: "serato_db",
        label: "Serato Database",
        value: config.serato_db.clone(),
        default: d.serato_db.clone(),
        placeholder: "/path/to/database V2",
    }));

    // ── Account ────────────────────────────────────────────────────
    out.push(SettingItem::Section("Account"));
    out.push(SettingItem::Row(SettingRow::new(
        "logout",
        "Logout",
        vec![],
        0,
        0,
    )));

    // ── Reset ──────────────────────────────────────────────────────
    out.push(SettingItem::Section("Reset"));
    out.push(SettingItem::Row(SettingRow::new(
        RESET_ALL_KEY,
        "Reset all to defaults",
        Vec::new(),
        0,
        0,
    )));

    out
}

// ── helpers for current-idx + default-idx parity ──────────────────

fn output_device_idx(name: &str) -> usize {
    if name.is_empty() {
        0
    } else {
        crate::audio::output_device_names()
            .iter()
            .position(|n| n == name)
            .map(|i| i + 1)
            .unwrap_or(0)
    }
}
fn monitor_device_idx(name: &str) -> usize {
    output_device_idx(name)
}
fn pitch_stretch_idx(e: &crate::audio::pitch_stretch::PitchStretchEngine) -> usize {
    match e {
        crate::audio::pitch_stretch::PitchStretchEngine::Off => 0,
        crate::audio::pitch_stretch::PitchStretchEngine::Rubberband => 1,
        crate::audio::pitch_stretch::PitchStretchEngine::Timestretch => 2,
    }
}
fn crossfade_bars_idx(bars: u32, auto: bool) -> usize {
    if auto {
        4
    } else {
        match bars {
            8 => 0,
            32 => 2,
            64 => 3,
            _ => 1,
        }
    }
}
fn glide_bars_idx(bars: u32) -> usize {
    match bars {
        8 => 0,
        32 => 2,
        64 => 3,
        0 => 4,
        _ => 1,
    }
}
fn jump_bars_idx(bars: u32) -> usize {
    match bars {
        4 => 0,
        16 => 2,
        32 => 3,
        _ => 1,
    }
}
fn quantize_beats_idx(q: f64) -> usize {
    if q < 0.1875 {
        0
    }
    // 1/8
    else if q < 0.375 {
        1
    }
    // 1/4
    else if q < 0.75 {
        2
    }
    // 1/2
    else if q < 1.5 {
        3
    }
    // 1
    else if q < 3.0 {
        4
    }
    // 2
    else if q < 6.0 {
        5
    }
    // 4
    else {
        6
    } // 8
}
fn tempo_range_idx(r: u32) -> usize {
    match r {
        4 => 0,
        6 => 1,
        10 => 3,
        16 => 4,
        _ => 2,
    }
}
fn nudge_percent_idx(n: u32) -> usize {
    match n {
        1 => 0,
        2 => 1,
        4 => 3,
        5 => 4,
        _ => 2,
    }
}
fn mix_in_point_idx(p: &MixInPoint) -> usize {
    match p {
        MixInPoint::FirstBeat | MixInPoint::FirstAudio => 0,
        MixInPoint::Drop => 1,
        MixInPoint::Middle => 2,
    }
}
fn strictness_idx(s: &Strictness) -> usize {
    match s {
        Strictness::Strict => 0,
        Strictness::Prefer => 1,
        Strictness::Off => 2,
    }
}
fn browser_idx(name: &str) -> usize {
    match name {
        "Safari" => 1,
        "Firefox" => 2,
        "Arc" => 3,
        _ => 0,
    }
}

/// Apply a setting change to the config. Dispatches by stable key
/// rather than row index so adding/reordering rows in
/// `build_settings` doesn't cascade into renumbered match arms.
/// Unknown keys silently do nothing (forward-compatible with patches
/// that add new rows + handlers).
pub fn apply_setting(config: &mut AppConfig, key: &str, option_idx: usize) -> Option<&'static str> {
    match key {
        "audio_quality" => {
            config.audio_quality = if option_idx == 1 {
                AudioQuality::Standard
            } else {
                AudioQuality::High
            };
            None
        }
        "preview_quality" => {
            config.preview_quality = if option_idx == 0 {
                AudioQuality::Standard
            } else {
                AudioQuality::High
            };
            None
        }
        "bpm_mode" => {
            config.bpm_mode = match option_idx {
                0 => BpmMode::Glide,
                _ => BpmMode::Lock,
            };
            None
        }
        "split_cue" => {
            config.split_cue = option_idx == 1;
            None
        }
        "crossfade_bars" => {
            match option_idx {
                0 => {
                    config.crossfade_bars = 8;
                    config.crossfade_bars_auto = false;
                }
                2 => {
                    config.crossfade_bars = 32;
                    config.crossfade_bars_auto = false;
                }
                3 => {
                    config.crossfade_bars = 64;
                    config.crossfade_bars_auto = false;
                }
                4 => {
                    config.crossfade_bars_auto = true;
                }
                _ => {
                    config.crossfade_bars = 16;
                    config.crossfade_bars_auto = false;
                }
            }
            Some("crossfade_bars_changed")
        }
        "glide_bars" => {
            config.glide_bars = match option_idx {
                0 => 8,
                2 => 32,
                3 => 64,
                4 => 0,
                _ => 16,
            };
            None
        }
        "jump_bars" => {
            config.jump_bars = match option_idx {
                0 => 4,
                2 => 16,
                3 => 32,
                _ => 8,
            };
            Some("jump_bars_changed")
        }
        "quantize_on" => {
            config.quantize_on = option_idx == 1;
            Some("quantize_changed")
        }
        "quantize_beats" => {
            config.quantize_beats = match option_idx {
                0 => 0.125,
                1 => 0.25,
                2 => 0.5,
                3 => 1.0,
                4 => 2.0,
                5 => 4.0,
                6 => 8.0,
                _ => 1.0,
            };
            Some("quantize_changed")
        }
        "tempo_range" => {
            config.tempo_range = match option_idx {
                0 => 4,
                1 => 6,
                3 => 10,
                4 => 16,
                _ => 8,
            };
            None
        }
        "nudge_percent" => {
            config.nudge_percent = match option_idx {
                0 => 1,
                1 => 2,
                3 => 4,
                4 => 5,
                _ => 3,
            };
            None
        }
        "mix_in_point" => {
            config.mix_in_point = match option_idx {
                0 => MixInPoint::FirstBeat,
                1 => MixInPoint::Drop,
                _ => MixInPoint::Middle,
            };
            None
        }
        "smart_mix_out" => {
            config.smart_mix_out = option_idx == 1;
            None
        }
        "shuffle_on_play" => {
            config.shuffle_on_play = option_idx == 1;
            None
        }
        "compact_view" => {
            config.compact_view = option_idx == 0;
            None
        }
        "browser" => {
            config.browser = match option_idx {
                1 => "Safari",
                2 => "Firefox",
                3 => "Arc",
                _ => "Google Chrome",
            }
            .into();
            None
        }
        "ai_beat_detection" => {
            config.ai_beat_detection = option_idx == 1;
            None
        }
        "ai_grid_validation" => {
            config.ai_grid_validation = option_idx == 1;
            None
        }
        "ai_phrase_detection" => {
            config.ai_phrase_detection = option_idx == 1;
            None
        }
        "tx_beatmatched" => {
            toggle_transition(config, "BeatMatched", option_idx == 1);
            Some("transitions_changed")
        }
        "tx_echoout" => {
            toggle_transition(config, "EchoOut", option_idx == 1);
            Some("transitions_changed")
        }
        "tx_bassswap" => {
            toggle_transition(config, "BassSwap", option_idx == 1);
            Some("transitions_changed")
        }
        "tx_filtersweep" => {
            toggle_transition(config, "FilterSweep", option_idx == 1);
            Some("transitions_changed")
        }
        "tx_looproll" => {
            toggle_transition(config, "LoopRoll", option_idx == 1);
            Some("transitions_changed")
        }
        "edit_rules" => Some("open_rules_editor"),
        "output_device" => {
            let devices = crate::audio::output_device_names();
            config.output_device = if option_idx == 0 {
                String::new()
            } else {
                devices.get(option_idx - 1).cloned().unwrap_or_default()
            };
            Some("output_device_changed")
        }
        "monitor_device" => {
            let devices = crate::audio::output_device_names();
            config.monitor_device = if option_idx == 0 {
                String::new()
            } else {
                devices.get(option_idx - 1).cloned().unwrap_or_default()
            };
            Some("monitor_device_changed")
        }
        "master_limiter" => {
            config.master_limiter = if option_idx == 0 {
                LimiterMode::Off
            } else {
                LimiterMode::SoftKnee
            };
            Some("master_limiter_changed")
        }
        "train_wreck" => {
            config.train_wreck_mode = match option_idx {
                0 => crate::config::TrainWreckMode::Off,
                1 => crate::config::TrainWreckMode::Detect,
                _ => crate::config::TrainWreckMode::AutoBail,
            };
            Some("train_wreck_changed")
        }
        "pitch_stretch" => {
            if option_idx == 1 && !cfg!(feature = "rubberband") {
                return Some("rubberband_unavailable");
            }
            if option_idx == 2 && !cfg!(feature = "timestretch") {
                return Some("timestretch_unavailable");
            }
            config.pitch_stretch_engine = match option_idx {
                1 => crate::audio::pitch_stretch::PitchStretchEngine::Rubberband,
                2 => crate::audio::pitch_stretch::PitchStretchEngine::Timestretch,
                _ => crate::audio::pitch_stretch::PitchStretchEngine::Off,
            };
            Some("pitch_stretch_changed")
        }
        "dj_mode" => {
            config.claude_dj.mode = match option_idx {
                0 => ClaudeDjMode::Auto,
                1 => ClaudeDjMode::Assist,
                _ => ClaudeDjMode::Manual,
            };
            Some("claudedj_changed")
        }
        "dj_camelot" => {
            config.claude_dj.camelot_strictness = match option_idx {
                0 => Strictness::Strict,
                2 => Strictness::Off,
                _ => Strictness::Prefer,
            };
            Some("claudedj_changed")
        }
        "dj_bpm_gap" => {
            config.claude_dj.bpm_gap_strictness = match option_idx {
                0 => Strictness::Strict,
                2 => Strictness::Off,
                _ => Strictness::Prefer,
            };
            Some("claudedj_changed")
        }
        "dj_transitions" => {
            config.claude_dj.transition_picker = match option_idx {
                1 => TransitionPicker::Claude,
                _ => TransitionPicker::Engine,
            };
            Some("claudedj_changed")
        }
        "dj_style" => {
            config.claude_dj.style = match option_idx {
                1 => DjStyle::Mainstream,
                2 => DjStyle::Exploratory,
                _ => DjStyle::Underground,
            };
            Some("claudedj_changed")
        }
        "dj_quick_mix" => {
            config.claude_dj.quick_mix = option_idx == 1;
            Some("claudedj_changed")
        }
        "dj_memory" => {
            config.claude_dj.memory_enabled = option_idx == 1;
            Some("claudedj_changed")
        }
        "resume_session" => {
            config.resume_behavior = match option_idx {
                0 => ResumeBehavior::Never,
                1 => ResumeBehavior::Ask,
                _ => ResumeBehavior::Always,
            };
            None
        }
        "analyzer_engine" => {
            // Always honor the toggle. If the `stratum` feature isn't
            // compiled in, resolve_bpm transparently falls back to the
            // built-in detector — the setting still records the user's
            // preference so a rebuild with the feature picks it up.
            config.analyzer_engine = match option_idx {
                1 => AnalyzerEngine::Stratum,
                _ => AnalyzerEngine::Builtin,
            };
            if option_idx == 1 && !cfg!(feature = "stratum") {
                Some("stratum_fallback")
            } else {
                None
            }
        }
        "default_genre" => Some("pick_genre"),
        "favorite_genres" => Some("pick_favorites"),
        "logout" => Some("logout"),
        // Reset sentinel — the dispatcher handles this specially.
        RESET_ALL_KEY => Some("reset_all"),
        _ => None,
    }
}

/// Apply a text/path value to the config by key. Mirrors apply_setting's
/// shape for choice rows. Returns the engine-resync key string when
/// the field has runtime effects (currently no library-dir fields do —
/// they trigger a re-scan only on next browse).
pub fn apply_text_setting(
    config: &mut AppConfig,
    key: &str,
    value: String,
) -> Option<&'static str> {
    match key {
        "local_library_dir" => {
            config.local_library_dir = value;
            None
        }
        "rekordbox_xml" => {
            config.rekordbox_xml = value;
            None
        }
        "engine_dj_db" => {
            config.engine_dj_db = value;
            None
        }
        "serato_db" => {
            config.serato_db = value;
            None
        }
        _ => None,
    }
}

fn toggle_transition(config: &mut AppConfig, name: &str, on: bool) {
    let has = config.enabled_transitions.iter().any(|s| s == name);
    if on && !has {
        config.enabled_transitions.push(name.to_string());
    } else if !on && has {
        config.enabled_transitions.retain(|s| s != name);
    }
}

/// Unified focusable-row reference. Choice rows and text rows are
/// both navigable from the settings list; their key handling diverges
/// (Choice: Enter cycles, Left/Right adjust; Text: Enter → edit mode,
/// typing modifies the value).
#[derive(Debug, Clone)]
pub enum SettingsRowEntry {
    Choice(SettingRow),
    Text(TextSettingRow),
}

impl SettingsRowEntry {
    #[allow(dead_code)] // accessor for future call sites that need either kind generically
    pub fn key(&self) -> &str {
        match self {
            SettingsRowEntry::Choice(r) => r.key,
            SettingsRowEntry::Text(r) => r.key,
        }
    }
    #[allow(dead_code)]
    pub fn modified(&self) -> bool {
        match self {
            SettingsRowEntry::Choice(r) => r.modified(),
            SettingsRowEntry::Text(r) => r.modified(),
        }
    }
}

/// Total focusable rows (Sections are not focusable). Used by the
/// key handler in `keys.rs` to bound `self.selected`.
pub fn settings_row_count(config: &AppConfig) -> usize {
    build_settings(config)
        .iter()
        .filter(|i| !matches!(i, SettingItem::Section(_)))
        .count()
}

/// The `row_idx`-th focusable row, by skipping `Section` items.
/// Returns a Choice row or a Text row.
pub fn settings_row_entry_at(config: &AppConfig, row_idx: usize) -> Option<SettingsRowEntry> {
    build_settings(config)
        .into_iter()
        .filter_map(|i| match i {
            SettingItem::Row(r) => Some(SettingsRowEntry::Choice(r)),
            SettingItem::TextRow(r) => Some(SettingsRowEntry::Text(r)),
            SettingItem::Section(_) => None,
        })
        .nth(row_idx)
}

/// Choice-row only accessor — for legacy code paths that don't need
/// to handle text rows. Returns None for text rows even at valid
/// indices.
pub fn settings_row_at(config: &AppConfig, row_idx: usize) -> Option<SettingRow> {
    match settings_row_entry_at(config, row_idx)? {
        SettingsRowEntry::Choice(r) => Some(r),
        SettingsRowEntry::Text(_) => None,
    }
}

/// Render the settings screen. Paints `── Section ──` headers (not
/// focusable), then `▸ <label>:  [active] / other  *` rows. `▸` =
/// focused row; `[bracket]` = current choice; `*` = modified from
/// `AppConfig::default()`. Matches the family settings UI convention
/// — see CLAUDE.md.
///
/// Scrolls when the row list exceeds `area.height` — keeps the
/// focused row visible by sliding the rendered window down as
/// `selected_row` advances past the bottom.
pub fn render_settings(
    frame: &mut Frame,
    area: Rect,
    config: &AppConfig,
    selected_row: usize,
    editing_text: Option<&str>,
) {
    let items = build_settings(config);
    let mut lines: Vec<Line> = Vec::with_capacity(items.len());
    let mut row_counter = 0usize;
    // Track the index in `lines` of the focused row so we can scroll
    // to keep it visible — section headers consume line slots but
    // aren't counted by `selected_row`, so this can't be derived
    // arithmetically.
    let mut focused_line_idx: usize = 0;

    for item in items.iter() {
        match item {
            SettingItem::Section(name) => {
                lines.push(Line::from(Span::styled(
                    format!("── {name} ──"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            SettingItem::Row(row) => {
                let is_selected = row_counter == selected_row;
                if is_selected {
                    focused_line_idx = lines.len();
                }
                row_counter += 1;
                let marker = if is_selected { "▸ " } else { "  " };
                let mut spans = vec![
                    Span::styled(
                        marker,
                        Style::default().fg(if is_selected {
                            Color::White
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(
                        format!("{}:  ", row.label),
                        Style::default().fg(if is_selected {
                            Color::White
                        } else {
                            Color::Gray
                        }),
                    ),
                ];

                if row.key == RESET_ALL_KEY {
                    spans.push(Span::styled(
                        "(Enter to reset)",
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(if is_selected {
                                Modifier::BOLD
                            } else {
                                Modifier::DIM
                            }),
                    ));
                } else if row.options.is_empty() {
                    // Logout-style action row — just the label colored.
                    spans.push(Span::styled(
                        row.label.to_string(),
                        Style::default().fg(Color::Red),
                    ));
                } else {
                    for (j, opt) in row.options.iter().enumerate() {
                        let is_current = j == row.current_idx;
                        if j > 0 {
                            spans.push(Span::styled(" / ", Style::default().fg(Color::DarkGray)));
                        }
                        if is_current {
                            spans.push(Span::styled(
                                format!("[{opt}]"),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        } else {
                            spans.push(Span::styled(
                                opt.clone(),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                    }
                    if row.modified() {
                        spans.push(Span::styled(
                            "  *",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }

                lines.push(Line::from(spans));
            }
            SettingItem::TextRow(row) => {
                let is_selected = row_counter == selected_row;
                if is_selected {
                    focused_line_idx = lines.len();
                }
                row_counter += 1;
                let marker = if is_selected { "▸ " } else { "  " };
                let mut spans = vec![
                    Span::styled(
                        marker,
                        Style::default().fg(if is_selected {
                            Color::White
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(
                        format!("{}:  ", row.label),
                        Style::default().fg(if is_selected {
                            Color::White
                        } else {
                            Color::Gray
                        }),
                    ),
                ];
                // While editing the focused row, paint the in-progress value
                // with a trailing block cursor. Otherwise show the saved
                // value (or placeholder when empty).
                if let (true, Some(editing)) = (is_selected, editing_text) {
                    spans.push(Span::styled(
                        editing.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled("█", Style::default().fg(Color::Yellow)));
                } else if row.value.is_empty() {
                    spans.push(Span::styled(
                        row.placeholder.to_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ));
                } else {
                    spans.push(Span::styled(
                        row.value.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                    if row.modified() {
                        spans.push(Span::styled(
                            "  *",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
                lines.push(Line::from(spans));
            }
        }
    }

    // Slide the rendered window so the focused row stays on-screen.
    // When focused_line_idx falls within the visible area, scroll = 0;
    // once it goes past the bottom, scroll advances row-by-row.
    let body_h = area.height as usize;
    let scroll = if body_h > 0 && focused_line_idx >= body_h {
        focused_line_idx + 1 - body_h
    } else {
        0
    };
    // Truncate each line to fit `area.width` so a long Local
    // Library Directory path (or similar TextRow value) doesn't
    // punch through the right edge of the panel. 2026-06-08 family-
    // wide settings-overflow sweep (matches mnml's
    // `truncate_line_to_width`).
    let window: Vec<Line> = lines
        .into_iter()
        .skip(scroll)
        .take(body_h)
        .map(|l| truncate_line_to_width(l, area.width as usize))
        .collect();
    let paragraph = Paragraph::new(window);
    frame.render_widget(paragraph, area);
}

/// Truncate a `Line` (span-by-span) so its total char count doesn't
/// exceed `max_width`. Appends `…` when truncation happens so the
/// row reads as "cut" rather than broken mid-word. Char-based width,
/// not display-cell — fine for ASCII / Latin values, slightly
/// pessimistic for CJK / emoji. Mirror of the helper in
/// mnml's `src/ui/settings_overlay.rs`.
fn truncate_line_to_width<'a>(line: Line<'a>, max_width: usize) -> Line<'a> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_width {
        return line;
    }
    let budget = max_width.saturating_sub(1); // 1 cell reserved for `…`
    let mut used = 0usize;
    let mut out: Vec<Span<'a>> = Vec::with_capacity(line.spans.len() + 1);
    for span in line.spans.into_iter() {
        let span_len = span.content.chars().count();
        if used + span_len <= budget {
            used += span_len;
            out.push(span);
        } else {
            let take = budget.saturating_sub(used);
            if take > 0 {
                let s: String = span.content.chars().take(take).collect();
                out.push(Span::styled(s, span.style));
            }
            break;
        }
    }
    out.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
    Line::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn text_rows_appear_in_library_section() {
        let cfg = AppConfig::default();
        let items = build_settings(&cfg);
        // The Library section should include the local_library_dir text row.
        let lib_idx = items
            .iter()
            .position(|i| matches!(i, SettingItem::Section("Library")));
        assert!(lib_idx.is_some(), "Library section missing");
        let after_lib = &items[lib_idx.unwrap()..];
        let has_text_row = after_lib
            .iter()
            .any(|i| matches!(i, SettingItem::TextRow(r) if r.key == "local_library_dir"));
        assert!(has_text_row, "local_library_dir TextRow missing");
    }

    #[test]
    fn apply_text_setting_updates_local_library_dir() {
        let mut cfg = AppConfig::default();
        assert_eq!(cfg.local_library_dir, "");
        let result = apply_text_setting(&mut cfg, "local_library_dir", "/Users/test/Music".into());
        assert!(result.is_none(), "no engine-resync key expected");
        assert_eq!(cfg.local_library_dir, "/Users/test/Music");
    }

    #[test]
    fn settings_row_count_includes_text_rows() {
        let cfg = AppConfig::default();
        let count = settings_row_count(&cfg);
        let entry_count = (0..count)
            .filter_map(|i| settings_row_entry_at(&cfg, i))
            .count();
        assert_eq!(count, entry_count, "row count vs entry iteration mismatch");
        // At least one TextRow should be in the list.
        let any_text = (0..count).any(|i| {
            matches!(
                settings_row_entry_at(&cfg, i),
                Some(SettingsRowEntry::Text(_))
            )
        });
        assert!(any_text, "expected at least one TextRow in the new schema");
    }

    #[test]
    fn build_settings_has_sections_and_reset() {
        let cfg = AppConfig::default();
        let items = build_settings(&cfg);
        // At least one section header.
        assert!(items.iter().any(|i| matches!(i, SettingItem::Section(_))));
        // Reset sentinel exists at the end.
        let reset = items.iter().find_map(|i| match i {
            SettingItem::Row(r) if r.key == RESET_ALL_KEY => Some(r),
            _ => None,
        });
        assert!(reset.is_some(), "reset sentinel row missing");
    }

    /// Regression: the renderer used to dump every line into a single
    /// Paragraph with no scroll offset, so once `selected_row` advanced
    /// past `area.height` the focused row went off-screen and the user
    /// appeared "stuck". This test renders into a too-small TestBackend
    /// with the focus on the last row and asserts the focus marker is
    /// visible somewhere in the buffer.
    #[test]
    fn focused_row_stays_visible_when_past_visible_area() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let cfg = AppConfig::default();
        let last_row = settings_row_count(&cfg).saturating_sub(1);
        assert!(last_row > 6, "test assumes more rows than the test area");

        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_settings(frame, frame.area(), &cfg, last_row, None);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        // The focused-row marker `▸` must appear somewhere visible.
        let mut found_marker = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() == "▸" {
                    found_marker = true;
                    break;
                }
            }
        }
        assert!(
            found_marker,
            "focus marker ▸ should be visible after scrolling"
        );
    }

    #[test]
    fn modified_marker_lights_on_change() {
        let mut cfg = AppConfig::default();
        let pre = settings_row_at(&cfg, 0).unwrap();
        let pre_modified = pre.modified();
        // Flip the same key the first row maps to (whatever it is).
        let n = pre.options.len();
        if n > 1 {
            let next = (pre.current_idx + 1) % n;
            apply_setting(&mut cfg, pre.key, next);
            let post = settings_row_at(&cfg, 0).unwrap();
            // At least one of the two states should have modified=true
            // (depending on whether the default was the "off" or "on"
            // index, flipping toggles modified into the opposite state).
            assert!(pre_modified != post.modified());
        }
    }

    #[test]
    fn settings_row_count_excludes_sections() {
        let cfg = AppConfig::default();
        let total = build_settings(&cfg).len();
        let rows = settings_row_count(&cfg);
        assert!(
            rows < total,
            "rows should be fewer than total (sections exist)"
        );
    }

    #[test]
    fn apply_setting_by_key_updates_ai_flags() {
        let mut cfg = AppConfig {
            ai_beat_detection: false,
            ..Default::default()
        };
        let result = apply_setting(&mut cfg, "ai_beat_detection", 1);
        assert!(cfg.ai_beat_detection);
        assert!(result.is_none());

        let result = apply_setting(&mut cfg, "ai_beat_detection", 0);
        assert!(!cfg.ai_beat_detection);
        assert!(result.is_none());
    }
}
