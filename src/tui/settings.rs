use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
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
}

impl SettingRow {
    fn new(key: &'static str, label: &'static str, options: Vec<String>, current_idx: usize) -> Self {
        Self { key, label, options, current_idx }
    }
}

pub fn build_settings(config: &AppConfig) -> Vec<SettingRow> {
    vec![
        // FLAC is omitted — the dj.beatport.com web app's OAuth scope
        // doesn't include the /download/ endpoint, so picking Lossless
        // would silently fall back to 256k anyway. Branches with
        // partner-scope auth re-add it.
        SettingRow::new("audio_quality", "Audio Quality", vec!["256k".into(), "128k".into()],
            match config.audio_quality { AudioQuality::Standard => 1, _ => 0 }),
        SettingRow::new("preview_quality", "Preview Quality", vec!["128k".into(), "256k".into()],
            match config.preview_quality { AudioQuality::Standard => 0, _ => 1 }),
        SettingRow::new("bpm_mode", "BPM Mode", vec!["Glide".into(), "Lock".into()],
            match config.bpm_mode { BpmMode::Glide => 0, BpmMode::Lock => 1 }),
        SettingRow::new("split_cue", "Split Cue", vec!["Off".into(), "On".into()],
            if config.split_cue { 1 } else { 0 }),
        SettingRow::new("crossfade_bars", "Crossfade Bars", vec!["8".into(), "16".into(), "32".into(), "64".into(), "Auto".into()],
            if config.crossfade_bars_auto { 4 }
            else { match config.crossfade_bars { 8 => 0, 32 => 2, 64 => 3, _ => 1 } }),
        SettingRow::new("glide_bars", "Glide Bars", vec!["8".into(), "16".into(), "32".into(), "64".into(), "Max".into()],
            match config.glide_bars { 8 => 0, 32 => 2, 64 => 3, 0 => 4, _ => 1 }),
        SettingRow::new("jump_bars", "Jump Bars", vec!["4".into(), "8".into(), "16".into(), "32".into()],
            match config.jump_bars { 4 => 0, 16 => 2, 32 => 3, _ => 1 }),
        SettingRow::new("quantize_on", "Quantize", vec!["Off".into(), "On".into()],
            if config.quantize_on { 1 } else { 0 }),
        SettingRow::new("quantize_beats", "Quantize Beats", vec!["1/8".into(), "1/4".into(), "1/2".into(), "1".into(), "2".into(), "4".into(), "8".into()], {
            let q = config.quantize_beats;
            if      q < 0.1875 { 0 } // 1/8
            else if q < 0.375  { 1 } // 1/4
            else if q < 0.75   { 2 } // 1/2
            else if q < 1.5    { 3 } // 1
            else if q < 3.0    { 4 } // 2
            else if q < 6.0    { 5 } // 4
            else               { 6 } // 8
        }),
        SettingRow::new("tempo_range", "Tempo Range", vec!["±4%".into(), "±6%".into(), "±8%".into(), "±10%".into(), "±16%".into()],
            match config.tempo_range { 4 => 0, 6 => 1, 10 => 3, 16 => 4, _ => 2 }),
        SettingRow::new("nudge_percent", "Nudge %", vec!["1%".into(), "2%".into(), "3%".into(), "4%".into(), "5%".into()],
            match config.nudge_percent { 1 => 0, 2 => 1, 4 => 3, 5 => 4, _ => 2 }),
        SettingRow::new("mix_in_point", "Mix In Point", vec!["First Beat".into(), "Drop".into(), "Middle".into()],
            match config.mix_in_point { MixInPoint::FirstBeat | MixInPoint::FirstAudio => 0, MixInPoint::Drop => 1, MixInPoint::Middle => 2 }),
        SettingRow::new("smart_mix_out", "Smart Mix Out", vec!["Off".into(), "On".into()],
            if config.smart_mix_out { 1 } else { 0 }),
        SettingRow::new("shuffle_on_play", "Shuffle on Play", vec!["Off".into(), "On".into()],
            if config.shuffle_on_play { 1 } else { 0 }),
        SettingRow::new("compact_view", "View Mode", vec!["Compact".into(), "Full".into()],
            if config.compact_view { 0 } else { 1 }),
        SettingRow::new("browser", "Browser", vec!["Google Chrome".into(), "Safari".into(), "Firefox".into(), "Arc".into()],
            match config.browser.as_str() { "Safari" => 1, "Firefox" => 2, "Arc" => 3, _ => 0 }),
        SettingRow::new("ai_beat_detection", "AI Beat Detection", vec!["Off".into(), "On".into()],
            if config.ai_beat_detection { 1 } else { 0 }),
        SettingRow::new("ai_grid_validation", "AI Grid Validation", vec!["Off".into(), "On".into()],
            if config.ai_grid_validation { 1 } else { 0 }),
        SettingRow::new("ai_phrase_detection", "AI Phrase Detection", vec!["Off".into(), "On".into()],
            if config.ai_phrase_detection { 1 } else { 0 }),
        SettingRow::new("tx_beatmatched", "Transition: BeatMatched", vec!["Off".into(), "On".into()],
            if config.enabled_transitions.iter().any(|s| s == "BeatMatched") { 1 } else { 0 }),
        SettingRow::new("tx_echoout", "Transition: EchoOut", vec!["Off".into(), "On".into()],
            if config.enabled_transitions.iter().any(|s| s == "EchoOut") { 1 } else { 0 }),
        SettingRow::new("tx_bassswap", "Transition: BassSwap", vec!["Off".into(), "On".into()],
            if config.enabled_transitions.iter().any(|s| s == "BassSwap") { 1 } else { 0 }),
        SettingRow::new("tx_filtersweep", "Transition: FilterSweep", vec!["Off".into(), "On".into()],
            if config.enabled_transitions.iter().any(|s| s == "FilterSweep") { 1 } else { 0 }),
        SettingRow::new("tx_looproll", "Transition: LoopRoll", vec!["Off".into(), "On".into()],
            if config.enabled_transitions.iter().any(|s| s == "LoopRoll") { 1 } else { 0 }),
        SettingRow::new("edit_rules", "Edit Transition Rules", vec!["Open".into()], 0),
        SettingRow::new("output_device", "Output Device", {
            let mut opts: Vec<String> = vec!["System Default".into()];
            opts.extend(crate::audio::output_device_names());
            opts
        }, {
            let name = config.output_device.as_str();
            if name.is_empty() { 0 } else {
                crate::audio::output_device_names().iter()
                    .position(|n| n == name).map(|i| i + 1).unwrap_or(0)
            }
        }),
        SettingRow::new("monitor_device", "Monitor Device", {
            let mut opts: Vec<String> = vec!["Off".into()];
            opts.extend(crate::audio::output_device_names());
            opts
        }, {
            let name = config.monitor_device.as_str();
            if name.is_empty() { 0 } else {
                crate::audio::output_device_names().iter()
                    .position(|n| n == name).map(|i| i + 1).unwrap_or(0)
            }
        }),
        SettingRow::new("master_limiter", "Master Limiter", vec!["Off".into(), "Soft Knee".into()],
            match config.master_limiter { LimiterMode::Off => 0, LimiterMode::SoftKnee => 1 }),
        SettingRow::new("train_wreck", "Train Wreck", vec!["Off".into(), "Detect".into(), "Auto Bail".into()],
            match config.train_wreck_mode {
                crate::config::TrainWreckMode::Off => 0,
                crate::config::TrainWreckMode::Detect => 1,
                crate::config::TrainWreckMode::AutoBail => 2,
            }),
        SettingRow::new("pitch_stretch", "Pitch Stretch", vec![
            "Off".into(),
            if cfg!(feature = "rubberband") { "Rubberband".into() }
            else { "Rubberband (build --features rubberband)".into() },
            if cfg!(feature = "timestretch") { "Timestretch".into() }
            else { "Timestretch (build --features timestretch)".into() },
        ], match config.pitch_stretch_engine {
            crate::audio::pitch_stretch::PitchStretchEngine::Off => 0,
            crate::audio::pitch_stretch::PitchStretchEngine::Rubberband => 1,
            crate::audio::pitch_stretch::PitchStretchEngine::Timestretch => 2,
        }),
        SettingRow::new("dj_mode", "DJ Mode", vec!["Auto".into(), "Assist".into(), "Manual".into()],
            match config.claude_dj.mode { ClaudeDjMode::Auto => 0, ClaudeDjMode::Assist => 1, ClaudeDjMode::Manual => 2 }),
        SettingRow::new("dj_camelot", "DJ Camelot", vec!["Strict".into(), "Prefer".into(), "Off".into()],
            match config.claude_dj.camelot_strictness { Strictness::Strict => 0, Strictness::Prefer => 1, Strictness::Off => 2 }),
        SettingRow::new("dj_bpm_gap", "DJ BPM Gap", vec!["Strict".into(), "Prefer".into(), "Off".into()],
            match config.claude_dj.bpm_gap_strictness { Strictness::Strict => 0, Strictness::Prefer => 1, Strictness::Off => 2 }),
        SettingRow::new("dj_transitions", "DJ Transitions", vec!["Engine".into(), "Claude".into()],
            match config.claude_dj.transition_picker { TransitionPicker::Engine => 0, TransitionPicker::Claude => 1 }),
        SettingRow::new("dj_style", "DJ Style", vec!["Underground".into(), "Mainstream".into(), "Exploratory".into()],
            match config.claude_dj.style { DjStyle::Underground => 0, DjStyle::Mainstream => 1, DjStyle::Exploratory => 2 }),
        SettingRow::new("dj_quick_mix", "DJ Quick Mix", vec!["Off".into(), "On".into()],
            if config.claude_dj.quick_mix { 1 } else { 0 }),
        SettingRow::new("dj_memory", "DJ Memory", vec!["Off".into(), "On".into()],
            if config.claude_dj.memory_enabled { 1 } else { 0 }),
        SettingRow::new("resume_session", "Resume Session", vec!["Never".into(), "Ask".into(), "Always".into()],
            match config.resume_behavior { ResumeBehavior::Never => 0, ResumeBehavior::Ask => 1, ResumeBehavior::Always => 2 }),
        SettingRow::new("analyzer_engine", "Analyzer Engine", vec![
            "Built-in".into(),
            if cfg!(feature = "stratum") { "Stratum".into() }
            else { "Stratum (build --features stratum)".into() },
        ], match config.analyzer_engine { AnalyzerEngine::Builtin => 0, AnalyzerEngine::Stratum => 1 }),
        SettingRow::new("default_genre", "Default Genre", vec![config.default_genre.clone()], 0),
        SettingRow::new("favorite_genres", "Favorite Genres", vec![format!("{} selected", config.favorite_genres.len())], 0),
        SettingRow::new("logout", "Logout", vec![], 0),
    ]
}

