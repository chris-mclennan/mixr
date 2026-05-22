use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_bpm_mode")]
    pub bpm_mode: BpmMode,
    #[serde(default = "default_audio_quality")]
    pub audio_quality: AudioQuality,
    #[serde(default)]
    pub split_cue: bool,
    #[serde(default = "default_genre")]
    pub default_genre: String,
    #[serde(default)]
    pub favorite_genres: Vec<String>,
    #[serde(default = "default_preview_quality")]
    pub preview_quality: AudioQuality,
    #[serde(default = "default_crossfade_bars")]
    pub crossfade_bars: u32,
    /// When true, the engine picks a crossfade length per-mix based on
    /// the incoming track's genre (short-genre-cuts ≈ 2–4 bars,
    /// progressive / techno / afro ≈ 32–64 bars). Overrides the
    /// manual `crossfade_bars` value.
    #[serde(default)]
    pub crossfade_bars_auto: bool,
    /// Quantize hot-cue jumps, bar jumps, and loops to beat
    /// boundaries — CDJ-3000 behavior. Press near a beat, the
    /// engine waits for the next boundary at `quantize_beats`
    /// resolution, then fires.
    #[serde(default = "default_true")]
    pub quantize_on: bool,
    /// Quantize resolution in BEATS (CDJ values: 0.125, 0.25, 0.5,
    /// 1, 2, 4, 8). 1 = on the beat (default, matches CDJ); 4 =
    /// once per bar; 0.5 = on the off-beat too. Fractional values
    /// land sub-beat for tighter scratch / hot-cue feel.
    #[serde(default = "default_quantize_beats")]
    pub quantize_beats: f64,
    #[serde(default = "default_mix_in_point")]
    pub mix_in_point: MixInPoint,
    #[serde(default = "default_true")]
    pub smart_mix_out: bool,
    #[serde(default)]
    pub shuffle_on_play: bool,
    #[serde(default = "default_true")]
    pub compact_view: bool,
    /// Dashboard overall layout — `Full` (classic stacked view) or
    /// `Panel` (controller + one secondary section, short). Cycled
    /// via `v` on the dashboard.
    #[serde(default)]
    pub dash_layout: crate::tui::dashboard::DashLayout,
    /// Which secondary section is visible in Panel layout.
    #[serde(default)]
    pub dash_panel_section: crate::tui::dashboard::PanelSection,
    #[serde(default = "default_browser")]
    pub browser: String,
    #[serde(default = "default_glide_bars")]
    pub glide_bars: u32,
    #[serde(default = "default_jump_bars")]
    pub jump_bars: u32,
    #[serde(default = "default_tempo_range")]
    pub tempo_range: u32,
    #[serde(default = "default_nudge_percent")]
    pub nudge_percent: u32,
    #[serde(default = "default_nudge_tap_ms")]
    pub nudge_tap_ms: u32,
    #[serde(default)]
    pub claude_dj_enabled: bool,
    #[serde(default)]
    pub ai_beat_detection: bool,
    #[serde(default)]
    pub ai_grid_validation: bool,
    #[serde(default)]
    pub ai_phrase_detection: bool,
    /// Transitions the rule engine / auto-selector is allowed to pick.
    /// Missing types default to enabled.
    #[serde(default = "default_enabled_transitions")]
    pub enabled_transitions: Vec<String>,
    /// Pitch-invariant stretch engine. Off = varispeed (pitch follows tempo).
    /// Rubberband needs the `rubberband` Cargo feature.
    #[serde(default)]
    pub pitch_stretch_engine: crate::audio::pitch_stretch::PitchStretchEngine,
    /// Soft-knee limiter on the master output (Off = hard ±1.0 clamp).
    #[serde(default)]
    pub master_limiter: LimiterMode,
    /// Train-wreck handling — what to do if a crossfade drifts badly
    /// out of phase mid-mix. AutoBail (default) salvages it via
    /// EchoOut; Detect just notifies; Off disables watching entirely.
    #[serde(default)]
    pub train_wreck_mode: TrainWreckMode,
    /// Main output device. Empty = system default; named = use that
    /// specific cpal output device (e.g. "MIXSTREAM PRO GO Audio")
    /// without changing the macOS system default. Applied at engine
    /// init — change requires a restart to take effect.
    #[serde(default)]
    pub output_device: String,
    /// Optional second output device that plays the *incoming* deck
    /// pre-mix (a DJ headphone cue bus). Empty string = disabled.
    /// Set by name, e.g. "External Headphones".
    #[serde(default)]
    pub monitor_device: String,
    /// Path to a local audio library — when non-empty, the browse
    /// menu shows a "Local Library" entry that lists files (FLAC,
    /// AAC, MP3, M4A, WAV, OGG) found by walking this directory.
    /// Tracks play through the same engine as Beatport tracks; the
    /// difference is just the source (file system vs streaming).
    /// Default empty = local library hidden.
    #[serde(default)]
    pub local_library_dir: String,
    /// Path to a rekordbox XML library export (File → Export
    /// Collection in xml format inside rekordbox). When set, the
    /// browse menu shows a "Rekordbox" entry that surfaces every
    /// track from the export with rekordbox's BPM/key analysis.
    /// Empty = hidden.
    #[serde(default)]
    pub rekordbox_xml: String,
    /// Path to an Engine DJ database (`m.db`, SQLite). Numark and
    /// Denon hardware writes this format. Desktop default is
    /// `~/Music/Engine Library/Database2/m.db`. Empty = hidden.
    #[serde(default)]
    pub engine_dj_db: String,
    /// Path to a Serato `database V2` file (typically at
    /// `~/Music/_Serato_/database V2`). Empty = hidden.
    #[serde(default)]
    pub serato_db: String,
    /// What to do with `~/.mixr/session.json` on launch. `Never` (default)
    /// starts fresh; `Always` reloads the last queue/history/playing
    /// track; `Ask` shows a prompt at startup.
    #[serde(default)]
    pub resume_behavior: ResumeBehavior,
    /// Which engine does BPM / key detection. `Builtin` is the
    /// in-crate onset + autocorrelation path; `Stratum` routes through
    /// `stratum-dsp` when compiled with the `stratum` feature. Used
    /// for A/B tests when a mix sounds off due to wrong BPM.
    #[serde(default)]
    pub analyzer_engine: AnalyzerEngine,
    /// Claude DJ behavior knobs. Independent of `claude_dj_enabled` (the
    /// master toggle) — these tune *how* Claude DJs when active.
    #[serde(default)]
    pub claude_dj: ClaudeDjSettings,
}