/// Apply a setting change to the config. Dispatches by stable key
/// rather than row index so adding/reordering rows in
/// `build_settings` doesn't cascade into renumbered match arms.
/// Unknown keys silently do nothing (forward-compatible with patches
/// that add new rows + handlers).
pub fn apply_setting(config: &mut AppConfig, key: &str, option_idx: usize) -> Option<&'static str> {
    match key {
        "audio_quality" => { config.audio_quality = if option_idx == 1 { AudioQuality::Standard } else { AudioQuality::High }; None }
        "preview_quality" => { config.preview_quality = if option_idx == 0 { AudioQuality::Standard } else { AudioQuality::High }; None }
        "bpm_mode" => { config.bpm_mode = match option_idx { 0 => BpmMode::Glide, _ => BpmMode::Lock }; None }
        "split_cue" => { config.split_cue = option_idx == 1; None }
        "crossfade_bars" => {
            match option_idx {
                0 => { config.crossfade_bars = 8;  config.crossfade_bars_auto = false; }
                2 => { config.crossfade_bars = 32; config.crossfade_bars_auto = false; }
                3 => { config.crossfade_bars = 64; config.crossfade_bars_auto = false; }
                4 => { config.crossfade_bars_auto = true; }
                _ => { config.crossfade_bars = 16; config.crossfade_bars_auto = false; }
            }
            Some("crossfade_bars_changed")
        }
        "glide_bars" => { config.glide_bars = match option_idx { 0 => 8, 2 => 32, 3 => 64, 4 => 0, _ => 16 }; None }
        "jump_bars" => { config.jump_bars = match option_idx { 0 => 4, 2 => 16, 3 => 32, _ => 8 }; Some("jump_bars_changed") }
        "quantize_on" => { config.quantize_on = option_idx == 1; Some("quantize_changed") }
        "quantize_beats" => {
            config.quantize_beats = match option_idx {
                0 => 0.125, 1 => 0.25, 2 => 0.5, 3 => 1.0,
                4 => 2.0, 5 => 4.0, 6 => 8.0, _ => 1.0,
            };
            Some("quantize_changed")
        }
        "tempo_range" => { config.tempo_range = match option_idx { 0 => 4, 1 => 6, 3 => 10, 4 => 16, _ => 8 }; None }
        "nudge_percent" => { config.nudge_percent = match option_idx { 0 => 1, 1 => 2, 3 => 4, 4 => 5, _ => 3 }; None }
        "mix_in_point" => { config.mix_in_point = match option_idx { 0 => MixInPoint::FirstBeat, 1 => MixInPoint::Drop, _ => MixInPoint::Middle }; None }
        "smart_mix_out" => { config.smart_mix_out = option_idx == 1; None }
        "shuffle_on_play" => { config.shuffle_on_play = option_idx == 1; None }
        "compact_view" => { config.compact_view = option_idx == 0; None }
        "browser" => { config.browser = match option_idx { 1 => "Safari", 2 => "Firefox", 3 => "Arc", _ => "Google Chrome" }.into(); None }
        "ai_beat_detection" => { config.ai_beat_detection = option_idx == 1; None }
        "ai_grid_validation" => { config.ai_grid_validation = option_idx == 1; None }
        "ai_phrase_detection" => { config.ai_phrase_detection = option_idx == 1; None }
        "tx_beatmatched" => { toggle_transition(config, "BeatMatched", option_idx == 1); Some("transitions_changed") }
        "tx_echoout" => { toggle_transition(config, "EchoOut", option_idx == 1); Some("transitions_changed") }
        "tx_bassswap" => { toggle_transition(config, "BassSwap", option_idx == 1); Some("transitions_changed") }
        "tx_filtersweep" => { toggle_transition(config, "FilterSweep", option_idx == 1); Some("transitions_changed") }
        "tx_looproll" => { toggle_transition(config, "LoopRoll", option_idx == 1); Some("transitions_changed") }
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
            config.master_limiter = if option_idx == 0 { LimiterMode::Off } else { LimiterMode::SoftKnee };
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
            config.claude_dj.mode = match option_idx { 0 => ClaudeDjMode::Auto, 1 => ClaudeDjMode::Assist, _ => ClaudeDjMode::Manual };
            Some("claudedj_changed")
        }
        "dj_camelot" => {
            config.claude_dj.camelot_strictness = match option_idx { 0 => Strictness::Strict, 2 => Strictness::Off, _ => Strictness::Prefer };
            Some("claudedj_changed")
        }
        "dj_bpm_gap" => {
            config.claude_dj.bpm_gap_strictness = match option_idx { 0 => Strictness::Strict, 2 => Strictness::Off, _ => Strictness::Prefer };
            Some("claudedj_changed")
        }
        "dj_transitions" => {
            config.claude_dj.transition_picker = match option_idx { 1 => TransitionPicker::Claude, _ => TransitionPicker::Engine };
            Some("claudedj_changed")
        }
        "dj_style" => {
            config.claude_dj.style = match option_idx { 1 => DjStyle::Mainstream, 2 => DjStyle::Exploratory, _ => DjStyle::Underground };
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
            config.resume_behavior = match option_idx { 0 => ResumeBehavior::Never, 1 => ResumeBehavior::Ask, _ => ResumeBehavior::Always };
            None
        }
        "analyzer_engine" => {
            // Always honor the toggle. If the `stratum` feature isn't
            // compiled in, resolve_bpm transparently falls back to the
            // built-in detector — the setting still records the user's
            // preference so a rebuild with the feature picks it up.
            config.analyzer_engine = match option_idx { 1 => AnalyzerEngine::Stratum, _ => AnalyzerEngine::Builtin };
            if option_idx == 1 && !cfg!(feature = "stratum") {
                Some("stratum_fallback")
            } else {
                None
            }
        }
        "default_genre" => Some("pick_genre"),
        "favorite_genres" => Some("pick_favorites"),
        "logout" => Some("logout"),
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

/// Render the settings screen.
pub fn render_settings(
    frame: &mut Frame,
    area: Rect,
    config: &AppConfig,
    selected_row: usize,
) {
    let settings = build_settings(config);
    let inner = area; // no inner block — outer block in app.rs handles it

    let mut lines: Vec<Line> = Vec::new();

    for (i, row) in settings.iter().enumerate() {
        let is_selected = i == selected_row;
        let marker = if is_selected { "▸ " } else { "  " };

        let mut spans = vec![
            Span::styled(marker, Style::default().fg(if is_selected { Color::White } else { Color::DarkGray })),
            Span::styled(
                format!("{}:  ", row.label),
                Style::default().fg(if is_selected { Color::White } else { Color::Gray }),
            ),
        ];

        if row.options.is_empty() {
            // Logout — just the label
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
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(
                        opt.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }

        lines.push(Line::from(spans));
    }

    // Hints are rendered in the global hints bar, not here

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Number of settings rows. Convenience wrapper around
/// `build_settings(...).len()` for callers that just need the count
/// (selection bounds, click target generation).
pub fn settings_count(config: &AppConfig) -> usize {
    build_settings(config).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn apply_setting_by_key_updates_ai_flags() {
        let mut c = AppConfig::default();
        apply_setting(&mut c, "ai_beat_detection", 1);
        assert!(c.ai_beat_detection);
        apply_setting(&mut c, "ai_grid_validation", 1);
        assert!(c.ai_grid_validation);
        apply_setting(&mut c, "ai_phrase_detection", 1);
        assert!(c.ai_phrase_detection);
    }

    #[test]
    fn unknown_key_is_silent_no_op() {
        let mut c = AppConfig::default();
        let before = c.audio_quality;
        let action = apply_setting(&mut c, "this_key_does_not_exist", 7);
        assert!(action.is_none(), "unknown keys must return None");
        assert_eq!(c.audio_quality, before, "config must be unchanged");
    }

    #[test]
    fn keys_are_unique() {
        // Two rows with the same key would silently shadow each other
        // — dispatch matches first arm, second row's UI works but its
        // updates land on the first arm's match.
        let config = AppConfig::default();
        let rows = build_settings(&config);
        let mut seen = std::collections::HashSet::new();
        for row in &rows {
            assert!(seen.insert(row.key),
                "duplicate setting key: {:?}", row.key);
        }
    }

    #[test]
    fn build_settings_current_index_in_bounds() {
        let config = AppConfig::default();
        for (i, row) in build_settings(&config).iter().enumerate() {
            if !row.options.is_empty() {
                assert!(row.current_idx < row.options.len(),
                    "row {i} ({}): current={} >= options.len()={}",
                    row.label, row.current_idx, row.options.len());
            }
        }
    }
}