/// Tunables for Claude DJ behavior. Composed into the system prompt and
/// the engine's manual-mix plumbing. Everything has a sensible default
/// so missing keys in older config files still parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeDjSettings {
    /// How Claude drives the mix.
    /// - `Auto` (default): the engine's transition curves run; Claude
    ///   picks tracks, tunes tempo, nudges. Today's behavior.
    /// - `Assist`: Auto drives, Claude comments/suggests but doesn't
    ///   move the crossfader.
    /// - `Manual`: Claude rides the pitch fader, moves the crossfader,
    ///   EQs the kills, times the drops. Engine provides safety rails
    ///   (phase-sync math, downbeat align) but auto curves don't run.
    #[serde(default)]
    pub mode: ClaudeDjMode,
    /// How hard the prompt enforces Camelot-wheel key matching on track
    /// selection. `Strict` = "must match or wheel-neighbor"; `Prefer`
    /// = "lean toward matches but surprises are ok"; `Off` = not
    /// mentioned. Default `Prefer`.
    #[serde(default)]
    pub camelot_strictness: Strictness,
    /// How hard the prompt enforces BPM-gap limits on track selection.
    /// Same levels as camelot. Default `Prefer`.
    #[serde(default)]
    pub bpm_gap_strictness: Strictness,
    /// Who picks the transition type (BeatMatched / EchoOut / BassSwap /
    /// FilterSweep / LoopRoll) for each mix. `Engine` runs the rule
    /// engine + key-distance heuristic; `Claude` lets the model call
    /// `set_transition` every time.
    #[serde(default)]
    pub transition_picker: TransitionPicker,
    /// Prompt flavor — influences track-selection style without
    /// changing the hard rules. `Underground` = dig deep, pick from
    /// charts not Top 100; `Mainstream` = Top 10 is fine; `Exploratory`
    /// = cross-genre, break rules. Default `Underground`.
    #[serde(default)]
    pub style: DjStyle,
    /// Read + write the persistent `~/.mixr/dj_memory.json` so Claude
    /// carries "what worked" forward across sessions. Default on.
    #[serde(default = "default_true")]
    pub memory_enabled: bool,
    /// Quick-mix test mode: once the playing deck has been running long
    /// enough for a phrase boundary and the incoming deck is loaded,
    /// auto-fire a crossfade. Bypasses the usual "wait for natural
    /// trigger time" logic so each mix lands in ~45s instead of ~4min —
    /// used for iterating on DJ behavior without listening to full tracks.
    /// Default off.
    #[serde(default)]
    pub quick_mix: bool,
    /// How many bars of musical playback before quick-mix fires the
    /// crossfade. 16 = one phrase (default — past intro, into the
    /// first groove). At 128 BPM ≈ 30 s; at 90 BPM ≈ 42 s.
    /// Tempo-independent because bars are musical time, not seconds.
    #[serde(default = "default_quick_mix_bars")]
    pub quick_mix_bars: u32,
}

fn default_quick_mix_bars() -> u32 { 16 }

impl Default for ClaudeDjSettings {
    fn default() -> Self {
        Self {
            mode: ClaudeDjMode::default(),
            camelot_strictness: Strictness::default(),
            bpm_gap_strictness: Strictness::default(),
            transition_picker: TransitionPicker::default(),
            style: DjStyle::default(),
            // Training memory on by default — matches the `default = true`
            // hint for the serde path, and makes the "rate a mix with
            // +/-" feature work out of the box without settings fiddling.
            memory_enabled: true,
            // Quick mix is opt-in; default off so a normal session plays
            // full tracks.
            quick_mix: false,
            quick_mix_bars: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeDjMode {
    #[default]
    Auto,
    Assist,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Strictness {
    Strict,
    #[default]
    Prefer,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransitionPicker {
    #[default]
    Engine,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DjStyle {
    #[default]
    Underground,
    Mainstream,
    Exploratory,
}

/// What to do when the engine detects a crossfade going badly off
/// (sustained phase RMS over a threshold mid-mix).
/// - `Off`: don't watch.
/// - `Detect`: log + toast, no action.
/// - `AutoBail`: switch the in-progress transition to EchoOut at the
///   moment of detection, hard-cut the playing deck, let the echo
///   tail mask the wreck while the incoming track fades in cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrainWreckMode {
    Off,
    Detect,
    #[default]
    AutoBail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LimiterMode {
    /// Hard ±1.0 ceiling — existing behaviour.
    Off,
    /// Soft knee above 0.7 → asymptote to 1.0. Default.
    #[default]
    SoftKnee,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BpmMode {
    Glide,
    Lock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioQuality {
    Lossless,
    High,
    Standard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MixInPoint {
    FirstAudio,
    FirstBeat,
    Drop,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResumeBehavior {
    #[default]
    Never,
    Ask,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnalyzerEngine {
    #[default]
    Builtin,
    Stratum,
}

// Defaults
fn default_bpm_mode() -> BpmMode { BpmMode::Glide }
fn default_audio_quality() -> AudioQuality { AudioQuality::High }
fn default_preview_quality() -> AudioQuality { AudioQuality::Standard }
fn default_genre() -> String { "Melodic House & Techno".into() }
fn default_crossfade_bars() -> u32 { 16 }
fn default_quantize_beats() -> f64 { 1.0 }
fn default_mix_in_point() -> MixInPoint { MixInPoint::FirstBeat }
fn default_true() -> bool { true }
fn default_browser() -> String { "Google Chrome".into() }
fn default_enabled_transitions() -> Vec<String> {
    vec!["BeatMatched".into(), "EchoOut".into(), "BassSwap".into(), "FilterSweep".into(), "LoopRoll".into()]
}
fn default_glide_bars() -> u32 { 32 }
fn default_jump_bars() -> u32 { 8 }
fn default_tempo_range() -> u32 { 8 }
fn default_nudge_percent() -> u32 { 3 }
fn default_nudge_tap_ms() -> u32 { 300 }

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bpm_mode: default_bpm_mode(),
            audio_quality: default_audio_quality(),
            split_cue: false,
            default_genre: default_genre(),
            favorite_genres: Vec::new(),
            preview_quality: default_preview_quality(),
            crossfade_bars: default_crossfade_bars(),
            crossfade_bars_auto: false,
            quantize_on: true,
            quantize_beats: default_quantize_beats(),
            mix_in_point: default_mix_in_point(),
            smart_mix_out: true,
            shuffle_on_play: false,
            compact_view: true,
            dash_layout: Default::default(),
            dash_panel_section: Default::default(),
            browser: default_browser(),
            glide_bars: default_glide_bars(),
            jump_bars: default_jump_bars(),
            tempo_range: default_tempo_range(),
            nudge_percent: default_nudge_percent(),
            nudge_tap_ms: default_nudge_tap_ms(),
            claude_dj_enabled: false,
            ai_beat_detection: false,
            ai_grid_validation: false,
            ai_phrase_detection: false,
            enabled_transitions: default_enabled_transitions(),
            pitch_stretch_engine: crate::audio::pitch_stretch::PitchStretchEngine::default(),
            master_limiter: LimiterMode::default(),
            train_wreck_mode: TrainWreckMode::default(),
            output_device: String::new(),
            monitor_device: String::new(),
            local_library_dir: String::new(),
            rekordbox_xml: String::new(),
            engine_dj_db: String::new(),
            serato_db: String::new(),
            resume_behavior: ResumeBehavior::default(),
            analyzer_engine: AnalyzerEngine::default(),
            claude_dj: ClaudeDjSettings::default(),
        }
    }
}

impl AppConfig {
    fn config_path() -> PathBuf {
        let dir = dirs::home_dir()
            .unwrap_or_default()
            .join(".mixr");
        std::fs::create_dir_all(&dir).ok();
        dir.join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            std::fs::write(path, json).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claudedj_ipc_accepts_camel_case_quick_mix_bars() {
        // The struct has #[serde(rename_all = "camelCase")] on its
        // parent, so the IPC key must be `quickMixBars` not
        // `quick_mix_bars`. Pinned so the rename can't be silently
        // dropped — a docs/IPC mismatch would be a real bug.
        let json = r#"{"mode":"manual","quickMixBars":32}"#;
        let s: ClaudeDjSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.quick_mix_bars, 32);
        assert_eq!(s.mode, ClaudeDjMode::Manual);
    }

    #[test]
    fn claudedj_ipc_unknown_keys_are_ignored() {
        // Forward-compatibility: an IPC patch with extra keys (e.g.
        // from a newer client) shouldn't fail. Defaults fill in.
        let json = r#"{"unknownFieldXYZ":42}"#;
        let s: ClaudeDjSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.quick_mix_bars, 16, "default kicks in");
    }
}
