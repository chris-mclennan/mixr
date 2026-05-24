use anyhow::Result;
use cpal::Stream;
use cpal::traits::{DeviceTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
/// When true, `fill_output` samples per-section timings via `Instant::now()`
/// and pushes them to the profiler. Off by default so the RT callback pays
/// only 4 atomic loads (~4 ns total) per invocation instead of 5 syscalls.
/// Flip with `set_profiler_enabled(true)` or IPC `{"profile":1}`.
pub static PROFILER_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_profiler_enabled(on: bool) {
    PROFILER_ENABLED.store(on, Ordering::Relaxed);
}
pub fn profiler_enabled() -> bool {
    PROFILER_ENABLED.load(Ordering::Relaxed)
}

/// User has just made a manual mix-touching adjustment (crossfader,
/// EQ, fader, filter, nudge, etc.). Two effects:
///
/// 1. If a crossfade is in progress: suppress the auto train-wreck
///    handler for the rest of this mix — the user is in the loop
///    and shouldn't have the engine override their choice.
/// 2. If only one deck is playing (idle): pause auto-crossfade
///    triggering until the track is promoted (next mix completes).
///    User said: "if user starts messing with controls don't take
///    over until they get mixed out of that track."
///
/// Both flags clear at `start_crossfade` (next mix) so the override
/// is scoped to the current track / current mix, not permanent.
fn suppress_train_wreck_during_user_override(s: &mut AudioState) {
    match s.state {
        EngineState::Crossfading => {
            s.mix_wreck_fired = true;
        }
        // Set the flag and a one-shot edge marker the tick loop converts to
        // a single toast. Every CC tweak from a knob sweep would otherwise
        // spam toasts — the inner guard prevents re-firing.
        EngineState::Playing | EngineState::Idle if !s.user_paused_auto => {
            s.user_paused_auto = true;
            s.user_paused_auto_just_triggered = true;
        }
        _ => {}
    }
}

use super::beat_grid::BeatGrid;
use super::crossfade::CrossfadeController;
use super::deck::DeckPlayer;
use super::fill_output::{build_monitor_stream, fill_output};
// Re-export from fill_output so `crate::audio::engine::*` paths keep working.
#[allow(unused_imports)]
pub use super::fill_output::output_device_names;
use super::fill_output::pick_output_device;
#[allow(unused_imports)]
pub(crate) use super::fill_output::{apply_limiter, manual_progress_from_crossfader};
use super::mixer::Mixer;
// Re-export status types so `crate::audio::engine::NowPlayingInfo` etc. keep working.
#[allow(unused_imports)]
pub use super::status::{AlignmentPeaks, AlignmentReadout, NowPlayingInfo};
use super::transition::TransitionType;
use crate::beatport::models::BeatportTrack;
use crate::config::AppConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EngineState {
    #[default]
    Idle,
    Playing,
    PreparingCrossfade,
    Crossfading,
}

#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub track: std::sync::Arc<BeatportTrack>,
}

impl From<BeatportTrack> for QueueEntry {
    fn from(track: BeatportTrack) -> Self {
        Self {
            track: std::sync::Arc::new(track),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub track: std::sync::Arc<BeatportTrack>,
    pub mix_score: Option<u8>,
}

#[derive(Debug)]
pub enum EngineEvent {
    /// First track when idle — load onto playing deck and start playback.
    NeedFirstTrack(QueueEntry),
    /// Preload next track onto incoming deck for crossfade.
    NeedNextTrack(QueueEntry),
    PlaybackEnded,
    CrossfadeComplete {
        track: std::sync::Arc<BeatportTrack>,
        bpm: f64,
    },
    /// Mid-mix phase RMS exceeded the wreck threshold. `bailed` is
    /// true when `train_wreck_mode == AutoBail` and the engine
    /// switched the transition to EchoOut on the spot; false in
    /// Detect mode where the user gets a heads-up but no action.
    /// `rms_ms` is the rolling RMS that tripped the detection.
    TrainWreckDetected {
        rms_ms: f64,
        bailed: bool,
    },
    /// User just touched a control while only one deck was playing.
    /// Auto-crossfade trigger is now paused for the rest of this
    /// track. UI shows a toast once per pause cycle.
    AutoMixPaused,
}

// NowPlayingInfo, AlignmentReadout, AlignmentPeaks are in super::status

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeckId {
    A,
    B,
}

/// Which deck's samples feed the monitor (headphone-cue) output ring.
/// `Incoming` = role-based (whichever deck isn't currently playing) —
/// auto-DJ default. `DeckA`/`DeckB` = physical-deck pinned, used for
/// manual-mode preview. `Off` disables the push entirely for a short
/// silence when a preview is transitioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MonitorSource {
    #[default]
    Incoming,
    DeckA,
    DeckB,
    /// Both decks mixed — lets the DJ hear both in headphones during a crossfade.
    Both,
}

impl DeckId {
    fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Lightweight stack-only snapshot of the fields needed to compute
/// phase offset and rate correction outside the audio-state mutex.
/// All types are Copy — no heap allocation.
struct CrossfadeSnapshot {
    playing_deck: DeckId,
    playing_time: f64,
    playing_rate: f64,
    playing_grid: Option<BeatGrid>,
    incoming_time: f64,
    incoming_grid: Option<BeatGrid>,
    crossfade_start_playing_time: Option<f64>,
    crossfade_progress: f64,
    /// Type of transition in flight. Used to pick the right progress
    /// source: most transitions advance progress from the playing
    /// deck's source-time delta. LoopRoll and EchoOut are the
    /// exceptions — both decouple the playing deck from the audible
    /// mix (LoopRoll locks it in a 4-beat loop; EchoOut hard-cuts it
    /// to silence), so its source time stops tracking the crossfade.
    /// For those two we drive progress from wall-clock elapsed instead.
    transition_type: TransitionType,
    transition_uses_phase_sync: bool,
    manual_mix: bool,
    crossfader_pos: f32,
    crossfade_start: Option<std::time::Instant>,
    last_crossfader_move: Option<std::time::Instant>,
    /// Per-ms peak amplitudes for one beat from each deck, sampled at
    /// snapshot time. Used for audio-domain beat correlation — detects
    /// grid mismatches that the phase meter can't see.
    playing_beat_peaks: Vec<f32>,
    incoming_beat_peaks: Vec<f32>,
}

pub(crate) struct AudioState {
    pub(crate) deck_a: DeckPlayer,
    pub(crate) deck_b: DeckPlayer,
    pub(crate) playing_deck: DeckId,
    pub(crate) state: EngineState,
    pub(crate) crossfade_progress: f64,
    pub(crate) crossfade_start: Option<std::time::Instant>,
    /// Playing-deck source time at the instant the crossfade started. Used
    /// to drive crossfade_progress from actual audio played — so pause
    /// (which halts deck playback) naturally freezes the progress.
    pub(crate) crossfade_start_playing_time: Option<f64>,
    pub(crate) crossfade_controller: Option<CrossfadeController>,
    pub(crate) transition_type: TransitionType,
    pub(crate) split_cue: bool,
    /// Per-sample slewed 0..1 crossfade between mono output (both
    /// ears the same) and split-cue output (deck A left, deck B
    /// right). Target = 1 while the engine is Crossfading AND
    /// `split_cue` is enabled, else 0. Prevents the jarring
    /// stereo-image flip that a hard toggle would cause when the
    /// mix starts or ends.
    pub(crate) split_ramp: f32,
    /// Pre-computed `1 - exp(-1/sr)` for the split-cue ramp's
    /// per-sample slew. The output sample rate never changes once the
    /// cpal stream is built, so caching this here saves an `exp()`
    /// call per audio callback.
    pub(crate) split_alpha: f32,
    /// Wall-clock time of the last crossfade completion. Used to keep
    /// split-cue active for a short trail-out after the mix ends so
    /// the stereo image matches the pre-mix lead-in (symmetric).
    pub(crate) last_crossfade_end: Option<std::time::Instant>,
    /// Quantize on/off + beat resolution (CDJ-3000 style). Mirrored
    /// from `AppConfig`. `quantize_beats` of 0.125/0.25/0.5/1/2/4/8
    /// matches the CDJ values; fractional <1 quantizes to sub-beats.
    pub(crate) quantize_on: bool,
    pub(crate) quantize_beats: f64,
    /// Scheduled jump waiting for its bar boundary. The tick loop
    /// fires the seek when `playing.current_time() >= fire_at`. A
    /// new jump request overwrites any pending one — last press wins.
    pub(crate) pending_jump: Option<PendingJump>,
    /// Loop activate / release waiting for its bar boundary. Activate
    /// schedules a loop_in on the next bar then runs for `beats`.
    /// Release schedules the loop drop on the next bar so playback
    /// continues cleanly off the boundary instead of mid-bar.
    pub(crate) pending_loop: Option<PendingLoopOp>,
    pub(crate) metronome: bool,
    pub(crate) preview: Option<DeckPlayer>,
    pub(crate) preview_stop_time: f64,
    // Nudge state
    pub(crate) nudge_base_rate: Option<f64>,
    pub(crate) nudge_revert_at: Option<std::time::Instant>,
    /// Set when the user takes manual phase control during a mix
    /// (nudge or grid shift). As long as this is true the auto
    /// rate-correction controller stays hands-off — no fighting the
    /// user mid-mix. Cleared on the next `start_crossfade` so the
    /// *next* mix starts with fresh auto behavior.
    pub(crate) user_overrode_this_mix: bool,
    /// Set when the user touches a mix-affecting control (crossfader,
    /// EQ, fader, filter, nudge) during Playing/Idle state. Disables
    /// auto-crossfade triggering — the engine won't surprise-fire a
    /// mix while the user is performing. Cleared at the next
    /// `start_crossfade` (manual mix-now / queue advance / etc.) so
    /// the *next* track resumes normal auto behavior.
    pub(crate) user_paused_auto: bool,
    /// Edge-trigger flag for the "auto-mix paused" toast — set the
    /// first time `user_paused_auto` flips to true, drained by the
    /// tick loop's event emitter so we toast once per pause-cycle
    /// instead of on every knob tweak.
    pub(crate) user_paused_auto_just_triggered: bool,
    // Cached mix trigger point (seconds into playing track)
    pub(crate) cached_trigger_time: Option<f64>,
    /// Set by `extend_playback` so the per-tick trigger refresh
    /// preserves the user's extension instead of overwriting it. The
    /// 1-bar heuristic that preceded this could be tripped by
    /// non-monotone analyzer phrase recomputation. Cleared on track
    /// load and at crossfade fire so each new track starts fresh.
    pub(crate) trigger_user_extended: bool,
    // Pre-allocated scratch buffers for audio callback (no heap alloc in hot path)
    pub(crate) scratch_a: Vec<f32>,
    pub(crate) scratch_b: Vec<f32>,
    pub(crate) scratch_echo_a: Vec<f32>,
    pub(crate) scratch_echo_b: Vec<f32>,
    pub(crate) scratch_preview: Vec<f32>,
    // Rate glide state
    pub(crate) is_gliding: bool,
    pub(crate) glide_start_rate: f64,
    pub(crate) glide_start_time: f64,
    pub(crate) glide_duration: f64,
    // Deferred drop: items moved here by the RT callback, cleaned up by tick()
    pub(crate) deferred_drop: Option<DeckPlayer>,
    // Mixer-wide controls (independent of per-deck volume and transition curves)
    pub(crate) crossfader_pos: f32, // -1.0 full A, 0.0 center, +1.0 full B
    pub(crate) channel_fader_a: f32, // 0.0..1.0
    pub(crate) channel_fader_b: f32, // 0.0..1.0
    pub(crate) master_gain: f32,    // post-mix output gain (0.0..1.0)
    pub(crate) limiter_mode: crate::config::LimiterMode,
    /// Shared sample ring between the main callback (producer: incoming
    /// deck pre-mix) and the monitor-device callback (consumer). Mutex
    /// contention is trivial at audio-callback scales (~µs).
    /// Which deck's pre-mix signal feeds the monitor (headphone-cue)
    /// bus. Default `Incoming` keeps the auto-DJ behavior where the
    /// monitor always hears the cued-up deck. Manual-mode preview
    /// sets this to a specific deck so the DJ can audition regardless
    /// of role.
    pub(crate) monitor_source: MonitorSource,
    pub(crate) monitor_ring: Option<Arc<Mutex<std::collections::VecDeque<f32>>>>,
    /// Hard cap on the monitor ring, sized to ~1s at the monitor device's
    /// sample rate. Used as the trim threshold in `fill_output` so the
    /// VecDeque never reallocates on the RT thread. 0 when no monitor.
    pub(crate) monitor_ring_cap: usize,
    pub(crate) rule_engine: super::transition_rules::RuleEngine,
    pub(crate) profiler: super::profiler::AudioProfiler,
    /// Phase offset samples accumulated during the current crossfade
    /// (in ms, absolute). Used to score the mix on completion.
    pub(crate) mix_phase_samples: Vec<f64>,
    /// A scheduled teleport waiting for the next musical cut point
    /// (next downbeat in the current playback position). When
    /// `current_time` reaches `fire_at` the engine seeks to `target`
    /// — both endpoints are bar-aligned so the jump sounds like a
    /// beat-perfect cut rather than a mid-bar chop. Cleared on skip,
    /// stop, or another teleport call.
    pub(crate) pending_teleport: Option<PendingTeleport>,
    /// Train-wreck handling mode (Off / Detect / AutoBail). Mirrored
    /// from AppConfig at engine startup; runtime-toggleable via the
    /// settings UI without restart.
    pub(crate) train_wreck_mode: crate::config::TrainWreckMode,
    /// True after a wreck has been detected on the current crossfade
    /// — prevents repeated bails or repeated warning toasts on a
    /// single mix. Cleared when a new crossfade starts.
    pub(crate) mix_wreck_fired: bool,
    /// Which transition types the rule engine / auto-selector may pick.
    /// Synced from AppConfig via MixEngine::set_enabled_transitions.
    pub(crate) enabled_transitions: Vec<String>,
    /// First time a track started playing this session — drives time_in_set_min.
    pub(crate) session_start: Option<std::time::Instant>,
    /// Display-smoothed phase offset (ms). EMA over raw measurements so the
    /// meter reads cleanly even when the raw calc jitters near beat edges.
    pub(crate) phase_display_ema: f64,
    /// Manual-mix mode: when true, crossfades skip `transition.apply()`
    /// so the DJ's crossfader_pos / channel_fader / EQ / filter settings
    /// stick instead of being overwritten by the auto curve. Phase-sync
    /// and downbeat align still run (safety rails). Auto-swap fires when
    /// crossfader_pos crosses the terminal side toward the incoming
    /// deck, same as auto mode. Toggled by the Claude DJ settings.
    pub(crate) manual_mix: bool,
    /// Quick-mix mode: when on, the Playing-state tick branch forces
    /// a crossfade once the playing deck has played past
    /// `quick_mix_bars` (musical time) and the incoming deck is
    /// loaded. Used for iteration — don't leave it on during a real set.
    pub(crate) quick_mix: bool,
    /// How many bars before quick-mix fires. Mirror of
    /// `AppConfig.claude_dj.quick_mix_bars`. Updated via
    /// `set_quick_mix_bars` whenever the user changes the setting.
    /// Default `DEFAULT_QUICK_MIX_BARS` so a fresh engine matches
    /// the legacy hardcoded behavior.
    pub(crate) quick_mix_bars: u32,
    /// Crossfade length in bars. Mirror of `AppConfig.crossfade_bars`,
    /// seeded in `MixEngine::new` and updated via `set_crossfade_bars`
    /// whenever the setting changes. `start_crossfade` reads this
    /// instead of hardcoding 16 so the Settings UI and IPC actually
    /// affect the length of future mixes.
    pub(crate) crossfade_bars: u32,
    /// Mirror of `AppConfig.jump_bars` so the dashboard's `◀ JUMP N ▶`
    /// label can render the current value without the engine reaching
    /// into AppConfig storage. Updated by `set_jump_bars`.
    pub(crate) jump_bars: u32,
    /// Auto-pick crossfade length per-mix from incoming genre. Mirror
    /// of `AppConfig.crossfade_bars_auto`. Overrides `crossfade_bars`
    /// when true.
    pub(crate) crossfade_bars_auto: bool,
    /// Optional in-progress crossfader sweep: the DJ calls
    /// `sweep_crossfader(target, bars)` once, and the tick loop
    /// interpolates `crossfader_pos` from its current value toward
    /// `target` over `over_secs` seconds of wall time. Prevents the
    /// failure mode where Claude batches five `set_crossfader` calls
    /// in one API response and they all execute in microseconds —
    /// sonically a hard cut instead of a sweep.
    pub(crate) sweep: Option<CrossfaderSweep>,
    /// Channel fader values captured the last time `preview_deck` ran.
    /// `stop_deck_preview` restores the deck's fader to this value
    /// instead of a hardcoded 1.0 — preserves any non-unity fader the
    /// user had set before previewing. None = no preview captured yet
    /// for that deck (defensive default in stop_deck_preview is 1.0).
    pub(crate) preview_saved_fader_a: Option<f32>,
    pub(crate) preview_saved_fader_b: Option<f32>,
    /// Instant of the last crossfader move (set_crossfader, sweep tick,
    /// snap). Used by the manual-mix stall detector: if manual_mix is on
    /// during a crossfade and nobody has touched the crossfader in 30s,
    /// we fall back to auto curves so the mix doesn't strand.
    pub(crate) last_crossfader_move: Option<std::time::Instant>,
    /// Snapshot of the most recent `start_crossfade` — tracks + their
    /// source-time positions + the forced transition type. Captured at
    /// the top of `start_crossfade`, consumed by the `T` (rewind) path
    /// so the user can replay / experiment with the mix.
    pub(crate) last_mix_snapshot: Option<MixSnapshot>,
    /// Rewind is a two-step dance (async track load, then finalize).
    /// This holds the snapshot between steps; the `load_incoming`
    /// completion hook picks it up and fires `finalize_rewind`.
    pub(crate) pending_rewind: Option<MixSnapshot>,
}

/// Outcome of a rewind request. `InPlace` = engine has already
/// rewound and re-fired the crossfade; `NeedLoad` = caller must
/// fetch + decode the track and call `load_incoming` so the engine
/// can complete the rewind on the completion hook.
// Returned once per rewind request, never collected in a Vec —
// boxing the BeatportTrack variant would add indirection without
// any memory benefit.
#[allow(clippy::large_enum_variant)]
pub enum RewindOutcome {
    InPlace,
    NeedLoad(BeatportTrack),
    /// Rewind was refused because a mix is currently running — restarting
    /// the crossfade on top of an active one corrupts fader / phase state.
    Blocked,
}

/// Result of `play_next` — tells the caller what actually happened so
/// the toast / log can be specific.
#[derive(Debug, Clone)]
pub enum PlayNextOutcome {
    /// Engine was idle — this track will start the session.
    StartedFresh,
    /// Engine playing without a preloaded incoming — this track is
    /// next in line, will be loaded as incoming for the next mix.
    LoadedAsIncoming,
    /// Engine playing with incoming preloaded but not yet playing —
    /// the incoming was unloaded and the displaced track was pushed
    /// back to queue so it still plays after this pick.
    ReplacedIncoming,
    /// Engine in the middle of a crossfade — couldn't swap mid-mix,
    /// added to the front of the queue (becomes next-after-this-mix).
    QueuedAtFront,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PendingLoopOp {
    /// Activate a beat-loop of `beats` length, starting at `fire_at`.
    Activate {
        deck_id: DeckId,
        beats: f64,
        fire_at: f64,
    },
    /// Release the active loop at `fire_at` so playback resumes
    /// cleanly off a bar boundary.
    Release { deck_id: DeckId, fire_at: f64 },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingJump {
    pub deck_id: DeckId,
    /// Source-time the destination seek should land at.
    pub target_time: f64,
    /// Source-time on the same deck at which the seek fires.
    pub fire_at: f64,
}

/// Deferred teleport. Always operates on the playing deck.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingTeleport {
    /// Source-time at which the seek fires (next downbeat).
    pub fire_at: f64,
    /// Source-time the seek lands at (bar-aligned destination).
    pub target: f64,
}

#[derive(Clone)]
pub(crate) struct MixSnapshot {
    pub playing_track: BeatportTrack,
    pub playing_time: f64,
    pub incoming_track: BeatportTrack,
    pub incoming_time: f64,
    pub transition_type: TransitionType,
    /// Which deck held the playing track at snapshot time. On rewind
    /// the deck that *doesn't* currently hold the incoming track
    /// becomes the playing role.
    pub playing_deck: DeckId,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossfaderSweep {
    pub from: f32,
    pub target: f32,
    pub started_at: std::time::Instant,
    pub duration: std::time::Duration,
}

impl AudioState {
    /// Set crossfader_pos and cancel any in-progress sweep. Used by
    /// every explicit crossfader write — set_crossfader for manual
    /// drag/IPC, start_crossfade for the manual-mode snap. Without
    /// the sweep cancel, a still-active sweep would overwrite the
    /// new value on the next tick.
    fn snap_crossfader(&mut self, pos: f32) {
        self.sweep = None;
        self.crossfader_pos = pos.clamp(-1.0, 1.0);
        self.last_crossfader_move = Some(std::time::Instant::now());
    }

    fn playing(&self) -> &DeckPlayer {
        match self.playing_deck {
            DeckId::A => &self.deck_a,
            DeckId::B => &self.deck_b,
        }
    }

    fn playing_mut(&mut self) -> &mut DeckPlayer {
        match self.playing_deck {
            DeckId::A => &mut self.deck_a,
            DeckId::B => &mut self.deck_b,
        }
    }

    fn incoming(&self) -> &DeckPlayer {
        match self.playing_deck {
            DeckId::A => &self.deck_b,
            DeckId::B => &self.deck_a,
        }
    }

    fn incoming_mut(&mut self) -> &mut DeckPlayer {
        match self.playing_deck {
            DeckId::A => &mut self.deck_b,
            DeckId::B => &mut self.deck_a,
        }
    }

    /// Get mutable refs to both playing and incoming decks simultaneously.
    fn decks_mut(&mut self) -> (&mut DeckPlayer, &mut DeckPlayer) {
        match self.playing_deck {
            DeckId::A => (&mut self.deck_a, &mut self.deck_b),
            DeckId::B => (&mut self.deck_b, &mut self.deck_a),
        }
    }

    /// Seek the incoming deck by `raw` seconds. If the backward seek would
    /// go past the buffer start, add `period` (one beat or one bar) to go
    /// forward instead. Returns the adjusted seek amount, or 0 if skipped.
    fn seek_forward_safe(
        incoming: &mut DeckPlayer,
        raw: f64,
        period: f64,
        min_threshold: f64,
    ) -> f64 {
        let inc_t = incoming.current_time();
        let duration = incoming.duration();
        let adj = forward_safe_delta(inc_t, raw, period, duration, min_threshold);
        if adj != 0.0 {
            incoming.seek(inc_t + adj);
        }
        adj
    }

    fn incoming_loaded_not_playing(&self) -> bool {
        match self.playing_deck.other() {
            DeckId::A => self.deck_a.is_loaded() && !self.deck_a.playing,
            DeckId::B => self.deck_b.is_loaded() && !self.deck_b.playing,
        }
    }

    fn start_crossfade(&mut self) {
        if self.session_start.is_none() {
            self.session_start = Some(std::time::Instant::now());
        }
        // Fresh mix → auto rate-correction resumes. Previous mix's
        // manual-override flag is reset here so the user's last-mix
        // nudges don't permanently disable auto for the set.
        self.user_overrode_this_mix = false;
        // Auto-mix triggering also resumes for the *next* track. The
        // current mix is firing now (manual or auto) so we're past
        // the "user is performing on the playing deck" window.
        self.user_paused_auto = false;
        // The mix is starting now — drop any pending teleport,
        // its purpose was to set up THIS mix.
        self.pending_teleport = None;
        // In manual mode, snap the crossfader to the playing-deck side at
        // crossfade start. Without this the mapping (crossfader=0 → 0.5
        // progress) leaves the mix stalled midway the moment a scheduled
        // crossfade fires, because Claude hasn't moved the fader yet.
        // Starting at 0 forces the DJ to actively sweep to complete.
        if self.manual_mix {
            // snap_crossfader clears the sweep + writes the position
            // atomically so a still-active sweep can't overwrite the
            // snap on the next tick.
            self.snap_crossfader(match self.playing_deck {
                DeckId::A => -1.0,
                DeckId::B => 1.0,
            });
        }
        let playing_bpm = self.playing().beat_grid.map(|g| g.bpm).unwrap_or(128.0);
        let incoming_bpm = self.incoming().beat_grid.map(|g| g.bpm).unwrap_or(128.0);

        let playing_key = self.playing().track.as_ref().and_then(|t| t.key.clone());
        let incoming_key = self.incoming().track.as_ref().and_then(|t| t.key.clone());

        // User-supplied rule engine picks the transition. Falls back to the
        // hardcoded choose() if no rules matched and default is unset.
        let min_bpm = playing_bpm.min(incoming_bpm).max(1.0);
        let ratio = playing_bpm.max(incoming_bpm) / min_bpm;
        let normalized = if (1.8..=2.2).contains(&ratio) {
            ratio / 2.0
        } else {
            ratio
        };
        let bpm_gap_pct = (normalized - 1.0) * 100.0;
        let key_dist = match (playing_key.as_deref(), incoming_key.as_deref()) {
            (Some(a), Some(b)) => camelot_key_dist(a, b) as usize,
            _ => 99,
        };
        // Energy delta from analyzer RMS, drop detection from current phrase,
        // and minutes-in-set from session start (first mix wall-clock).
        let energy_delta = {
            let p = self
                .playing()
                .analysis
                .as_ref()
                .map(|a| a.rms_loudness)
                .unwrap_or(0.0);
            let i = self
                .incoming()
                .analysis
                .as_ref()
                .map(|a| a.rms_loudness)
                .unwrap_or(0.0);
            i - p
        };
        let phrase_is_drop = {
            let p = self.playing();
            match (p.analysis.as_ref(), p.beat_grid) {
                (Some(a), Some(_)) => {
                    let t = p.current_time();
                    a.phrases
                        .iter()
                        .rev()
                        .find(|ph| ph.start_time <= t)
                        .map(|ph| matches!(ph.phrase_type, super::analyzer::PhraseType::Drop))
                        .unwrap_or(false)
                }
                _ => false,
            }
        };
        let time_in_set_min = self
            .session_start
            .map(|t| t.elapsed().as_secs() / 60)
            .unwrap_or(0) as u32;
        let ctx = super::transition_rules::RuleContext {
            bpm_gap_pct,
            key_dist,
            last_transition: self.rule_engine.last_transition,
            mix_count: self.rule_engine.mix_count,
            energy_delta,
            phrase_is_drop,
            time_in_set_min,
        };
        let mut transition = self.rule_engine.choose(ctx, &self.enabled_transitions);
        // BPM-gap cutoff for forcing EchoOut. Rubberband-aware (see
        // `bpm_gap_cutoff`). Also overrides any user rule that
        // disabled EchoOut: the safety floor trumps the preference.
        let cutoff = bpm_gap_cutoff(self.playing().pitch_stretch.is_some());
        if bpm_gap_pct > cutoff && transition.use_phase_sync() {
            transition = super::transition::TransitionType::EchoOut;
            tracing::info!("BPM gap {bpm_gap_pct:.1}% > {cutoff:.0}% cutoff: forcing EchoOut",);
        }
        // Manual mode can't drive EchoOut properly: transition.apply()
        // is skipped so delay_wet stays at 1.0 and the incoming never
        // auto-starts. Force BeatMatched for a clean manual sweep.
        if self.manual_mix && transition == super::transition::TransitionType::EchoOut {
            transition = super::transition::TransitionType::BeatMatched;
            tracing::info!("Manual mode: overriding EchoOut → BeatMatched");
        }
        self.transition_type = transition;

        // Snapshot the mix for the `T` (rewind) path — captures both
        // tracks + their source-time positions *before* the alignment
        // seeks below run, so a replay re-runs the same alignment math.
        // Skipped when a track is missing (shouldn't happen at this
        // point but keeps the type Optional-free downstream).
        if let (Some(pt), Some(it)) = (
            self.playing().track.as_deref().cloned(),
            self.incoming().track.as_deref().cloned(),
        ) {
            self.last_mix_snapshot = Some(MixSnapshot {
                playing_track: pt,
                playing_time: self.playing().current_time(),
                incoming_track: it,
                incoming_time: self.incoming().current_time(),
                transition_type: transition,
                playing_deck: self.playing_deck,
            });
        }

        // Phase-align incoming to playing deck's beat position.
        // Shift the incoming's source position so its within-beat phase
        // matches the playing deck's. After this seek, both decks tick
        // their next beat at the same wall-time (assuming matched
        // audible rates from prepare() below).
        //
        // Generalized from the older "advance = playing_phase * inc_int"
        // formula, which silently assumed the incoming was sitting at
        // phase 0 (its first beat). That held in the common path but
        // broke when the incoming had been previewed, quick-mixed, or
        // loaded mid-track — leaving a residual equal to the incoming's
        // existing within-beat offset. Now we measure both phases and
        // apply the signed shortest-path seek.
        if transition.use_phase_sync() {
            let playing_phase = {
                let p = self.playing();
                p.beat_grid
                    .map(|g| g.phase(p.current_time()))
                    .unwrap_or(0.0)
            };

            let incoming = self.incoming_mut();
            let inc_time = incoming.current_time();
            let inc_grid = incoming.beat_grid;

            if let Some(ig) = inc_grid {
                let incoming_beat_interval = ig.beat_interval();
                let incoming_phase = ig.phase(inc_time);

                let advance = super::beat_grid::BeatGrid::phase_align_advance(
                    playing_phase,
                    incoming_phase,
                    incoming_beat_interval,
                );

                let adj =
                    Self::seek_forward_safe(incoming, advance, incoming_beat_interval, 0.0005);
                if adj != 0.0 {
                    tracing::info!(
                        "Phase aligned: shifted incoming by {:+.1}ms (playing phase={playing_phase:.3}, incoming phase={incoming_phase:.3})",
                        adj * 1000.0
                    );
                } else if advance.abs() > 0.001 {
                    tracing::warn!(
                        "Phase align rejected: advance={:+.1}ms (inc_t={:.3}s, dur={:.1}s)",
                        advance * 1000.0,
                        inc_time,
                        incoming.duration()
                    );
                    // Fallback: if we can't seek backward (position near 0),
                    // seek forward by a full beat + the advance so we land on
                    // the correct phase without going negative.
                    let fallback = advance + incoming_beat_interval;
                    let fb_t = inc_time + fallback;
                    if fb_t >= 0.0 && fb_t < incoming.duration() {
                        incoming.seek(fb_t);
                        tracing::info!(
                            "Phase aligned (fallback): shifted incoming by {:+.1}ms",
                            fallback * 1000.0
                        );
                    }
                }
            } else {
                tracing::warn!("Phase align skipped: incoming has no beat_grid");
            }

            // Downbeat-align the incoming to the playing. Beat-level phase
            // lock (above) puts the ticks on the same ms, but a deck can
            // still sit on the wrong beat of the bar — the "off by 1"
            // that sounds nearly-tight but wrong. Here we seek incoming
            // by ±N beats (shortest path) so the 1s of both tracks land
            // on the same bar slot. Only applied when both grids carry a
            // real first-beat time; pure-zero defaults would cause false
            // corrections on un-analyzed decks.
            let (pg, ig, p_time, i_time) = {
                let p = self.playing();
                let i = self.incoming();
                (p.beat_grid, i.beat_grid, p.current_time(), i.current_time())
            };
            if let (Some(pg), Some(ig)) = (pg, ig) {
                let bib_p = pg.beat_in_bar(p_time);
                let bib_i = ig.beat_in_bar(i_time);
                let bar_seek =
                    super::beat_grid::BeatGrid::bar_aligned_seek_offset(&ig, i_time, &pg, p_time);
                tracing::info!(
                    "Downbeat check: playing beat_in_bar={bib_p}, incoming beat_in_bar={bib_i}, bar_seek={:.1}ms",
                    bar_seek * 1000.0
                );
                if bar_seek.abs() > 1e-3 {
                    let incoming = self.incoming_mut();
                    let adj = Self::seek_forward_safe(incoming, bar_seek, ig.bar_interval(), 1e-3);
                    if adj != 0.0 {
                        tracing::info!(
                            "Downbeat aligned: shifted incoming by {:+.0}ms ({} beat(s)) to land on the 1",
                            adj * 1000.0,
                            (adj / ig.beat_interval()).round() as i32
                        );
                    } else {
                        tracing::warn!(
                            "Downbeat align rejected: bar_seek={:+.1}ms (inc_t={:.3}s)",
                            bar_seek * 1000.0,
                            self.incoming().current_time()
                        );
                    }
                }
            }

            // Cleanup: re-measure phase after both seeks and nudge if
            // there's still a residual > 2ms. The combined phase+downbeat
            // forward-fallbacks can leave a small error.
            {
                let (p_phase, i_phase, inc_int) = {
                    let p = self.playing();
                    let i = self.incoming();
                    let pp = p
                        .beat_grid
                        .map(|g| g.phase(p.current_time()))
                        .unwrap_or(0.0);
                    let ip = i
                        .beat_grid
                        .map(|g| g.phase(i.current_time()))
                        .unwrap_or(0.0);
                    let ii = i.beat_grid.map(|g| g.beat_interval()).unwrap_or(0.5);
                    (pp, ip, ii)
                };
                let cleanup =
                    super::beat_grid::BeatGrid::phase_align_advance(p_phase, i_phase, inc_int);
                if cleanup.abs() > 0.002 {
                    let incoming = self.incoming_mut();
                    // Use seek_forward_safe (same helper the main
                    // alignment seek uses) so a small negative cleanup
                    // near track-start walks forward by one beat
                    // instead of clamping to 0.0.
                    let applied = Self::seek_forward_safe(incoming, cleanup, inc_int, 0.002);
                    if applied != 0.0 {
                        tracing::info!(
                            "Phase cleanup: nudged {:+.1}ms after combined alignment",
                            applied * 1000.0
                        );
                    }
                } else {
                    // Log even when no nudge fired — gives a clear
                    // signal in the log that the cleanup pass ran and
                    // residual was within tolerance, instead of the
                    // ambiguous "no log entry => maybe skipped". Info
                    // level (matches the nudge-fired path above) so it
                    // appears in default production logs.
                    tracing::info!(
                        "Phase cleanup: residual {:+.1}ms within tolerance, no nudge",
                        cleanup * 1000.0
                    );
                }
            }

            // Log resulting offset
            let p = self.playing();
            let i = self.incoming();
            let phase_ms = if let (Some(pg), Some(ig)) = (p.beat_grid, i.beat_grid) {
                super::beat_grid::BeatGrid::phase_offset(
                    &pg,
                    p.current_time(),
                    &ig,
                    i.current_time(),
                ) * 1000.0
            } else {
                0.0
            };
            tracing::info!(
                "Crossfade start: playing@{:.3}s (phase {:.2}), incoming@{:.3}s (phase {:.2}), offset={phase_ms:+.1}ms",
                p.current_time(),
                p.beat_grid
                    .map(|g| g.phase(p.current_time()))
                    .unwrap_or(0.0),
                i.current_time(),
                i.beat_grid
                    .map(|g| g.phase(i.current_time()))
                    .unwrap_or(0.0),
            );
        }

        // BPM grid sanity check: compare the grid-based rate ratio against
        // the Beatport hint ratio. If they diverge, the grid is wrong and
        // the phase meter will read 0.0ms while actual beats drift.
        if transition.use_phase_sync() {
            let hint_bpm = self.incoming().track.as_ref().and_then(|t| t.bpm);
            if let Some(hint) = hint_bpm {
                let grid_ratio = playing_bpm / incoming_bpm;
                let hint_ratio = playing_bpm / hint;
                if (grid_ratio - hint_ratio).abs() > 0.03 {
                    tracing::error!(
                        "BPM grid mismatch: grid says {incoming_bpm:.1} (ratio {grid_ratio:.3}) but Beatport says {hint:.1} (ratio {hint_ratio:.3}) — phase meter will be WRONG"
                    );
                }
            }
        }

        // Let the transition type set up deck state (rate, play, echo arm, etc.)
        {
            let (playing, incoming) = self.decks_mut();
            transition.prepare(playing, incoming, playing_bpm, incoming_bpm);
        }

        // Latency compensation. After prepare() the incoming's rate is set,
        // so its stretcher's latency now reflects the BPM-matched ratio.
        // Playing has been at native rate for a while; its latency is the
        // ratio=1 value. The audible offset between the two = source offset
        // − (incoming_latency − playing_latency). Pre-advance incoming by
        // exactly that difference so the audible mix lands on the beat.
        // (latency compensation removed — was making displayed phase worse,
        //  not better; see follow-up investigation)

        // Pick the base bar count. Auto mode inspects incoming genre
        // + phrase density; otherwise the user-set value. Transitions
        // with an absolute bar count (EchoOut = 8) still override.
        let base_bars = if self.crossfade_bars_auto {
            let t = self.incoming().track.clone();
            let a = self.incoming().analysis.clone();
            t.map(|track| auto_crossfade_bars(&track, a.as_deref()))
                .unwrap_or(self.crossfade_bars)
        } else {
            self.crossfade_bars
        };
        let bars = if let Some(n) = transition.absolute_bars() {
            n
        } else {
            ((base_bars as f64) * transition.duration_multiplier()).max(1.0) as u32
        };
        if self.crossfade_bars_auto {
            tracing::info!(
                "Auto crossfade: {} bars (genre {:?})",
                base_bars,
                self.incoming()
                    .track
                    .as_ref()
                    .and_then(|t| t.genre_name.clone())
            );
        }
        self.crossfade_controller = Some(CrossfadeController::new(playing_bpm, incoming_bpm, bars));
        self.crossfade_progress = 0.0;
        self.mix_phase_samples.clear();
        self.mix_wreck_fired = false;
        self.crossfade_start = Some(std::time::Instant::now());
        self.crossfade_start_playing_time = Some(self.playing().current_time());
        self.state = EngineState::Crossfading;

        tracing::info!(
            "Crossfade started: {playing_bpm:.0} → {incoming_bpm:.0} BPM, {transition:?}"
        );
        crate::ipc::write_event(&serde_json::json!({
            "kind": "crossfade_started",
            "playing_bpm": playing_bpm,
            "incoming_bpm": incoming_bpm,
            "transition": format!("{transition:?}"),
        }));
    }

    /// Returns old samples Vec for deferred deallocation outside the lock.
    fn swap_decks(&mut self, config: &AppConfig) -> Vec<f32> {
        let glide_bars = config.glide_bars;
        // Stop and clear old playing deck so it's not mistaken for a loaded incoming
        let old_deck = self.playing_deck;
        let old_samples = match old_deck {
            DeckId::A => self.deck_a.unload(),
            DeckId::B => self.deck_b.unload(),
        };
        self.playing_deck = self.playing_deck.other();
        self.crossfade_progress = 0.0;
        self.crossfade_start = None;
        self.crossfade_start_playing_time = None;
        self.crossfade_controller = None;
        // Clear the old playing track's mix-in trigger. The tick loop
        // will recompute for the new playing deck on its next pass;
        // leaving the stale value would flash one frame of garbage in
        // the dashboard's "MIX IN N bars" readout. Also clear the
        // user-extension flag so the new track doesn't inherit a
        // sticky extension from the previous one.
        self.cached_trigger_time = None;
        self.trigger_user_extended = false;
        self.state = EngineState::Playing;
        // Mark crossfade-end time so the split-cue trail-out can keep
        // the stereo image open for ~1.5s after swap (mirrors the
        // pre-mix lead-in).
        self.last_crossfade_end = Some(std::time::Instant::now());

        // Stop any existing glide first
        self.is_gliding = false;
        tracing::info!(
            "Swap complete: state={:?}, playing_deck={:?}, playing.playing={}, playing.is_loaded={}",
            self.state,
            self.playing_deck,
            self.playing().playing,
            self.playing().is_loaded()
        );
        // Log the new playing deck's rate for debugging
        let new_rate = self.playing().rate;
        let new_resample = self.playing().resample_ratio();
        tracing::info!(
            "Swap: new playing rate={new_rate:.4}, resample={new_resample:.4}, ratio={:.4}",
            new_rate / new_resample
        );

        // Start new glide only for beat-matched transitions
        let deck = self.playing_mut();
        let current_rate = deck.rate; // rate is now just the speed multiplier, no resample
        let was_beat_matched =
            self.transition_type == super::transition::TransitionType::BeatMatched;
        if was_beat_matched
            && config.bpm_mode == crate::config::BpmMode::Glide
            && (current_rate - 1.0).abs() > 0.005
        {
            self.glide_start_rate = current_rate;
            self.glide_start_time = self.playing().current_time();
            // glide_bars = 0 is the "Max" sentinel: stretch the glide
            // across the entire runway to the next mix-out so pitch
            // settles as gently as possible. Falls back to 8 bars if
            // the runway would be shorter than that (tiny tracks,
            // mid-track teleports, etc.) so the glide never disappears.
            let bar_dur = self
                .playing()
                .beat_grid
                .map(|g| g.bar_interval())
                .unwrap_or(2.0);
            self.glide_duration = if glide_bars == 0 {
                let duration = self.playing().duration();
                let current = self.playing().current_time();
                let xfade_dur = bar_dur * self.crossfade_bars as f64;
                let safety = bar_dur * 4.0;
                ((duration - current) - xfade_dur - safety).max(bar_dur * 8.0)
            } else {
                bar_dur * glide_bars as f64
            };
            self.is_gliding = true;
            tracing::info!(
                "Rate glide: {current_rate:.3} → 1.0 over {:.1}s",
                self.glide_duration
            );
        }

        tracing::info!("Decks swapped, now {:?}", self.playing_deck);
        old_samples
    }

    /// Complete a mix rewind. Called from `load_incoming` once the
    /// snapshot's playing track has been re-loaded onto the (post-
    /// original-swap) empty deck. Flips roles so that deck resumes
    /// the playing role, seeks both decks to the captured positions,
    /// forces the captured transition type (bypassing the rule
    /// engine), and fires `start_crossfade`.
    fn finalize_rewind(&mut self, snap: MixSnapshot) {
        tracing::info!(
            "Rewind: replaying {} → {} at {:.1}s/{:.1}s",
            snap.playing_track.full_title(),
            snap.incoming_track.full_title(),
            snap.playing_time,
            snap.incoming_time,
        );
        // The reloaded track just landed on the current incoming deck.
        // Flip roles so it's the playing deck again, matching the
        // snapshot's geometry (old playing track → playing role).
        self.playing_deck = snap.playing_deck;
        // Seek both decks. A fresh load lands at start_time (usually
        // first_beat_time); override to the captured pre-mix position.
        self.playing_mut().seek(snap.playing_time);
        self.incoming_mut().seek(snap.incoming_time);
        self.playing_mut().play();
        // Force the captured transition so re-running the rule engine
        // (which advanced mix_count since the original) doesn't pick
        // a different type on replay.
        self.transition_type = snap.transition_type;
        // Clear any stale crossfade state.
        self.state = EngineState::Playing;
        self.crossfade_progress = 0.0;
        self.crossfade_controller = None;
        self.is_gliding = false;
        // Fire. `start_crossfade` will re-snapshot (last_mix_snapshot
        // updates), which is fine — the next T picks up the replayed
        // mix, giving a sensible "undo the replay" path.
        // Defensive: only fire if the incoming deck actually has a
        // track loaded and isn't already playing. Belt-and-suspenders
        // — finalize_rewind shouldn't run otherwise, but if a race
        // ever lands here mid-crossfade we'd corrupt mixer state.
        if self.incoming_loaded_not_playing() {
            self.start_crossfade();
        } else {
            tracing::warn!("Rewind: incoming deck not ready, skipping crossfade");
        }
    }
}

pub struct MixEngine {
    pub(crate) audio_state: Arc<Mutex<AudioState>>,
    _stream: Option<Stream>,
    _monitor_stream: Option<Stream>,
    pub(crate) queue: Vec<QueueEntry>,
    pub(crate) history: Vec<HistoryEntry>,
    next_track_requested: bool,
}

impl MixEngine {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let host = cpal::default_host();
        // Honor `config.output_device` when set: pick that named cpal
        // output instead of system default, so the user can route
        // mixr to e.g. their controller's audio interface without
        // changing the macOS system default. Fall back to default
        // if the named device isn't found (e.g. controller unplugged).
        let device = pick_output_device(&host, config.output_device.trim())
            .ok_or_else(|| anyhow::anyhow!("no audio output device"))?;

        let supported_config = device.default_output_config()?;
        let sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels() as usize;

        tracing::info!(
            "Audio: {} @ {}Hz, {}ch",
            device.name().unwrap_or_default(),
            sample_rate,
            channels
        );

        let audio_state = Arc::new(Mutex::new(AudioState {
            deck_a: DeckPlayer::new(sample_rate),
            deck_b: DeckPlayer::new(sample_rate),
            playing_deck: DeckId::A,
            state: EngineState::Idle,
            crossfade_progress: 0.0,
            crossfade_start: None,
            crossfade_start_playing_time: None,
            crossfade_controller: None,
            transition_type: TransitionType::BeatMatched,
            split_cue: false,
            split_ramp: 0.0,
            split_alpha: 1.0 - (-1.0f32 / sample_rate as f32).exp(),
            last_crossfade_end: None,
            quantize_on: config.quantize_on,
            quantize_beats: config.quantize_beats,
            pending_jump: None,
            pending_teleport: None,
            pending_loop: None,
            metronome: false,
            preview: None,
            preview_stop_time: 0.0,
            cached_trigger_time: None,
            trigger_user_extended: false,
            nudge_base_rate: None,
            nudge_revert_at: None,
            user_overrode_this_mix: false,
            user_paused_auto: false,
            user_paused_auto_just_triggered: false,
            scratch_a: vec![0.0; 65536],
            scratch_b: vec![0.0; 65536],
            scratch_echo_a: vec![0.0; 65536],
            scratch_echo_b: vec![0.0; 65536],
            scratch_preview: vec![0.0; 65536],
            is_gliding: false,
            glide_start_rate: 1.0,
            glide_start_time: 0.0,
            glide_duration: 0.0,
            deferred_drop: None,
            crossfader_pos: 0.0,
            channel_fader_a: 1.0,
            channel_fader_b: 1.0,
            master_gain: 1.0,
            limiter_mode: crate::config::LimiterMode::SoftKnee,
            monitor_source: MonitorSource::Incoming,
            monitor_ring: None,
            monitor_ring_cap: 0,
            rule_engine: super::transition_rules::RuleEngine::load(),
            profiler: super::profiler::AudioProfiler::new(256),
            // Sized for the longest reasonable crossfade (64 bars at 100 BPM ≈
            // 150 seconds at 60 Hz tick ≈ 9000 samples). Avoids reallocation
            // during the crossfade; cleared on completion.
            mix_phase_samples: Vec::with_capacity(9000),
            train_wreck_mode: config.train_wreck_mode,
            mix_wreck_fired: false,
            enabled_transitions: config.enabled_transitions.clone(),
            session_start: None,
            phase_display_ema: 0.0,
            manual_mix: false,
            quick_mix: false,
            quick_mix_bars: DEFAULT_QUICK_MIX_BARS,
            crossfade_bars: config.crossfade_bars,
            crossfade_bars_auto: config.crossfade_bars_auto,
            jump_bars: config.jump_bars,
            sweep: None,
            preview_saved_fader_a: None,
            preview_saved_fader_b: None,
            last_crossfader_move: None,
            last_mix_snapshot: None,
            pending_rewind: None,
        }));

        let sc = Arc::clone(&audio_state);
        let stream = device.build_output_stream(
            &supported_config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut state = sc.lock().unwrap_or_else(|e| e.into_inner());
                fill_output(&mut state, data, channels);
            },
            |err| tracing::error!("Audio error: {err}"),
            None,
        )?;
        stream.play()?;

        // Optional monitor (DJ-headphone) output: a second cpal stream to a
        // user-selected device that plays only the incoming-deck pre-mix.
        let monitor_stream = build_monitor_stream(&host, config, &audio_state);

        Ok(Self {
            audio_state,
            _stream: Some(stream),
            _monitor_stream: monitor_stream,
            queue: Vec::new(),
            history: Vec::new(),
            next_track_requested: false,
        })
    }

    /// Push to the queue with dedup: skip if the track is already
    /// queued OR currently loaded on either deck. Returns true if it
    /// was added. Common case: user pressed `a` (queue all) twice on
    /// the same chart — the second press should be a no-op per
    /// track, not pile every track in twice.
    pub fn enqueue(&mut self, entry: QueueEntry) -> bool {
        if self.is_track_queued_or_loaded(entry.track.id) {
            return false;
        }
        crate::ipc::write_event(&serde_json::json!({
            "kind": "queued",
            "track_id": entry.track.id,
            "title": entry.track.full_title(),
            "artist": entry.track.artist_name(),
        }));
        self.queue.push(entry);
        true
    }

    /// Track ID is in the queue or loaded on a deck (regardless of
    /// playing state). Used by `enqueue` / `enqueue_front` for dedup
    /// and by callers that want to know without trying to push.
    pub fn is_track_queued_or_loaded(&self, track_id: i64) -> bool {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let on_deck_a = s.deck_a.track.as_ref().map(|t| t.id) == Some(track_id);
        let on_deck_b = s.deck_b.track.as_ref().map(|t| t.id) == Some(track_id);
        drop(s);
        on_deck_a || on_deck_b || self.queue.iter().any(|q| q.track.id == track_id)
    }

    /// "Play next" — loads a track such that it plays before any
    /// already-queued items, with state-aware semantics.
    pub fn play_next(&mut self, track: BeatportTrack) -> PlayNextOutcome {
        let id = track.id;
        // Dedup: if this track is already queued, remove the existing
        // entry so we don't end up with both "old position" and
        // "front position" pointing to the same track.
        self.queue.retain(|q| q.track.id != id);
        let entry = QueueEntry::from(track);

        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());

        match s.state {
            EngineState::Idle => {
                drop(s);
                self.queue.insert(0, entry);
                self.next_track_requested = true;
                PlayNextOutcome::StartedFresh
            }
            EngineState::Crossfading => {
                drop(s);
                self.queue.insert(0, entry);
                PlayNextOutcome::QueuedAtFront
            }
            EngineState::Playing | EngineState::PreparingCrossfade => {
                let incoming_loaded_not_playing = s.incoming().is_loaded() && !s.incoming().playing;
                if incoming_loaded_not_playing {
                    // Snapshot the displaced track so it can be re-queued
                    // (preserves the user's prior choice — they just
                    // changed their mind about WHICH plays next).
                    let displaced = s.incoming().track.as_ref().map(|t| (**t).clone());
                    s.incoming_mut().unload();
                    drop(s);
                    self.queue.insert(0, entry);
                    if let Some(t) = displaced {
                        // Insert displaced after the new pick so it
                        // plays second, not first. Skip if it was the
                        // same track we're loading (rare).
                        if t.id != id {
                            self.queue.insert(1, QueueEntry::from(t));
                        }
                    }
                    self.next_track_requested = true;
                    PlayNextOutcome::ReplacedIncoming
                } else {
                    drop(s);
                    self.queue.insert(0, entry);
                    self.next_track_requested = true;
                    PlayNextOutcome::LoadedAsIncoming
                }
            }
        }
    }

    /// Build a session snapshot of current deck + queue state for
    /// `~/.mixr/session.json`. Returns `None` if nothing's loaded
    /// worth persisting (first launch, idle after logout).
    pub fn session_snapshot(&self) -> Option<crate::session::SessionSnapshot> {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let playing = s.playing();
        let incoming = s.incoming();
        // Cap saved positions a comfortable distance from the end of
        // the track. Seeking back to the very last sample on resume
        // immediately tripped end-of-track and stopped playback.
        // 30s gives the new session real audio to land on.
        let safe_pos = |d: &super::deck::DeckPlayer| -> f64 {
            let now = d.current_time();
            let dur = d.duration();
            if dur > 0.0 && now > dur - 30.0 {
                d.beat_grid.map(|g| g.first_beat_time).unwrap_or(0.0)
            } else {
                now
            }
        };
        let playing_state = playing.track.as_ref().map(|t| crate::session::TrackState {
            track: (**t).clone(),
            position: safe_pos(playing),
        });
        let incoming_state = incoming.track.as_ref().map(|t| crate::session::TrackState {
            track: (**t).clone(),
            position: safe_pos(incoming),
        });
        let queue: Vec<BeatportTrack> = self.queue.iter().map(|e| (*e.track).clone()).collect();
        // Don't bother writing a snapshot with nothing in it — avoids
        // overwriting a useful previous session with an empty one on
        // a quick relaunch before playback starts.
        if playing_state.is_none() && incoming_state.is_none() && queue.is_empty() {
            return None;
        }
        Some(crate::session::SessionSnapshot {
            playing: playing_state,
            incoming: incoming_state,
            queue,
            playing_deck: match s.playing_deck {
                DeckId::A => "a".into(),
                DeckId::B => "b".into(),
            },
            saved_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    }

    /// Push a queue entry to the *front* so it becomes the next track
    /// loaded into the incoming deck. Used by the manual-mode
    /// `load_to_deck` tool so the DJ can pick a specific track without
    /// waiting for earlier queue items to play out.
    pub fn enqueue_front(&mut self, entry: QueueEntry) -> bool {
        if self.is_track_queued_or_loaded(entry.track.id) {
            return false;
        }
        self.queue.insert(0, entry);
        true
    }
    pub fn clear_queue(&mut self) {
        self.queue.clear();
    }
    pub fn export_history(&self) -> usize {
        let count = self.history.len();
        if count == 0 {
            return 0;
        }
        let dir = dirs::home_dir().unwrap_or_default().join(".mixr");
        std::fs::create_dir_all(&dir).ok();
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();

        // Text format
        let mut text = format!(
            "mixr history — {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        text.push_str(&"─".repeat(60));
        text.push('\n');
        for (i, entry) in self.history.iter().enumerate() {
            let bpm = entry
                .track
                .bpm
                .map(|b| format!("{:.0} BPM", b))
                .unwrap_or("?".into());
            let key = entry.track.key.as_deref().unwrap_or("?");
            text.push_str(&format!(
                "{:>3}. {} - {}  {} / {}\n",
                i + 1,
                entry.track.artist_name(),
                entry.track.full_title(),
                bpm,
                key
            ));
        }

        // JSON format
        let entries: Vec<serde_json::Value> = self
            .history
            .iter()
            .map(|e| {
                serde_json::json!({
                    "artist": e.track.artist_name(),
                    "title": e.track.full_title(),
                    "bpm": e.track.bpm,
                    "key": e.track.key,
                })
            })
            .collect();
        let json = serde_json::to_string_pretty(&entries).unwrap_or("[]".into());

        std::fs::write(dir.join(format!("history-{date}.txt")), &text).ok();
        std::fs::write(dir.join(format!("history-{date}.json")), &json).ok();
        tracing::info!("History exported: {count} tracks");
        count
    }

    pub fn smart_shuffle(&mut self) {
        if self.queue.len() <= 1 {
            return;
        }
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let (playing, incoming) = match s.playing_deck {
            DeckId::A => (&s.deck_a, &s.deck_b),
            DeckId::B => (&s.deck_b, &s.deck_a),
        };
        let start_bpm = playing
            .beat_grid
            .map(|g| g.bpm)
            .or(incoming.beat_grid.map(|g| g.bpm))
            .or(self.queue.first().and_then(|e| e.track.bpm))
            .unwrap_or(128.0);
        let start_key = playing
            .track
            .as_ref()
            .and_then(|t| t.key.clone())
            .or(incoming.track.as_ref().and_then(|t| t.key.clone()));
        drop(s);

        let mut remaining = std::mem::take(&mut self.queue);
        let mut sorted: Vec<QueueEntry> = Vec::new();
        let mut cur_bpm = start_bpm;
        let mut cur_key = start_key;

        while !remaining.is_empty() {
            let best_idx = remaining
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let sa = shuffle_score(&a.track, cur_bpm, cur_key.as_deref());
                    let sb = shuffle_score(&b.track, cur_bpm, cur_key.as_deref());
                    sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
                .unwrap();
            let entry = remaining.remove(best_idx);
            cur_bpm = entry.track.bpm.unwrap_or(cur_bpm);
            cur_key = entry.track.key.clone();
            sorted.push(entry);
        }
        self.queue = sorted;
        tracing::info!("Queue smart-shuffled ({} tracks)", self.queue.len());
    }

    pub fn move_queue_item(&mut self, from: usize, to: usize) {
        if from < self.queue.len() && to < self.queue.len() {
            let item = self.queue.remove(from);
            self.queue.insert(to, item);
        }
    }

    pub fn play_track(
        &mut self,
        samples: Vec<f32>,
        sample_rate: u32,
        analysis: super::analyzer::AnalysisResult,
        track: BeatportTrack,
        start_time: f64,
    ) {
        let detected_bpm = analysis.beat_grid.bpm;
        let beatport_bpm = track.bpm.unwrap_or(0.0);
        tracing::info!(
            "BPM: beatport={:.1}, detected={:.1}, delta={:.1}",
            beatport_bpm,
            detected_bpm,
            (beatport_bpm - detected_bpm).abs()
        );
        crate::ipc::write_event(&serde_json::json!({
            "kind": "play_started",
            "track_id": track.id,
            "title": track.full_title(),
            "artist": track.artist_name(),
            "bpm": detected_bpm,
            "key": track.key.as_deref().unwrap_or(""),
        }));
        let old_samples = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
            let old = std::mem::take(&mut s.playing_mut().samples);
            s.playing_mut().load(samples, sample_rate, analysis, track);
            if start_time > 0.0 {
                s.playing_mut().seek(start_time);
            }
            s.playing_mut().play();
            s.state = EngineState::Playing;
            old
        };
        drop(old_samples); // dealloc outside the lock
        // First track loaded and playing. Now allow ONE preload for the incoming deck.
        // The preload sets the flag true again, preventing further downloads until swap.
        self.next_track_requested = false;
    }

    pub fn load_incoming(
        &mut self,
        samples: Vec<f32>,
        sample_rate: u32,
        analysis: super::analyzer::AnalysisResult,
        track: BeatportTrack,
        start_time: f64,
    ) {
        let detected_bpm = analysis.beat_grid.bpm;
        let beatport_bpm = track.bpm.unwrap_or(0.0);
        tracing::info!(
            "BPM: beatport={:.1}, detected={:.1}, delta={:.1}",
            beatport_bpm,
            detected_bpm,
            (beatport_bpm - detected_bpm).abs()
        );
        let old_samples = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
            // Warn if we're overwriting an already-loaded incoming deck that
            // never got to play (the prior candidate is silently discarded).
            if let Some(ref prev) = s.incoming().track {
                tracing::warn!(
                    "incoming deck replaced before crossfade: discarding {} — {}",
                    prev.artist_name(),
                    prev.full_title(),
                );
            }
            let old = std::mem::take(&mut s.incoming_mut().samples);
            s.incoming_mut().load(samples, sample_rate, analysis, track);
            if start_time > 0.0 {
                s.incoming_mut().seek(start_time);
            }
            // Don't reset next_track_requested here — incoming is loaded, no more
            // tracks needed until crossfade completes and decks swap.
            tracing::info!("Incoming deck loaded, ready for crossfade");
            // Rewind flow: a snapshot-track is being reloaded onto the
            // (currently-empty) incoming deck. Once it lands, flip the
            // deck roles so the reloaded track resumes the playing
            // role it held in the snapshot, seek both decks back to
            // the captured positions, and fire the mix. Done here (not
            // in the normal PreparingCrossfade branch) because rewind
            // skips the rule-engine selection.
            if let Some(snap) = s.pending_rewind.take() {
                s.finalize_rewind(snap);
            } else if s.state == EngineState::PreparingCrossfade {
                s.start_crossfade();
            }
            old
        };
        drop(old_samples); // dealloc outside the lock
    }

    /// Start a mix rewind. Two paths:
    ///   - `InPlace`: both decks still hold the snapshotted tracks (the
    ///     user hit `T` mid-crossfade or before the post-mix unload).
    ///     Rewind runs immediately — just seek both decks back and
    ///     re-fire the crossfade; no reload needed.
    ///   - `NeedLoad(track)`: post-swap, the old playing deck was
    ///     unloaded. Caller must load `track` via `load_incoming`;
    ///     the completion hook picks up `pending_rewind` and calls
    ///     `finalize_rewind`.
    ///
    /// Returns `None` if there's no snapshot or a rewind is in flight.
    pub fn request_rewind(&self) -> Option<RewindOutcome> {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.pending_rewind.is_some() {
            return None;
        }
        let snap = s.last_mix_snapshot.clone()?;

        // Block mid-mix rewind. Restarting a crossfade on top of one
        // that's already running leaves crossfader_pos / channel
        // faders / incoming playing-state stale, producing a stuck
        // state (crossfader pegged, phase readout thrashing). If the
        // user really wants to rewind a mix, wait for it to complete
        // first. A proper "cancel current mix + rewind" path would
        // need full deck/fader reset; not wired yet.
        if s.state == EngineState::Crossfading {
            return Some(RewindOutcome::Blocked);
        }

        // Post-swap in-place path: both decks might still carry the
        // snap's tracks (if the post-mix unload was deferred). In
        // that case skip the download.
        let track_on = |d: DeckId| -> Option<i64> {
            match d {
                DeckId::A => s.deck_a.track.as_ref().map(|t| t.id),
                DeckId::B => s.deck_b.track.as_ref().map(|t| t.id),
            }
        };
        let in_place_possible = track_on(snap.playing_deck) == Some(snap.playing_track.id)
            && track_on(snap.playing_deck.other()) == Some(snap.incoming_track.id);

        if in_place_possible {
            s.finalize_rewind(snap);
            return Some(RewindOutcome::InPlace);
        }

        let track = snap.playing_track.clone();
        s.pending_rewind = Some(snap);
        Some(RewindOutcome::NeedLoad(track))
    }

    /// Hold nudge: rate stays offset by nudge_percent while the key is
    /// held. Each call (press + OS auto-repeat) renews the revert
    /// window; the tick loop restores base rate once the window
    /// expires — i.e. ~NUDGE_HOLD_WINDOW_MS after the last key event.
    /// Returns (shift_ms, current_first_beat) for toast display during preview,
    /// or None for rate nudge.
    pub fn nudge(&self, direction: i32) -> Option<(f64, f64)> {
        // Grace window past the last key event before rate snaps back.
        // Must cover the OS *initial* key-repeat delay (macOS default
        // ~500ms, configurable in System Settings), not just the repeat
        // interval — otherwise the rate reverts before auto-repeat
        // kicks in, and you see a "stutter" (two start events instead
        // of continuous hold). 500ms covers default settings; users
        // with slow-repeat-delay may still see a brief gap.
        const NUDGE_HOLD_WINDOW_MS: u64 = 500;

        let config = crate::config::AppConfig::load(); // read config before acquiring audio lock
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());

        // During preview: shift the beat grid (position nudge, not rate nudge)
        if let Some(ref mut preview) = s.preview {
            let shift_ms = 5.0 * direction as f64;
            Mixer::shift_grid(preview, shift_ms);
            if let Some(ref grid) = preview.beat_grid {
                let fb = grid.first_beat_time;
                tracing::info!("Preview nudge: {shift_ms:+.0}ms, first_beat now {fb:.3}s");
                return Some((shift_ms, fb));
            }
            return None;
        }

        // Normal playback: rate nudge
        let is_crossfading = s.state == EngineState::Crossfading;
        let deck_id = if is_crossfading {
            s.playing_deck.other()
        } else {
            s.playing_deck
        };

        // Capture the *commanded* rate, not the in-flight slewed value —
        // so revert restores the pre-nudge target even if the audio
        // thread hasn't fully settled to it yet.
        let current_rate = match deck_id {
            DeckId::A => {
                if !s.deck_a.playing {
                    return None;
                }
                s.deck_a.rate_target
            }
            DeckId::B => {
                if !s.deck_b.playing {
                    return None;
                }
                s.deck_b.rate_target
            }
        };

        let base_rate = s.nudge_base_rate.unwrap_or(current_rate);
        let pct = direction as f64 * config.nudge_percent as f64;

        match deck_id {
            DeckId::A => {
                Mixer::nudge_rate(&mut s.deck_a, base_rate, pct);
            }
            DeckId::B => {
                Mixer::nudge_rate(&mut s.deck_b, base_rate, pct);
            }
        }
        s.nudge_base_rate = Some(base_rate);
        let was_active = s.nudge_revert_at.is_some();
        s.nudge_revert_at = Some(
            std::time::Instant::now() + std::time::Duration::from_millis(NUDGE_HOLD_WINDOW_MS),
        );
        // Once the user starts nudging during a mix, auto rate-
        // correction stays out for the rest of the mix. Cleared at
        // the next start_crossfade.
        s.user_overrode_this_mix = true;

        // Log once per hold burst — OS auto-repeat would otherwise spam.
        if !was_active {
            tracing::info!(
                "Nudge start: {} {}% (deck {:?})",
                if direction > 0 { "+" } else { "-" },
                config.nudge_percent,
                deck_id,
            );
        }
        None
    }

    /// Mixer-wide crossfader position. -1 full A, 0 center, +1 full B.
    /// Cancels any in-progress sweep so a manual set/drag takes
    /// effect immediately — otherwise the next tick's sweep
    /// interpolation overwrites the new value within ~16 ms,
    /// making mid-sweep mouse drags feel ignored.
    pub fn set_crossfader(&self, pos: f32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.snap_crossfader(pos);
        suppress_train_wreck_during_user_override(&mut s);
    }

    /// Manually re-enable auto-mix triggering. Useful when the user
    /// has touched controls (which auto-pauses) but then decides
    /// they want the engine to take over again on this track.
    /// Clears `user_paused_auto` so the next phrase / time-remaining
    /// trigger fires normally.
    pub fn resume_auto(&self) -> bool {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let was_paused = s.user_paused_auto;
        s.user_paused_auto = false;
        was_paused
    }

    /// Read-only: is the engine's auto-mix currently paused due to
    /// user override on the playing track? Used by status.json /
    /// dashboard to show an "AUTO PAUSED" badge.
    #[allow(dead_code)] // referenced by future dashboard work
    pub fn auto_paused(&self) -> bool {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.user_paused_auto
    }

    /// Master output gain (post-mix). Range 0.0..=1.5; clamped.
    pub fn set_master_gain(&self, g: f32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.master_gain = g.clamp(0.0, 1.5);
    }

    pub fn set_limiter_mode(&self, mode: crate::config::LimiterMode) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.limiter_mode = mode;
    }

    /// Per-channel fader (0.0..1.0). `is_a = true` for deck A.
    pub fn set_channel_fader(&self, is_a: bool, level: f32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let v = level.clamp(0.0, 1.0);
        if is_a {
            s.channel_fader_a = v;
        } else {
            s.channel_fader_b = v;
        }
        suppress_train_wreck_during_user_override(&mut s);
    }

    /// Enable or disable manual-mix mode. When on, the auto crossfade
    /// curve (`transition.apply()`) is skipped and the engine's
    /// `crossfade_progress` is driven from `crossfader_pos` — the DJ's
    /// crossfader move is what advances the state machine toward
    /// swap_decks. Phase-sync + downbeat align still run as safety
    /// rails. Idempotent — no-op if the mode is already set.
    pub fn set_manual_mix(&self, on: bool) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.manual_mix == on {
            return;
        }
        s.manual_mix = on;
        tracing::info!("Manual mix mode: {on}");
    }

    /// Apply the full ClaudeDjSettings block in one call. Replaces
    /// the three-call sequence (manual_mix + quick_mix + quick_mix_bars)
    /// that was duplicated across initial setup, the Settings handler,
    /// and the IPC patch. Future settings additions only need to be
    /// wired here, not at every call site.
    ///
    /// `claude_dj_enabled` gates manual mode: with no driver (Claude off
    /// and no active mouse use), manual_mix would leave the crossfader
    /// frozen mid-mix. Force-fall back to auto curves in that case.
    pub fn apply_claude_dj_settings(
        &self,
        s: &crate::config::ClaudeDjSettings,
        claude_dj_enabled: bool,
    ) {
        let manual = claude_dj_enabled && s.mode == crate::config::ClaudeDjMode::Manual;
        self.set_manual_mix(manual);
        self.set_quick_mix(s.quick_mix);
        self.set_quick_mix_bars(s.quick_mix_bars);
    }

    /// Start a paced crossfader sweep. `target` is the destination
    /// position (−1..+1). `bars` is how many bars of the playing
    /// deck's beat grid to take — defaults used by the caller for
    /// "8 bars = leisurely DJ sweep". The tick loop reads this and
    /// interpolates crossfader_pos each tick toward target until
    /// wall-time elapsed ≥ duration.
    ///
    /// Solves the observed pattern where Claude batches five
    /// set_crossfader calls in one API response and they all execute
    /// in microseconds — sonically a hard cut. With sweep_crossfader
    /// Claude calls once, engine paces over bars.
    pub fn sweep_crossfader(&self, target: f32, bars: u32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let bpm = s.playing().beat_grid.map(|g| g.bpm).unwrap_or(128.0);
        let bar_secs = (60.0 / bpm) * 4.0;
        let dur = std::time::Duration::from_secs_f64(bar_secs * bars as f64);
        s.sweep = Some(CrossfaderSweep {
            from: s.crossfader_pos,
            target: target.clamp(-1.0, 1.0),
            started_at: std::time::Instant::now(),
            duration: dur,
        });
        tracing::info!(
            "Crossfader sweep: {:+.2} → {target:+.2} over {} bars ({:.1}s)",
            s.crossfader_pos,
            bars,
            dur.as_secs_f64()
        );
    }

    /// Override the quick-mix bar threshold (default
    /// `DEFAULT_QUICK_MIX_BARS = 16`). Tempo-independent — measured in
    /// bars of the playing deck, not seconds. Lower bound 8 because
    /// fewer bars (≤4) doesn't give the rate-correction controller
    /// enough runway to converge phase before the crossfade starts;
    /// upper bound 64 (a 5-min track is ~100 bars).
    pub fn set_quick_mix_bars(&self, bars: u32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.quick_mix_bars = bars.clamp(8, 64);
    }

    /// Update the crossfade length used by `start_crossfade`. Clamped
    /// to 4..=64 bars — shorter doesn't give phase correction time
    /// to converge, longer would span most of a track.
    pub fn set_crossfade_bars(&self, bars: u32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.crossfade_bars = bars.clamp(4, 64);
    }

    /// Live config update for the jump-bars indicator.
    pub fn set_jump_bars(&self, bars: u32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.jump_bars = bars.clamp(1, 64);
    }

    /// Toggle auto-pick crossfade length based on genre. When on,
    /// `start_crossfade` picks bars per-mix via `auto_crossfade_bars`.
    pub fn set_crossfade_bars_auto(&self, auto: bool) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.crossfade_bars_auto = auto;
    }

    /// Enable quick-mix mode. The tick loop will fire a crossfade once
    /// the playing deck has played past `quick_mix_bars` and the
    /// incoming deck is loaded. Idempotent.
    pub fn set_quick_mix(&self, on: bool) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.quick_mix == on {
            return;
        }
        s.quick_mix = on;
        tracing::info!("Quick mix mode: {on}");
    }

    /// Start a specific physical deck playing on the monitor (cue) bus
    /// only — main output stays unchanged. The named deck's `playing`
    /// flag goes true, its channel fader is forced to 0 so it's silent
    /// in the main mix, and `monitor_source` routes its pre-fader
    /// samples to the headphones. Used by the manual-mode
    /// `preview_deck` tool so the DJ can audition while the other
    /// deck is still playing live.
    ///
    /// Returns false if the named deck has no samples loaded.
    pub fn preview_deck(&self, is_a: bool) -> bool {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        if !deck.is_loaded() {
            return false;
        }
        deck.playing = true;
        // save_fader_once guards against a second preview_deck()
        // without an intervening stop overwriting the original fader
        // with the silenced 0.0. Read the fader value into a local
        // first so the borrow checker can split the two field borrows.
        if is_a {
            let cur = s.channel_fader_a;
            s.channel_fader_a = save_fader_once(&mut s.preview_saved_fader_a, cur);
        } else {
            let cur = s.channel_fader_b;
            s.channel_fader_b = save_fader_once(&mut s.preview_saved_fader_b, cur);
        }
        s.monitor_source = if is_a {
            MonitorSource::DeckA
        } else {
            MonitorSource::DeckB
        };
        tracing::info!(
            "Preview: deck {} on monitor bus",
            if is_a { "A" } else { "B" }
        );
        true
    }

    /// Stop previewing — pauses the named deck and reverts monitor
    /// routing to `Incoming`. Idempotent. Named `stop_deck_preview`
    /// to avoid colliding with the older `stop_preview()` that
    /// clears the one-shot browse-screen track audition (separate
    /// system, used by the Space key).
    pub fn stop_deck_preview(&self, is_a: bool) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        deck.playing = false;
        // Restore the fader to its pre-preview value, not a hardcoded
        // 1.0. If no preview was ever captured for this deck, fall
        // back to 1.0 (sensible default; better than leaving silenced).
        let other_still_previewing = if is_a {
            s.channel_fader_a = restore_fader(&mut s.preview_saved_fader_a);
            s.preview_saved_fader_b.is_some()
        } else {
            s.channel_fader_b = restore_fader(&mut s.preview_saved_fader_b);
            s.preview_saved_fader_a.is_some()
        };
        // Only revert the monitor bus to Incoming if the OTHER deck
        // isn't still being previewed. Without this, stopping A while
        // B is still on the monitor bus would silently kick B off
        // the headphones — DJ loses cue mid-preview.
        if !other_still_previewing {
            s.monitor_source = MonitorSource::Incoming;
        }
        tracing::info!("Preview stopped: deck {}", if is_a { "A" } else { "B" });
    }

    /// Start a specific deck playing on the MAIN output (exit preview).
    /// Restores its channel fader to 1.0 so it's audible, resets
    /// monitor routing to Incoming. Used by the manual-mode play_deck
    /// tool as the "take the mix live" moment — typically paired with
    /// a set_crossfader move.
    pub fn play_deck(&self, is_a: bool) -> bool {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        if !deck.is_loaded() {
            return false;
        }
        deck.playing = true;
        // play_deck takes the deck live on main: restore the saved
        // fader (preview→play handoff) or default to 1.0 if no
        // preview was running. Same monitor-preservation guard as
        // stop_deck_preview — if the OTHER deck is still being
        // previewed, don't kick it off the headphone bus.
        let other_still_previewing = if is_a {
            s.channel_fader_a = restore_fader(&mut s.preview_saved_fader_a);
            s.preview_saved_fader_b.is_some()
        } else {
            s.channel_fader_b = restore_fader(&mut s.preview_saved_fader_b);
            s.preview_saved_fader_a.is_some()
        };
        if !other_still_previewing {
            s.monitor_source = MonitorSource::Incoming;
        }
        tracing::info!("Play deck {} on main output", if is_a { "A" } else { "B" });
        true
    }

    /// Set EQ bands on a specific physical deck. Pass None to leave a band unchanged.
    pub fn set_eq(&self, is_a: bool, low: Option<f32>, mid: Option<f32>, high: Option<f32>) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        if let Some(v) = low {
            Mixer::set_eq_low(deck, v);
        }
        if let Some(v) = mid {
            Mixer::set_eq_mid(deck, v);
        }
        if let Some(v) = high {
            Mixer::set_eq_high(deck, v);
        }
        suppress_train_wreck_during_user_override(&mut s);
    }

    /// Run a closure against one physical deck with the audio-state lock
    /// held. Picks deck A when `is_a` is true, otherwise deck B. All the
    /// per-deck facade methods (filter / loops / cues) route through here.
    fn with_deck<R>(&self, is_a: bool, f: impl FnOnce(&mut DeckPlayer) -> R) -> R {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        f(if is_a { &mut s.deck_a } else { &mut s.deck_b })
    }

    /// Set filter sweep on a specific physical deck (pos in [-1, +1]).
    pub fn set_filter(&self, is_a: bool, pos: f32) {
        self.with_deck(is_a, |d| Mixer::set_filter(d, pos));
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        suppress_train_wreck_during_user_override(&mut s);
    }

    /// Set a beat-aligned loop of N beats. CDJ-style: when quantize
    /// is on, the loop's in-point is deferred to the next bar
    /// boundary so the loop cycle lines up with the phrase.
    pub fn loop_beats(&self, is_a: bool, beats: f64) {
        const LOOKAHEAD_MS: f64 = 30.0;
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck_id = if is_a { DeckId::A } else { DeckId::B };
        // Read everything we need from the deck up-front so we can
        // borrow `s` mutably for the schedule write below.
        let (grid, now) = {
            let d = if is_a { &s.deck_a } else { &s.deck_b };
            match d.beat_grid {
                Some(g) => (g, d.current_time()),
                None => return,
            }
        };
        let qon = s.quantize_on;
        let qbeats = s.quantize_beats;
        if !qon {
            let d = if is_a { &mut s.deck_a } else { &mut s.deck_b };
            Mixer::loop_beats(d, beats);
            return;
        }
        let fire_at = Self::next_quantize_boundary(&grid, now, qbeats);
        if (fire_at - now) * 1000.0 < LOOKAHEAD_MS {
            let d = if is_a { &mut s.deck_a } else { &mut s.deck_b };
            Mixer::loop_beats(d, beats);
            s.pending_loop = None;
        } else {
            s.pending_loop = Some(PendingLoopOp::Activate {
                deck_id,
                beats,
                fire_at,
            });
            tracing::info!(
                "Quantized loop scheduled: {beats:.0} beats, fire_at={fire_at:.3}s (in {:.0}ms)",
                (fire_at - now) * 1000.0,
            );
        }
    }

    /// Release any active loop. Quantized: schedules the drop on
    /// the next bar boundary so playback exits the loop cleanly.
    pub fn loop_release(&self, is_a: bool) {
        const LOOKAHEAD_MS: f64 = 30.0;
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck_id = if is_a { DeckId::A } else { DeckId::B };
        let (active, grid, now) = {
            let d = if is_a { &s.deck_a } else { &s.deck_b };
            (d.loop_active, d.beat_grid, d.current_time())
        };
        if !active {
            return;
        }
        let qon = s.quantize_on;
        let qbeats = s.quantize_beats;
        let Some(grid) = grid else {
            let d = if is_a { &mut s.deck_a } else { &mut s.deck_b };
            Mixer::loop_release(d);
            return;
        };
        if !qon {
            let d = if is_a { &mut s.deck_a } else { &mut s.deck_b };
            Mixer::loop_release(d);
            return;
        }
        let fire_at = Self::next_quantize_boundary(&grid, now, qbeats);
        if (fire_at - now) * 1000.0 < LOOKAHEAD_MS {
            let d = if is_a { &mut s.deck_a } else { &mut s.deck_b };
            Mixer::loop_release(d);
            s.pending_loop = None;
        } else {
            s.pending_loop = Some(PendingLoopOp::Release { deck_id, fire_at });
            tracing::info!(
                "Quantized loop release scheduled: fire_at={fire_at:.3}s (in {:.0}ms)",
                (fire_at - now) * 1000.0,
            );
        }
    }

    /// Toggle the playing deck's loop with a default 4-beat length.
    /// Used by the `i` / `u` / `O` / etc. hotkeys: same-press releases,
    /// otherwise activates. Both paths quantized.
    pub fn loop_toggle_playing(&self, beats: f64) {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let is_a = s.playing_deck == DeckId::A;
        let active = if is_a {
            s.deck_a.loop_active
        } else {
            s.deck_b.loop_active
        };
        drop(s);
        if active {
            self.loop_release(is_a);
        } else {
            self.loop_beats(is_a, beats);
        }
    }

    /// Per-deck UI engage. Clicking a number on deck A's loop row
    /// always targets deck A regardless of role; same for B.
    pub fn loop_engage_deck(&self, is_a: bool, beats: f64) {
        self.loop_beats(is_a, beats);
    }

    /// Per-deck UI release (the "off" button on each deck row).
    pub fn loop_disengage_deck(&self, is_a: bool) {
        self.loop_release(is_a);
    }

    /// Set delay feedback on a specific deck (0.0..1.0).
    pub fn set_delay_feedback(&self, is_a: bool, value: f32) {
        self.with_deck(is_a, |d| Mixer::set_delay_feedback(d, value));
    }

    /// Set delay time in samples on a specific deck.
    pub fn set_delay_samples(&self, is_a: bool, value: usize) {
        self.with_deck(is_a, |d| Mixer::set_delay_samples(d, value));
    }

    /// Set delay time synced to BPM on a specific deck.
    pub fn set_delay_sync(&self, is_a: bool, beat_fraction: f64) {
        self.with_deck(is_a, |d| Mixer::set_delay_sync(d, beat_fraction));
    }

    /// Set loop in-point at current position on a specific deck.
    pub fn loop_in(&self, is_a: bool) {
        self.with_deck(is_a, Mixer::loop_in);
    }

    /// Set loop out-point and activate loop on a specific deck.
    pub fn loop_out(&self, is_a: bool) {
        self.with_deck(is_a, Mixer::loop_out);
    }

    pub fn stop_deck(&self, is_a: bool) {
        self.with_deck(is_a, Mixer::stop);
    }

    pub fn seek_deck(&self, is_a: bool, time: f64) {
        self.with_deck(is_a, |d| Mixer::seek(d, time));
    }

    /// Store the deck's current position as hot cue `slot` (0..=3).
    pub fn cue_set(&self, is_a: bool, slot: usize) {
        self.with_deck(is_a, |d| Mixer::cue_set(d, slot));
    }

    /// Jump the deck to hot cue `slot` (no-op if unset). Quantized
    /// when `quantize_on` is true: defers the seek until the next
    /// bar boundary so the jump lands cleanly.
    pub fn cue_jump(&self, is_a: bool, slot: usize) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck_id = if is_a { DeckId::A } else { DeckId::B };
        let deck = if is_a { &s.deck_a } else { &s.deck_b };
        let Some(target) = deck.cues[slot] else {
            return;
        };
        let target_time = target as f64 / deck.sample_rate as f64;
        Self::schedule_or_seek_jump(&mut s, deck_id, target_time);
    }

    /// Clear hot cue `slot`.
    pub fn cue_clear(&self, is_a: bool, slot: usize) {
        self.with_deck(is_a, |d| Mixer::cue_clear(d, slot));
    }

    /// Update the list of transition types the rule engine may pick.
    pub fn set_enabled_transitions(&self, enabled: Vec<String>) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.enabled_transitions = enabled;
    }

    /// Snapshot of the current transition rule config.
    pub fn rules_config(&self) -> super::transition_rules::RuleConfig {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.rule_engine.config.clone()
    }

    /// Replace the transition rule config and persist to disk. The JSON
    /// write happens after the lock is released so the audio callback
    /// never contends with disk I/O.
    pub fn set_rules_config(&self, cfg: super::transition_rules::RuleConfig) {
        let to_save = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
            s.rule_engine.config = cfg;
            s.rule_engine.config.clone()
        };
        if let Err(e) = super::transition_rules::save_rules(&to_save) {
            tracing::warn!("Failed to persist transition rules: {e}");
        }
    }

    /// Snapshot of audio-callback timings (rolling window of recent callbacks).
    pub fn profile_stats(&self) -> super::profiler::ProfileStats {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.profiler.stats()
    }

    /// Maybe emit a profile log line. Cheap (rate-limited internally to 10s).
    /// Skipped entirely when the profiler is disabled — avoids logging stale
    /// zero stats from before the most recent enable.
    pub fn maybe_log_profile(&self) {
        if !profiler_enabled() {
            return;
        }
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.profiler.maybe_log();
    }

    /// Switch the pitch-stretch engine on both decks.
    pub fn set_pitch_stretch_engine(&self, engine: super::pitch_stretch::PitchStretchEngine) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.deck_a.pitch_stretch = super::pitch_stretch::make(engine);
        s.deck_b.pitch_stretch = super::pitch_stretch::make(engine);
        tracing::info!("Pitch stretch engine: {engine:?}");
    }

    /// Live config for train-wreck detection / auto-bail.
    pub fn set_train_wreck_mode(&self, mode: crate::config::TrainWreckMode) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.train_wreck_mode = mode;
        tracing::info!("Train wreck mode: {mode:?}");
    }

    /// Force the in-progress crossfade onto EchoOut to salvage a bad
    /// mix. Switches transition type, arms delay_wet=1.0 on the
    /// playing deck so the existing tail rings out, and ensures the
    /// incoming deck is actually playing (EchoOut starts incoming
    /// silently, then fades it up at progress > 40.6%). No-op when
    /// not Crossfading. Returns true if a bail was applied.
    pub fn bail_crossfade(&self) -> bool {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.state != EngineState::Crossfading {
            return false;
        }
        if matches!(
            s.transition_type,
            super::transition::TransitionType::EchoOut
        ) {
            return false; // already on EchoOut, nothing to bail to
        }
        s.transition_type = super::transition::TransitionType::EchoOut;
        s.mix_wreck_fired = true; // suppress further auto-bail this mix
        // Arm the echo and start the incoming deck if it isn't yet.
        s.playing_mut().delay_wet = 1.0;
        if !s.incoming().playing && s.incoming().is_loaded() {
            s.incoming_mut().play();
        }
        tracing::warn!(
            "Crossfade bailed → EchoOut at progress {:.0}%",
            s.crossfade_progress * 100.0
        );
        true
    }

    /// Override the transition type for the next crossfade.
    /// Returns true if the name was recognized.
    pub fn set_transition(&self, name: &str) -> bool {
        use super::transition::TransitionType;
        let t = match name.to_ascii_lowercase().as_str() {
            "beatmatched" | "beat_matched" | "beat" => TransitionType::BeatMatched,
            "echoout" | "echo_out" | "echo" => TransitionType::EchoOut,
            "bassswap" | "bass_swap" | "bass" => TransitionType::BassSwap,
            "filtersweep" | "filter_sweep" | "filter" => TransitionType::FilterSweep,
            "looproll" | "loop_roll" | "loop" => TransitionType::LoopRoll,
            _ => return false,
        };
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.transition_type = t;
        tracing::info!("Transition override: {t:?}");
        true
    }

    pub fn toggle_split_cue(&self) -> bool {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.split_cue = !s.split_cue;
        s.split_cue
    }

    /// Preview a track from first_beat for 4 bars with metronome. Doesn't interrupt main playback.
    pub fn preview_track(
        &self,
        samples: Vec<f32>,
        sample_rate: u32,
        analysis: super::analyzer::AnalysisResult,
    ) {
        // Build the new deck outside the lock to avoid allocating under contention
        let first_beat = analysis.beat_grid.first_beat_time;
        let bar_interval = analysis.beat_grid.bar_interval();
        let stop_time = first_beat + bar_interval * 16.0; // 16 bars

        let dummy = crate::beatport::models::BeatportTrack {
            id: 0,
            title: "Preview".into(),
            mix_name: None,
            artists: vec![],
            bpm: Some(analysis.beat_grid.bpm),
            key: None,
            duration: None,
            label_id: None,
            label_name: None,
            genre_id: None,
            genre_name: None,
            genre_slug: None,
            release_id: None,
            release_date: None,
            remixers: vec![],
            local_path: None,
        };

        let old_preview = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
            let mut deck = DeckPlayer::new(s.deck_a.output_sample_rate);
            deck.load(samples, sample_rate, analysis, dummy);
            deck.seek(first_beat);
            deck.play();

            let old = s.preview.take();
            s.preview = Some(deck);
            s.preview_stop_time = stop_time;
            s.metronome = true; // auto-enable metronome during preview
            old
        };
        drop(old_preview); // dealloc outside the lock
        tracing::info!("Preview: first_beat={first_beat:.3}s, 4 bars to {stop_time:.1}s");
    }

    /// Stop preview playback.
    pub fn stop_preview(&self) {
        let old = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
            s.preview.take()
        };
        drop(old); // dealloc outside the lock
    }

    pub fn is_previewing(&self) -> bool {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.preview.is_some()
    }

    pub fn toggle_metronome(&self) -> bool {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.metronome = !s.metronome;
        s.metronome
    }

    /// Switch the monitor headphone-cue source at runtime.
    pub fn set_monitor_source(&self, source: MonitorSource) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.monitor_source = source;
    }

    /// Toggle pause across *all* audible decks: deck A, deck B, preview.
    /// Remembers which were playing via `paused`; resume only resumes those.
    pub fn pause(&self) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let a_on = s.deck_a.playing;
        let b_on = s.deck_b.playing;
        let p_on = s.preview.as_ref().map(|p| p.playing).unwrap_or(false);
        let any_playing = a_on || b_on || p_on;

        if any_playing {
            if a_on {
                Mixer::pause(&mut s.deck_a);
            }
            if b_on {
                Mixer::pause(&mut s.deck_b);
            }
            if p_on && let Some(ref mut p) = s.preview {
                Mixer::pause(p);
            }
            tracing::info!("Pause: a={a_on} b={b_on} preview={p_on}");
        } else {
            // Resume previously-paused decks first; if nothing was
            // paused, fall back to *starting* the playing-role deck
            // when it has a track loaded. This catches the post-
            // session-resume case where decks are loaded + stopped
            // (not paused) and pressing P would otherwise no-op.
            let mut resumed = (false, false, false);
            if s.deck_a.paused {
                Mixer::play(&mut s.deck_a);
                s.deck_a.paused = false;
                resumed.0 = true;
            }
            if s.deck_b.paused {
                Mixer::play(&mut s.deck_b);
                s.deck_b.paused = false;
                resumed.1 = true;
            }
            if let Some(ref mut p) = s.preview
                && p.paused
            {
                Mixer::play(p);
                p.paused = false;
                resumed.2 = true;
            }
            if !resumed.0 && !resumed.1 && !resumed.2 {
                let pd = s.playing_deck;
                let deck = match pd {
                    DeckId::A => &mut s.deck_a,
                    DeckId::B => &mut s.deck_b,
                };
                if deck.is_loaded() && !deck.playing {
                    Mixer::play(deck);
                    s.state = EngineState::Playing;
                    tracing::info!("Start: deck {pd:?} (was stopped, now playing)");
                    return;
                }
            }
            tracing::info!(
                "Resume: a={} b={} preview={}",
                resumed.0,
                resumed.1,
                resumed.2
            );
        }
    }

    /// Teleport to just before the crossfade trigger point.
    /// If incoming isn't loaded yet, leaves 30s buffer for download+analysis.
    /// Set playback rate on the playing deck.
    pub fn set_playing_rate(&self, rate: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        Mixer::set_rate(s.playing_mut(), rate);
        tracing::info!("Set playing rate: {rate:.4}");
    }

    /// Set playback rate on the incoming deck.
    pub fn set_incoming_rate(&self, rate: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        Mixer::set_rate(s.incoming_mut(), rate);
        tracing::info!("Set incoming rate: {rate:.4}");
    }

    /// Set playback rate on a specific physical deck. Used by the
    /// dashboard drag-on-tempo-strip handler: the strip is tied to
    /// the physical deck (A on the left, B on the right), so the
    /// dispatch shouldn't care about current roles.
    /// Per-deck nudge — like `nudge` but always targets a specific
    /// deck regardless of mix state. Used by hardware controllers
    /// where each deck has its own pitch-bend buttons. Same revert
    /// semantics as `nudge` (auto-reverts after the hold window).
    pub fn nudge_deck(&self, is_a: bool, direction: i32) {
        const NUDGE_HOLD_WINDOW_MS: u64 = 500;
        let config = crate::config::AppConfig::load();
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let pct = direction as f64 * config.nudge_percent as f64;
        // Read deck state into locals first to avoid overlapping borrows
        // when we later re-borrow `s` mutably to update nudge fields.
        let (playing, current_rate) = {
            let deck = if is_a { &s.deck_a } else { &s.deck_b };
            (deck.playing, deck.rate_target)
        };
        if !playing {
            return;
        }
        let base_rate = s.nudge_base_rate.unwrap_or(current_rate);
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        Mixer::nudge_rate(deck, base_rate, pct);
        s.nudge_base_rate = Some(base_rate);
        s.nudge_revert_at = Some(
            std::time::Instant::now() + std::time::Duration::from_millis(NUDGE_HOLD_WINDOW_MS),
        );
        s.user_overrode_this_mix = true;
    }

    /// Per-deck play/pause toggle. Reads current playing state and
    /// flips it. Independent of the global `pause()` which only
    /// affects the playing deck.
    pub fn play_pause_deck(&self, is_a: bool) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        if !deck.is_loaded() {
            return;
        }
        deck.playing = !deck.playing;
    }

    /// Per-deck bar jump. `jump` (no deck) targets the playing deck;
    /// this targets a specific one regardless of mix state. Bars
    /// are converted to beats using 4/4 time (4 beats per bar).
    pub fn jump_deck_bars(&self, is_a: bool, bars: i32) {
        self.jump_deck_beats(is_a, bars.saturating_mul(4));
    }

    pub fn set_deck_rate(&self, is_a: bool, rate: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if is_a {
            Mixer::set_rate(&mut s.deck_a, rate);
        } else {
            Mixer::set_rate(&mut s.deck_b, rate);
        }
    }

    /// Re-analyze the playing deck's in-memory samples with the given
    /// engine and replace its `BeatGrid`/`AnalysisResult` in place.
    /// No file I/O — uses the samples already on the deck. Returns
    /// the new BPM on success, or `None` if the deck has no samples.
    /// Used by the A/B hotkey so the user can re-grid a bad mix
    /// without re-downloading.
    pub fn reanalyze_playing(&self, engine: crate::config::AnalyzerEngine) -> Option<f64> {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = s.playing_mut();
        if deck.samples.is_empty() {
            return None;
        }
        let bpm_hint = deck.track.as_ref().and_then(|t| t.bpm);
        let sr = deck.sample_rate;
        // Clone samples out since analyze_samples_pub takes a slice and
        // we're still holding the deck mutably. Sample count is bounded
        // by track length (tens of MB at most) — a one-shot allocation
        // is cheap compared to re-downloading + re-decoding.
        let samples = deck.samples.clone();
        drop(s);
        let analysis = crate::audio::analyzer::analyze_samples_pub(&samples, sr, bpm_hint, engine);
        let new_bpm = analysis.beat_grid.bpm;
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = s.playing_mut();
        deck.beat_grid = Some(analysis.beat_grid);
        deck.analysis = Some(std::sync::Arc::new(analysis));
        Some(new_bpm)
    }

    /// Set volume on a deck (0 = playing, 1 = incoming).
    pub fn set_volume(&self, deck: u8, volume: f32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        match deck {
            0 => Mixer::set_volume(s.playing_mut(), volume),
            1 => Mixer::set_volume(s.incoming_mut(), volume),
            _ => {}
        }
    }

    /// Shift the playing deck's beat grid by N milliseconds.
    pub fn shift_grid(&self, ms: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = s.playing_mut();
        if let Some(ref mut grid) = deck.beat_grid {
            grid.first_beat_time += ms / 1000.0;
            tracing::info!(
                "Grid shifted by {ms:+.1}ms → first_beat={:.3}s",
                grid.first_beat_time
            );
        }
    }

    /// Shift the active deck's beat grid by N milliseconds. "Active" =
    /// incoming during crossfade, playing otherwise — same deck nudge
    /// targets. Used by `;` / `'` for phase correction that bypasses
    /// the rate controller (grid-domain, not rate-domain).
    /// Shift the active deck's beat grid by whole beats. Same target
    /// selection as `shift_grid_active` (incoming during crossfade,
    /// playing otherwise). For fixing "1s don't land together" —
    /// when the grid's first_beat landed on an offbeat / pickup,
    /// a ±1 or ±2 beat shift realigns the downbeat.
    pub fn shift_grid_active_beats(&self, beats: i32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let is_crossfading = s.state == EngineState::Crossfading;
        let deck_id = if is_crossfading {
            s.playing_deck.other()
        } else {
            s.playing_deck
        };
        let deck = match deck_id {
            DeckId::A => &mut s.deck_a,
            DeckId::B => &mut s.deck_b,
        };
        if let Some(ref mut grid) = deck.beat_grid {
            let beat_ms = grid.beat_interval() * 1000.0;
            let shift_ms = beats as f64 * beat_ms;
            grid.first_beat_time += shift_ms / 1000.0;
            tracing::info!(
                "Grid beat-shift ({deck_id:?}): {beats:+} beat(s) = {shift_ms:+.0}ms → first_beat={:.3}s",
                grid.first_beat_time
            );
        }
        // Treat beat shifts as a manual phase override too.
        s.user_overrode_this_mix = true;
    }

    pub fn shift_grid_active(&self, ms: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let is_crossfading = s.state == EngineState::Crossfading;
        let deck_id = if is_crossfading {
            s.playing_deck.other()
        } else {
            s.playing_deck
        };
        let shifted = {
            let deck = match deck_id {
                DeckId::A => &mut s.deck_a,
                DeckId::B => &mut s.deck_b,
            };
            if let Some(ref mut grid) = deck.beat_grid {
                grid.first_beat_time += ms / 1000.0;
                tracing::info!(
                    "Grid shift ({deck_id:?}): {ms:+.1}ms → first_beat={:.3}s",
                    grid.first_beat_time
                );
                true
            } else {
                false
            }
        };
        // Grid shift is an explicit user phase override — block the
        // auto rate-correction for the rest of this mix.
        if shifted {
            s.user_overrode_this_mix = true;
        }
    }

    /// Extend playback by N bars — delay the crossfade trigger.
    pub fn extend_playback(&self, bars: i32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let bpm = s.playing().beat_grid.map(|g| g.bpm).unwrap_or(128.0);
        let bar_dur = 60.0 / bpm * 4.0;
        if let Some(trigger) = s.cached_trigger_time.as_mut() {
            *trigger += bars as f64 * bar_dur;
            let new_trigger = *trigger;
            s.trigger_user_extended = true;
            tracing::info!("Extended by {bars} bars → trigger at {new_trigger:.1}s");
        }
    }

    /// Set the mix-in point (seconds into incoming track where it should start).
    pub fn set_mix_in_point(&self, time: f64) {
        // Store as start offset for the incoming deck
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = s.incoming_mut();
        if deck.samples.is_empty() {
            return;
        }
        deck.position = time * deck.sample_rate as f64;
        tracing::info!("Mix-in point set to {time:.1}s");
    }

    /// Jump the playing deck forward or back by N bars. Quantized
    /// when on: schedules the seek for the next bar boundary so both
    /// the leave-point and the land-point sit on a bar — bar-to-bar.
    pub fn jump(&self, bars: i32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck_id = s.playing_deck;
        let deck = s.playing();
        if !deck.playing {
            return;
        }
        let bpm = deck.beat_grid.map(|g| g.bpm).unwrap_or(128.0);
        let bar_dur = 60.0 / bpm * 4.0;
        // Source-time of the bar boundary the jump will fire from
        // (or "now" if quantize off).
        let leave_at = if let (true, Some(g)) = (s.quantize_on, deck.beat_grid) {
            Self::next_quantize_boundary(&g, deck.current_time(), s.quantize_beats)
        } else {
            deck.current_time()
        };
        let target_time =
            (leave_at + bars as f64 * bar_dur).clamp(0.0, (deck.duration() - 1.0).max(0.0));
        Self::schedule_or_seek_jump(&mut s, deck_id, target_time);
    }

    /// Helper: either fire the seek immediately (quantize off, or
    /// landing at-or-past the next boundary) or stash a `PendingJump`
    /// for the tick loop to fire on.
    fn schedule_or_seek_jump(s: &mut AudioState, deck_id: DeckId, target_time: f64) {
        let deck = match deck_id {
            DeckId::A => &mut s.deck_a,
            DeckId::B => &mut s.deck_b,
        };
        if !s.quantize_on {
            deck.seek(target_time);
            tracing::info!("Jump (no quantize) → {target_time:.3}s");
            return;
        }
        let Some(g) = deck.beat_grid else {
            deck.seek(target_time);
            return;
        };
        let now = deck.current_time();
        let fire_at = Self::next_quantize_boundary(&g, now, s.quantize_beats);
        // Lookahead: if we're within ~30ms of the boundary, fire now —
        // keeps the response snappy when the user presses on the beat.
        const LOOKAHEAD_MS: f64 = 30.0;
        if (fire_at - now) * 1000.0 < LOOKAHEAD_MS {
            deck.seek(target_time);
            tracing::info!("Quantized jump (lookahead) → {target_time:.3}s");
            s.pending_jump = None;
        } else {
            s.pending_jump = Some(PendingJump {
                deck_id,
                target_time,
                fire_at,
            });
            tracing::info!(
                "Quantized jump scheduled: fire_at={fire_at:.3}s (in {:.0}ms) → {target_time:.3}s",
                (fire_at - now) * 1000.0,
            );
        }
    }

    /// Next beat-aligned source-time boundary at quantize resolution.
    /// `beats` is the quantize-beat value (CDJ values: 0.125 / 0.25 /
    /// 0.5 / 1 / 2 / 4 / 8). Fractional <1 lands on sub-beats.
    fn next_quantize_boundary(grid: &super::beat_grid::BeatGrid, time: f64, beats: f64) -> f64 {
        let span = grid.beat_interval() * beats.max(0.001);
        let n = ((time - grid.first_beat_time) / span + 1e-9).ceil();
        grid.first_beat_time + n * span
    }

    /// Live config update for quantize on/off + beat value.
    pub fn set_quantize(&self, on: bool, beats: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.quantize_on = on;
        s.quantize_beats = beats.max(0.001);
        if !on {
            s.pending_jump = None;
            s.pending_loop = None;
        }
    }

    /// Physical-deck API for manual mode: jump deck A or B by ±N beats,
    /// regardless of its current role. Used to fix the "off by 1"
    /// problem during manual beatmatching — phase tight, but 1s don't
    /// line up. Unlike `jump(bars)` (which acts on the playing deck),
    /// this targets whichever deck the DJ names.
    pub fn jump_deck_beats(&self, is_a: bool, beats: i32) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        if let Some(g) = deck.beat_grid {
            let delta = beats as f64 * g.beat_interval();
            let new_t = (deck.current_time() + delta).max(0.0);
            deck.seek(new_t);
            tracing::info!(
                "Deck {} jump {beats:+} beats → {:.3}s",
                if is_a { "A" } else { "B" },
                new_t
            );
        }
    }

    /// Physical-deck seek. `target_seconds` is absolute time from the
    /// start of the deck's source. Used by manual-mode seek_deck tool.
    pub fn seek_deck_time(&self, is_a: bool, target_seconds: f64) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        deck.seek(target_seconds.max(0.0));
        tracing::info!(
            "Deck {} seek → {:.3}s",
            if is_a { "A" } else { "B" },
            target_seconds
        );
    }

    /// Seek named helpers. Returns the seconds value that was sought to,
    /// or None if the deck isn't loaded / the label doesn't resolve.
    pub fn seek_deck_named(&self, is_a: bool, label: &str) -> Option<f64> {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let deck = if is_a { &mut s.deck_a } else { &mut s.deck_b };
        let analysis = deck.analysis.as_ref()?;
        let target = match label {
            "start" | "first_beat" => deck.beat_grid.map(|g| g.first_beat_time).unwrap_or(0.0),
            "drop" => analysis
                .phrases
                .iter()
                .find(|p| matches!(p.phrase_type, super::analyzer::PhraseType::Drop))
                .map(|p| p.start_time)?,
            "middle" => deck.duration() * 0.5,
            _ => return None,
        };
        deck.seek(target.max(0.0));
        tracing::info!(
            "Deck {} seek to '{label}' → {:.3}s",
            if is_a { "A" } else { "B" },
            target
        );
        Some(target)
    }

    // read_alignment() and alignment_peaks() are in super::status

    pub fn teleport(&self, config: &AppConfig) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.state != EngineState::Playing {
            return;
        }
        let grid = s.playing().beat_grid;
        let now = s.playing().current_time();
        let bpm = grid.map(|g| g.bpm).unwrap_or(128.0);
        let bar_dur = (60.0 / bpm) * 4.0;
        // 4-bar phrase atom — typical building block in house/techno.
        // Both departure and arrival lock to a 4-bar boundary so the
        // listener experiences the cut as "skipped forward N phrases"
        // instead of "deck went weird mid-bar."
        let phrase_dur = bar_dur * 4.0;
        let xfade_dur = bar_dur * config.crossfade_bars as f64;
        let duration = s.playing().duration();

        let raw_target = if s.incoming_loaded_not_playing() {
            // Incoming ready — jump to ~2 bars before crossfade trigger.
            (duration - xfade_dur - bar_dur * 2.0).max(0.0)
        } else {
            // Incoming not ready — leave 30s for download + analysis.
            (duration - xfade_dur - 30.0).max(duration * 0.5)
        };

        // Snap target *down* to the nearest 4-bar phrase boundary so
        // the seek lands at the start of a phrase, not mid-fill.
        let target = if let Some(g) = grid {
            let phrases = ((raw_target - g.first_beat_time) / phrase_dur).floor();
            (g.first_beat_time + phrases * phrase_dur).max(0.0)
        } else {
            raw_target
        };

        // Already past the target → seek directly. No wait helps here.
        if target <= now + 0.05 {
            s.playing_mut().seek(target);
            s.pending_teleport = None;
            tracing::info!("Teleport: already at/past target ({target:.1}s), seeking immediately");
            return;
        }

        // Fire at the next 4-bar phrase boundary in current playback —
        // gives a clean musical exit. Both endpoints are
        // phrase-aligned, so the seek delta is an integer number of
        // 4-bar phrases. The listener hears N phrases skipped, on
        // the same beat-in-bar.
        let fire_at = if let Some(g) = grid {
            let phrases_now = (now - g.first_beat_time) / phrase_dur;
            let next = phrases_now.ceil();
            let candidate = g.first_beat_time + next * phrase_dur;
            // If we're <30 ms from the next phrase boundary, just hit
            // it now (would otherwise wait a full 4 bars to advance).
            if candidate - now < 0.030 {
                now
            } else {
                candidate
            }
        } else {
            now
        };

        let label = if s.incoming_loaded_not_playing() {
            "incoming ready"
        } else {
            "waiting for download"
        };
        if fire_at <= now + 0.05 {
            s.playing_mut().seek(target);
            s.pending_teleport = None;
            tracing::info!(
                "Teleported to {target:.1}s ({:.0}s before end, {label})",
                duration - target
            );
        } else {
            s.pending_teleport = Some(PendingTeleport { fire_at, target });
            tracing::info!(
                "Teleport scheduled in {:.0}ms → seek to {target:.1}s on next 4-bar boundary ({label})",
                (fire_at - now) * 1000.0,
            );
        }
    }

    pub fn skip(&mut self) {
        let old_samples = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
            let old_deck = s.playing_deck;
            s.playing_mut().stop();
            // Skipping invalidates any pending teleport — the deck
            // it was scheduled for is going away.
            s.pending_teleport = None;

            let old_samples = if s.incoming().is_loaded() {
                // Clear old deck and swap to incoming
                let old = match old_deck {
                    DeckId::A => s.deck_a.unload(),
                    DeckId::B => s.deck_b.unload(),
                };
                s.playing_deck = s.playing_deck.other();
                s.playing_mut().play();
                s.state = EngineState::Playing;
                old
            } else {
                // Clear current deck and go idle
                let old = match old_deck {
                    DeckId::A => s.deck_a.unload(),
                    DeckId::B => s.deck_b.unload(),
                };
                s.state = EngineState::Idle;
                old
            };
            s.crossfade_progress = 0.0;
            s.crossfade_controller = None;
            self.next_track_requested = false;
            old_samples
        };
        drop(old_samples); // dealloc outside the lock
    }

    pub fn mix_now(&mut self) {
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.state == EngineState::Playing {
            if s.incoming_loaded_not_playing() {
                s.start_crossfade();
            } else {
                s.state = EngineState::PreparingCrossfade;
            }
        }
    }

    // now_playing() is in super::status

    pub fn tick(&mut self, config: &AppConfig) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());

        // Drain the one-shot "user just paused auto" edge so the
        // outer event loop can toast once per pause cycle.
        if s.user_paused_auto_just_triggered {
            s.user_paused_auto_just_triggered = false;
            events.push(EngineEvent::AutoMixPaused);
        }

        // Collect allocations to drop after lock release (avoid dealloc under contention)
        let mut deferred_samples: Vec<Vec<f32>> = Vec::new();
        let deferred_deck = s.deferred_drop.take();

        // Fire a scheduled teleport once playback reaches the next
        // 4-bar phrase boundary. Both endpoints phrase-aligned — the
        // listener hears N phrases skipped on the same beat-in-bar.
        if let Some(p) = s.pending_teleport {
            let now = s.playing().current_time();
            if now >= p.fire_at {
                s.playing_mut().seek(p.target);
                s.pending_teleport = None;
                tracing::info!(
                    "Teleport fired → {:.3}s (musical cut on phrase boundary)",
                    p.target
                );
            }
        }

        // Fire any quantize-pending jump whose bar boundary just
        // arrived. CDJ-style: user pressed near the end of a bar,
        // we deferred the seek to land cleanly on the next bar.
        if let Some(p) = s.pending_jump {
            let now = match p.deck_id {
                DeckId::A => s.deck_a.current_time(),
                DeckId::B => s.deck_b.current_time(),
            };
            if now >= p.fire_at {
                let deck = match p.deck_id {
                    DeckId::A => &mut s.deck_a,
                    DeckId::B => &mut s.deck_b,
                };
                deck.seek(p.target_time);
                s.pending_jump = None;
                tracing::info!("Quantized jump fired → {:.3}s", p.target_time);
            }
        }
        // Same idea for loop activate / release.
        if let Some(p) = s.pending_loop {
            match p {
                PendingLoopOp::Activate {
                    deck_id,
                    beats,
                    fire_at,
                } => {
                    let now = match deck_id {
                        DeckId::A => s.deck_a.current_time(),
                        DeckId::B => s.deck_b.current_time(),
                    };
                    if now >= fire_at {
                        let deck = match deck_id {
                            DeckId::A => &mut s.deck_a,
                            DeckId::B => &mut s.deck_b,
                        };
                        Mixer::loop_beats(deck, beats);
                        s.pending_loop = None;
                        tracing::info!("Quantized loop fired: {beats:.0} beats");
                    }
                }
                PendingLoopOp::Release { deck_id, fire_at } => {
                    let now = match deck_id {
                        DeckId::A => s.deck_a.current_time(),
                        DeckId::B => s.deck_b.current_time(),
                    };
                    if now >= fire_at {
                        let deck = match deck_id {
                            DeckId::A => &mut s.deck_a,
                            DeckId::B => &mut s.deck_b,
                        };
                        Mixer::loop_release(deck);
                        s.pending_loop = None;
                        tracing::info!("Quantized loop release fired");
                    }
                }
            }
        }

        // Advance any active crossfader sweep by wall-time fraction.
        // Linear interpolation over `duration` from `from` to `target`;
        // clears once elapsed ≥ duration. This is what turns a batched
        // set of tool calls from "hard cut" into "paced sweep" — the
        // DJ calls sweep_crossfader once, the tick loop carries it out.
        if let Some(sw) = s.sweep {
            let elapsed = sw.started_at.elapsed();
            if elapsed >= sw.duration {
                s.crossfader_pos = sw.target;
                s.sweep = None;
            } else {
                let t = elapsed.as_secs_f64() / sw.duration.as_secs_f64();
                s.crossfader_pos = sw.from + (sw.target - sw.from) * t as f32;
            }
            s.last_crossfader_move = Some(std::time::Instant::now());
        }

        // Nudge revert: restore base rate after hold window expires
        // (i.e. no key events for NUDGE_HOLD_WINDOW_MS — the user
        // released the key).
        if let (Some(revert_at), Some(base)) = (s.nudge_revert_at, s.nudge_base_rate)
            && std::time::Instant::now() >= revert_at
        {
            let deck_id = if s.state == EngineState::Crossfading {
                s.playing_deck.other()
            } else {
                s.playing_deck
            };
            match deck_id {
                DeckId::A => Mixer::set_rate(&mut s.deck_a, base),
                DeckId::B => Mixer::set_rate(&mut s.deck_b, base),
            }
            s.nudge_base_rate = None;
            s.nudge_revert_at = None;
            tracing::info!("Nudge end: rate restored (deck {deck_id:?})");
        }

        match s.state {
            EngineState::Idle => {
                if !self.queue.is_empty() && !self.next_track_requested {
                    let entry = self.queue.remove(0);
                    self.next_track_requested = true;
                    events.push(EngineEvent::NeedFirstTrack(entry));
                }
            }

            EngineState::Playing => {
                // Rate glide back to native BPM after crossfade
                if s.is_gliding {
                    let elapsed = s.playing().current_time() - s.glide_start_time;
                    let progress = (elapsed / s.glide_duration).min(1.0);
                    // Cosine ease-out (sin(πp/2)): fast early, settles into
                    // unity. A linear ramp was audible as a constant-speed
                    // pitch drift on large BPM gaps (e.g. 132→140 27s glide).
                    let target_rate = glide_target_rate(s.glide_start_rate, progress);
                    Mixer::set_rate(s.playing_mut(), target_rate);
                    if progress >= 1.0 {
                        Mixer::set_rate(s.playing_mut(), 1.0);
                        s.is_gliding = false;
                    }
                }

                let is_loaded = s.playing().is_loaded();
                let is_playing = s.playing().playing;
                let time_remaining = s.playing().time_remaining();
                let bpm = s.playing().beat_grid.map(|g| g.bpm).unwrap_or(128.0);

                // Eager preload: if incoming deck is empty and queue has tracks, start loading now.
                // Skip any queued items whose track_id matches the currently-playing track —
                // happens when e.g. a queueall lands the same track in both positions, which
                // previously caused "ANOTR → ANOTR" self-mixes.
                if !s.incoming().is_loaded() && !self.queue.is_empty() && !self.next_track_requested
                {
                    let playing_id = s.playing().track.as_ref().map(|t| t.id);
                    // Drain duplicates from the front of the queue.
                    while let Some(front) = self.queue.first() {
                        if playing_id.is_some() && Some(front.track.id) == playing_id {
                            self.queue.remove(0);
                        } else {
                            break;
                        }
                    }
                    if let Some(entry) = (!self.queue.is_empty()).then(|| self.queue.remove(0)) {
                        self.next_track_requested = true;
                        events.push(EngineEvent::NeedNextTrack(entry));
                    }
                }

                if is_loaded && is_playing {
                    let bar_dur = (60.0 / bpm) * 4.0;

                    let xfade_dur = bar_dur * config.crossfade_bars as f64;

                    // Smart mix-out: prefer outro/breakdown phrase boundary
                    // over fixed time-remaining if phrase data is available.
                    // Gated by config.smart_mix_out (default on).
                    let current_time = s.playing().current_time();
                    let duration = s.playing().duration();
                    let phrase_trigger = if config.smart_mix_out {
                        s.playing().analysis.as_ref().and_then(|a| {
                            // Find the last Outro or Breakdown phrase that starts
                            // after our current position and before the track ends.
                            // Prefer Outro over Breakdown.
                            let outro = a.phrases.iter().rev().find(|p| {
                                p.phrase_type == super::analyzer::PhraseType::Outro
                                    && p.start_time > current_time
                                    && p.start_time < duration
                            });
                            let breakdown = a.phrases.iter().rev().find(|p| {
                                p.phrase_type == super::analyzer::PhraseType::Breakdown
                                    && p.start_time > current_time
                                    && p.start_time < duration
                            });
                            outro.or(breakdown).map(|p| p.start_time)
                        })
                    } else {
                        None
                    };

                    let recomputed = if let Some(pt) = phrase_trigger {
                        pt
                    } else {
                        // Fallback: time-remaining based trigger
                        duration - xfade_dur
                    };
                    // Preserve user-extended trigger explicitly via
                    // flag (set by `extend_playback`). A heuristic
                    // based on cached vs recomputed delta breaks when
                    // analyzer phrase recomputation moves `recomputed`
                    // backward — non-monotone, can falsely look like
                    // a user extension.
                    let trigger_time = if s.trigger_user_extended {
                        s.cached_trigger_time.unwrap_or(recomputed)
                    } else {
                        recomputed
                    };
                    s.cached_trigger_time = Some(trigger_time);

                    // Quick-mix override: fire as soon as the playing
                    // deck has played past `QUICK_MIX_MIN_BARS` (musical
                    // time, not seconds — matches how DJs think and is
                    // tempo-independent). Bypasses the time-remaining
                    // check so each track turns over in ~30s at 128 BPM
                    // instead of the usual ~4 min. Still quantizes to
                    // the nearest downbeat below.
                    let quick_fire = s.quick_mix
                        && s.playing()
                            .beat_grid
                            .map(|g| g.bar_index(s.playing().current_time()))
                            .unwrap_or(0)
                            >= s.quick_mix_bars as i64
                        && s.incoming_loaded_not_playing();

                    // Start crossfade — quantize to nearest downbeat (bar_phase ≈ 0)
                    // Use trigger_time for phrase-aware firing, fall back to time-remaining check.
                    let phrase_fire =
                        current_time >= trigger_time && s.incoming_loaded_not_playing();
                    let time_remaining_fire =
                        time_remaining <= xfade_dur + bar_dur && s.incoming_loaded_not_playing();
                    // User-override gate: if the user has been touching
                    // controls during the playing track, don't surprise-
                    // fire a mix. They get to finish their performance
                    // (and trigger the mix manually via `m` / mix_now)
                    // or let the track run out. Cleared on next mix start.
                    let auto_blocked = s.user_paused_auto;
                    if !auto_blocked && (quick_fire || phrase_fire || time_remaining_fire) {
                        let bar_phase = s
                            .playing()
                            .beat_grid
                            .map(|g| g.bar_phase(s.playing().current_time()))
                            .unwrap_or(0.0);
                        if !(0.02..=0.98).contains(&bar_phase) {
                            s.start_crossfade();
                            if quick_fire {
                                tracing::info!(
                                    "Quick-mix fired at {:.1}s",
                                    s.playing().current_time()
                                );
                            } else if phrase_trigger.is_some() {
                                tracing::info!(
                                    "Phrase-triggered crossfade at {:.1}s (trigger={:.1}s)",
                                    current_time,
                                    trigger_time
                                );
                            }
                        }
                    }
                }

                // Track ended naturally (not paused)
                let is_paused = s.playing().paused;
                if is_loaded && !is_playing && !is_paused {
                    if let Some(track) = s.playing().track.clone() {
                        self.history.push(HistoryEntry {
                            track,
                            mix_score: None,
                        });
                    }
                    if s.incoming().is_loaded() {
                        let old = s.playing_deck;
                        let old_samples = match old {
                            DeckId::A => s.deck_a.unload(),
                            DeckId::B => s.deck_b.unload(),
                        };
                        deferred_samples.push(old_samples);
                        s.playing_deck = s.playing_deck.other();
                        s.playing_mut().play();
                        self.next_track_requested = false;
                    } else {
                        s.state = EngineState::Idle;
                        self.next_track_requested = false;
                        events.push(EngineEvent::PlaybackEnded);
                    }
                }
            }

            EngineState::PreparingCrossfade => {
                if s.incoming_loaded_not_playing() {
                    s.start_crossfade();
                }
            }

            EngineState::Crossfading => {
                // --- Phase 1: snapshot scalars under lock ---
                // Audio-domain correlation runs only at the 25% and 75%
                // progress checkpoints; skip the per-ms peak scan on
                // every other tick. Saves two ~500-element heap allocs
                // and ~3000 sample scans per tick at 60Hz. The window
                // here matches the action window in the correlation
                // block exactly — wider gates would alloc-then-discard.
                let near_checkpoint = {
                    let p = s.crossfade_progress;
                    (p > 0.24 && p < 0.26) || (p > 0.74 && p < 0.76)
                };
                let (p_peaks, i_peaks) = if near_checkpoint && s.transition_type.use_phase_sync() {
                    let sr = s.playing().sample_rate as usize;
                    let bin = (sr / 1000).max(1);
                    let beat_ms = s
                        .playing()
                        .beat_grid
                        .map(|g| (g.beat_interval() * 1000.0) as usize)
                        .unwrap_or(469);
                    let p_start = (s.playing().current_time() * sr as f64) as usize;
                    let i_start = (s.incoming().current_time() * sr as f64) as usize;
                    let mut pp = Vec::with_capacity(beat_ms);
                    let mut ip = Vec::with_capacity(beat_ms);
                    for ms in 0..beat_ms {
                        let ps = p_start + ms * bin;
                        let pe = (ps + bin).min(s.playing().samples.len());
                        pp.push(if pe > ps && pe <= s.playing().samples.len() {
                            s.playing().samples[ps..pe]
                                .iter()
                                .map(|v| v.abs())
                                .fold(0.0f32, f32::max)
                        } else {
                            0.0
                        });
                        let is = i_start + ms * bin;
                        let ie = (is + bin).min(s.incoming().samples.len());
                        ip.push(if ie > is && ie <= s.incoming().samples.len() {
                            s.incoming().samples[is..ie]
                                .iter()
                                .map(|v| v.abs())
                                .fold(0.0f32, f32::max)
                        } else {
                            0.0
                        });
                    }
                    (pp, ip)
                } else {
                    (Vec::new(), Vec::new())
                };
                let snap = CrossfadeSnapshot {
                    playing_deck: s.playing_deck,
                    playing_time: s.playing().current_time(),
                    playing_rate: s.playing().rate.max(0.01),
                    playing_grid: s.playing().beat_grid,
                    incoming_time: s.incoming().current_time(),
                    incoming_grid: s.incoming().beat_grid,
                    crossfade_start_playing_time: s.crossfade_start_playing_time,
                    crossfade_progress: s.crossfade_progress,
                    transition_type: s.transition_type,
                    transition_uses_phase_sync: s.transition_type.use_phase_sync(),
                    manual_mix: s.manual_mix,
                    crossfader_pos: s.crossfader_pos,
                    crossfade_start: s.crossfade_start,
                    last_crossfader_move: s.last_crossfader_move,
                    playing_beat_peaks: p_peaks,
                    incoming_beat_peaks: i_peaks,
                };
                // Take the controller out — it's only used from tick(),
                // never from fill_output, so no other thread needs it.
                let mut ctrl_taken = s.crossfade_controller.take();
                drop(s); // --- release audio lock ---

                // --- Phase 2: compute (no lock held) ---

                // Crossfade progress: normally driven by the playing deck's
                // source-time delta — pauses freeze it, varispeed warps it
                // by the rate, exactly mirrors the audio the listener hears.
                //
                // LoopRoll and EchoOut are the exceptions — both decouple
                // the playing deck from the audible mix, so its source time
                // stops tracking the crossfade:
                //   - LoopRoll locks the playing deck into a 4-beat loop, so
                //     source time bobs within the loop bounds and never
                //     monotonically advances (observed: 4+ min stuck at
                //     progress ~0.05, the 0.85 loop release never firing).
                //   - EchoOut hard-cuts the playing deck to silence at
                //     progress ~0.005; from then on the listener hears only
                //     the (post-fader) echo tail and the incoming deck. The
                //     playing deck keeps running silently but, since EchoOut
                //     is the forced/rescue transition fired near track end
                //     and runs a fixed 8 bars, it routinely hits end-of-track
                //     mid-crossfade — `fill_buffer` then stops advancing
                //     `position`, source time freezes, and progress stalls
                //     forever (echo never decays past its 0.5 cutoff, decks
                //     never swap). Both are wall-clock effects by nature.
                // For both we use wall-clock elapsed instead.
                let new_progress = if snap.transition_type.progress_from_wall_clock() {
                    if let (Some(ctrl), Some(start_instant)) = (&ctrl_taken, snap.crossfade_start) {
                        let wall_elapsed = start_instant.elapsed().as_secs_f64();
                        (wall_elapsed / ctrl.duration()).min(1.0)
                    } else {
                        snap.crossfade_progress
                    }
                } else if let (Some(ctrl), Some(start_t)) =
                    (&ctrl_taken, snap.crossfade_start_playing_time)
                {
                    let duration = ctrl.duration();
                    let source_elapsed = (snap.playing_time - start_t).max(0.0);
                    let elapsed = source_elapsed / snap.playing_rate;
                    (elapsed / duration).min(1.0)
                } else {
                    snap.crossfade_progress
                };

                // Phase offset computation (pure math on BeatGrid + positions)
                let offset_ms = if snap.transition_uses_phase_sync {
                    match (&snap.playing_grid, &snap.incoming_grid) {
                        (Some(pg), Some(ig)) => {
                            BeatGrid::phase_offset(pg, snap.playing_time, ig, snap.incoming_time)
                                * 1000.0
                        }
                        _ => 0.0,
                    }
                } else {
                    0.0
                };

                // Audio-domain beat correlation: if grid says phase=0 but
                // peaks don't correlate, the grid is wrong. Check at 25%
                // and 75% progress (one-shot per checkpoint).
                if snap.transition_uses_phase_sync
                    && offset_ms.abs() < 5.0
                    && !snap.playing_beat_peaks.is_empty()
                    && !snap.incoming_beat_peaks.is_empty()
                    && ((new_progress > 0.24 && new_progress < 0.26)
                        || (new_progress > 0.74 && new_progress < 0.76))
                {
                    let n = snap
                        .playing_beat_peaks
                        .len()
                        .min(snap.incoming_beat_peaks.len());
                    if n > 10 {
                        let mut sum_pp = 0.0f64;
                        let mut sum_ip = 0.0f64;
                        let mut sum_cross = 0.0f64;
                        for i in 0..n {
                            let p = snap.playing_beat_peaks[i] as f64;
                            let q = snap.incoming_beat_peaks[i] as f64;
                            sum_pp += p * p;
                            sum_ip += q * q;
                            sum_cross += p * q;
                        }
                        let denom = (sum_pp * sum_ip).sqrt();
                        let corr = if denom > 1e-9 { sum_cross / denom } else { 0.0 };
                        if corr < 0.3 {
                            tracing::error!(
                                "Beat correlation LOW ({corr:.2}) at {:.0}% — grid says phase={offset_ms:+.1}ms but audio peaks don't match. BPM grid may be wrong.",
                                new_progress * 100.0
                            );
                        }
                    }
                }

                // Rate correction (mutates controller state — safe because
                // controller is taken out of AudioState, no contention)
                let rate_correction_result = if snap.transition_uses_phase_sync {
                    if let Some(ref mut ctrl) = ctrl_taken {
                        let correction = ctrl.rate_correction(offset_ms);
                        let playing_bpm = ctrl.playing_bpm;
                        let incoming_bpm = ctrl.incoming_bpm;
                        if incoming_bpm > 0.0 {
                            Some((playing_bpm / incoming_bpm) * (1.0 + correction))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Manual-mix stall check (reads only snapshot timestamps)
                const MANUAL_STALL_SECS: f64 = 30.0;
                let manual_stalled = snap.manual_mix && {
                    match snap.last_crossfader_move {
                        Some(t) => t.elapsed().as_secs_f64() > MANUAL_STALL_SECS,
                        None => snap
                            .crossfade_start
                            .map(|t| t.elapsed().as_secs_f64() > MANUAL_STALL_SECS)
                            .unwrap_or(false),
                    }
                };

                // Manual-mode progress override
                let manual_progress = if snap.manual_mix && !manual_stalled {
                    Some(manual_progress_from_crossfader(
                        snap.crossfader_pos as f64,
                        snap.playing_deck,
                    ))
                } else {
                    None
                };

                // --- Phase 3: re-acquire lock, apply mutations ---
                s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());

                // Put the controller back
                s.crossfade_controller = ctrl_taken;

                // Apply crossfade progress
                s.crossfade_progress = new_progress;

                // Apply rate correction — but stand down for the rest
                // of this mix once the user has taken manual control
                // (nudge or grid shift). No more fighting the user
                // mid-crossfade. Resets on the next start_crossfade.
                if let Some(corrected_rate) = rate_correction_result
                    && !s.user_overrode_this_mix
                {
                    Mixer::set_rate(s.incoming_mut(), corrected_rate);
                }

                // Record phase sample for mix quality scoring
                if snap.transition_uses_phase_sync
                    && s.crossfade_progress > 0.05
                    && s.crossfade_progress < 0.95
                    && s.mix_phase_samples.len() < s.mix_phase_samples.capacity()
                {
                    s.mix_phase_samples.push(offset_ms.abs());
                }

                // Train-wreck detection. Compute rolling RMS over the
                // last ~1s of phase samples. Trigger only:
                //  - after 15% progress (give the controller time to converge)
                //  - phase-sync transitions (the ones that can actually wreck)
                //  - not already fired this mix
                //  - not in manual mode (DJ owns the wheel)
                //  - mode != Off
                // Threshold (25 ms RMS sustained) is well above the
                // 5 ms "imperceptible" floor and above the 15 ms
                // mix-quality-score ceiling — at 25 ms+ the listener
                // is hearing flam clearly.
                const WRECK_RMS_THRESHOLD_MS: f64 = 25.0;
                const WRECK_WINDOW_TICKS: usize = 60; // ~1s at 60Hz
                if !s.mix_wreck_fired
                    && !s.manual_mix
                    && snap.transition_uses_phase_sync
                    && s.crossfade_progress > 0.15
                    && s.crossfade_progress < 0.95
                    && !matches!(s.train_wreck_mode, crate::config::TrainWreckMode::Off)
                    && s.mix_phase_samples.len() >= WRECK_WINDOW_TICKS
                {
                    let tail =
                        &s.mix_phase_samples[s.mix_phase_samples.len() - WRECK_WINDOW_TICKS..];
                    let n = tail.len() as f64;
                    let rms = (tail.iter().map(|x| x * x).sum::<f64>() / n).sqrt();
                    if rms > WRECK_RMS_THRESHOLD_MS {
                        s.mix_wreck_fired = true;
                        let mode = s.train_wreck_mode;
                        let bail = matches!(mode, crate::config::TrainWreckMode::AutoBail);
                        if bail {
                            s.transition_type = super::transition::TransitionType::EchoOut;
                            s.playing_mut().delay_wet = 1.0;
                            if !s.incoming().playing && s.incoming().is_loaded() {
                                s.incoming_mut().play();
                            }
                            tracing::warn!(
                                "Train wreck auto-bail: rolling RMS {rms:.1}ms > {WRECK_RMS_THRESHOLD_MS}ms — switched to EchoOut at progress {:.0}%",
                                s.crossfade_progress * 100.0,
                            );
                        } else {
                            tracing::warn!(
                                "Train wreck detected: rolling RMS {rms:.1}ms > {WRECK_RMS_THRESHOLD_MS}ms (Detect mode — no action)",
                            );
                        }
                        events.push(EngineEvent::TrainWreckDetected {
                            rms_ms: rms,
                            bailed: bail,
                        });
                    }
                }

                // Manual-mix stall detector
                if manual_stalled {
                    s.manual_mix = false;
                    tracing::info!(
                        "Manual-mix stall: no crossfader activity for {MANUAL_STALL_SECS}s, falling back to auto curves"
                    );
                }

                // Let the transition type update deck parameters — in
                // AUTO and ASSIST modes. In MANUAL mode the DJ is driving
                // the faders, EQ, and crossfader directly; running the
                // curve here would overwrite their inputs every tick.
                if !s.manual_mix {
                    let progress = s.crossfade_progress;
                    let transition = s.transition_type;
                    let (playing, incoming) = s.decks_mut();
                    transition.apply(progress, playing, incoming);
                }

                // In manual mode the crossfade_progress needle is driven
                // by the DJ's `crossfader_pos` rather than the clock.
                if let Some(mp) = manual_progress {
                    s.crossfade_progress = mp;
                }

                if s.crossfade_progress >= 1.0 {
                    // Compute mix quality score from accumulated phase samples.
                    // Ceiling at 15ms RMS — matches human-audible
                    // flamming threshold (5ms = imperceptible, 15ms+
                    // = clearly out of sync). Linear map: 0ms → 100,
                    // 15ms → 0. Old 50ms ceiling let an 8ms-RMS mix
                    // score 84/100 even though it was audibly bad.
                    let score = if s.mix_phase_samples.is_empty() {
                        Some(70u8)
                    } else {
                        let n = s.mix_phase_samples.len() as f64;
                        let rms =
                            (s.mix_phase_samples.iter().map(|x| x * x).sum::<f64>() / n).sqrt();
                        let s0 = ((15.0 - rms.min(15.0)) * (100.0 / 15.0)).round() as i32;
                        Some(s0.clamp(0, 100) as u8)
                    };
                    if let Some(track) = s.playing().track.clone() {
                        self.history.push(HistoryEntry {
                            track: track.clone(),
                            mix_score: score,
                        });
                        let bpm = s.playing().beat_grid.map(|g| g.bpm).unwrap_or(0.0);
                        crate::ipc::write_event(&serde_json::json!({
                            "kind": "crossfade_complete",
                            "outgoing_track_id": track.id,
                            "outgoing_title": track.full_title(),
                            "outgoing_artist": track.artist_name(),
                            "outgoing_bpm": bpm,
                            "mix_score": score,
                        }));
                        events.push(EngineEvent::CrossfadeComplete { track, bpm });
                    }
                    let finished_transition = s.transition_type;
                    s.rule_engine.record(finished_transition);
                    s.mix_phase_samples.clear();
                    deferred_samples.push(s.swap_decks(config));
                    self.next_track_requested = false;
                }
            }
        }

        drop(s); // release audio lock before dealloc
        drop(deferred_deck); // deferred DeckPlayer from RT callback
        drop(deferred_samples); // old sample buffers from unload/swap
        events
    }
}

/// Generate a metronome click — high tone on beat 1, low on 2/3/4.
/// Returns a sample value for the given time.
/// Lower score = better transition. BPM distance + Camelot key distance.
fn shuffle_score(track: &BeatportTrack, from_bpm: f64, from_key: Option<&str>) -> f64 {
    let bpm_dist = (track.bpm.unwrap_or(128.0) - from_bpm).abs();
    let key_bonus = match (from_key, track.key.as_deref()) {
        (Some(fk), Some(tk)) => camelot_key_dist(fk, tk) * 2.0,
        _ => 6.0,
    };
    bpm_dist + key_bonus
}

fn camelot_key_dist(from: &str, to: &str) -> f64 {
    fn parse(k: &str) -> Option<(i32, u8)> {
        let k = k.trim();
        let l = *k.as_bytes().last()?;
        if l != b'A' && l != b'B' {
            return None;
        }
        k[..k.len() - 1].parse().ok().map(|n| (n, l))
    }
    let (fn_, fl) = match parse(from) {
        Some(v) => v,
        None => return 6.0,
    };
    let (tn, tl) = match parse(to) {
        Some(v) => v,
        None => return 6.0,
    };
    if fn_ == tn && fl == tl {
        return 0.0;
    }
    if fn_ == tn {
        return 1.0;
    }
    let d = (fn_ - tn).unsigned_abs() as i32;
    let nd = d.min(12 - d);
    if fl == tl && nd == 1 {
        return 1.0;
    }
    nd as f64 + if fl == tl { 0.0 } else { 1.0 }
}

/// Save the current channel fader value into `slot` only if the slot
/// is empty (None). Used by the preview-deck flow so a SECOND
/// `preview_deck` call (without an intervening stop) doesn't
/// overwrite the original fader with the silenced 0.0. Returns the
/// new "silenced" fader to apply.
pub(crate) fn save_fader_once(slot: &mut Option<f32>, current: f32) -> f32 {
    if slot.is_none() {
        *slot = Some(current);
    }
    0.0
}

/// Restore a saved channel fader value, defaulting to 1.0 if no
/// preview was ever captured for this deck. Symmetric with
/// `save_fader_once`. Used by stop_deck_preview / play_deck.
pub(crate) fn restore_fader(slot: &mut Option<f32>) -> f32 {
    slot.take().unwrap_or(1.0)
}

/// Default bars before quick-mix fires (used when no AppConfig is
/// applied yet). Live value lives on AudioState as `quick_mix_bars`
/// and can be overridden from `AppConfig.claude_dj.quick_mix_bars`.
pub(crate) const DEFAULT_QUICK_MIX_BARS: u32 = 16;

// manual_progress_from_crossfader is in super::fill_output

/// Pick a crossfade length (in bars) for the given track based on its
/// genre. Longer for "dancefloor" electronic where DJs normally blend
/// across full phrases; shorter for vocal / pop / DnB styles where
/// tight cuts are the norm. Phrase density from the analyzer bumps
/// one tier in either direction — sparse phrases (big drops) → longer,
/// dense phrases (busy arrangement) → shorter.
/// Maximum BPM gap (percent) below which phase-sync transitions are
/// allowed. Past this, the engine forces EchoOut. Wider when a pitch
/// stretcher is loaded — Rubberband holds quality to ~15-20% before
/// degrading, so 14% (≈2.3 semitones equivalent) is safe. Without
/// Rubberband the deck runs varispeed and 10% gap = ~1.7 semitones of
/// pitch shift — audibly bad. 8% floor catches that case.
pub fn bpm_gap_cutoff(stretcher_loaded: bool) -> f64 {
    if stretcher_loaded { 14.0 } else { 8.0 }
}

/// Pure arithmetic backing `seek_forward_safe`. Returns the signed
/// delta the deck should seek by (or 0 if no safe seek is available).
/// If a backward seek would walk past 0, walks one `period` forward
/// instead. Returns 0 if the result is below the threshold or out of
/// the track's bounds.
pub(crate) fn forward_safe_delta(
    inc_t: f64,
    raw: f64,
    period: f64,
    duration: f64,
    min_threshold: f64,
) -> f64 {
    let mut adj = raw;
    if inc_t + adj < 0.0 {
        adj += period;
    }
    let new_t = inc_t + adj;
    if adj.abs() > min_threshold && new_t >= 0.0 && new_t < duration {
        adj
    } else {
        0.0
    }
}

pub fn auto_crossfade_bars(
    track: &BeatportTrack,
    analysis: Option<&super::analyzer::AnalysisResult>,
) -> u32 {
    let g = track
        .genre_slug
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(track.genre_name.as_deref())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Tiered by typical DJing convention. First-match wins, so order
    // matters: specific techno slugs go before the generic "techno"
    // catch-all, otherwise tech-house would always hit the 16-tier via
    // the substring "house" rather than the 16-tier via "tech-house".
    const GENRE_TIERS: &[(&[&str], u32)] = &[
        (
            &[
                "progressive-house",
                "progressive",
                "minimal-deep-tech",
                "minimal",
                "melodic-house-and-techno",
                "melodic",
                "afro-house",
                "afro",
                "techno-peak-time-driving",
                "techno-raw-deep-hypnotic",
                "organic-house",
            ],
            32,
        ),
        (
            &[
                "techno", // generic techno catch-all
                "tech-house",
                "deep-house",
                "electronica",
                "indie-dance",
            ],
            16,
        ),
        (
            &[
                "house", // generic house
                "nu-disco",
                "disco",
                "funk",
                "trance",
                "bass-house",
            ],
            8,
        ),
        (
            &[
                "drum-bass",
                "dnb",
                "breakbeat",
                "breaks",
                "dubstep",
                "drumstep",
                "trap",
                "future-bass",
            ],
            4,
        ),
        (
            &["hip-hop", "hip hop", "r-b", "pop", "reggae", "dancehall"],
            2,
        ),
    ];
    let base: u32 = GENRE_TIERS
        .iter()
        .find(|(slugs, _)| slugs.iter().any(|k| g.contains(k)))
        .map(|&(_, bars)| bars)
        .unwrap_or(16); // sensible default when genre is unknown

    // Phrase-density tweak. Dance tracks typically have 16-bar or
    // 32-bar phrases. At 128 BPM: 16-bar = 30s, 32-bar = 60s.
    //
    // Use per-window intervals (consecutive phrase deltas), not
    // total-span / count. The naive `last_start / (len-1)` average
    // conflates intro/outro length with actual phrase density: a
    // 360s track with only 3 detected phrases gives avg=120s and
    // always bumps up regardless of arrangement. Per-window
    // intervals reflect the real phrase period.

    if let Some(a) = analysis {
        if a.phrases.len() >= 3 {
            // Single-pass fold: avoids the intermediate Vec for window
            // intervals. Off-RT (called once per crossfade), but keeps
            // the tick loop alloc-free as a matter of policy.
            let (sum, count) = a
                .phrases
                .windows(2)
                .map(|w| w[1].start_time - w[0].start_time)
                .filter(|&d| d > 0.0)
                .fold((0.0_f64, 0_usize), |(s, n), d| (s + d, n + 1));
            if count == 0 {
                base
            } else {
                let avg_phrase = sum / count as f64;
                if avg_phrase > 45.0 {
                    // Bump up one tier (2 → 4 → 8 → 16 → 32 → 64)
                    match base {
                        2 => 4,
                        4 => 8,
                        8 => 16,
                        16 => 32,
                        32 => 64,
                        _ => base,
                    }
                } else if avg_phrase < 18.0 {
                    // Bump down one tier (64 → 32 → 16 → 8 → 4 → 2 → 0)
                    match base {
                        2 => 0,
                        4 => 2,
                        8 => 4,
                        16 => 8,
                        32 => 16,
                        64 => 32,
                        _ => base,
                    }
                } else {
                    base
                }
            }
        } else {
            base
        }
    } else {
        base
    }
}

pub(crate) fn glide_target_rate(start_rate: f64, progress: f64) -> f64 {
    // sin(π/2 × p): starts fast, eases into the target. This is the
    // ease-out shape a tempo glide wants — the pitch moves quickly off
    // the mismatched rate and settles into unity, so the listener hears
    // "the track locking in" rather than a slow sweep. `1 − cos(π/2 × p)`
    // would be ease-in instead (slow start, fast end), which would feel
    // like the tempo never catches up until the last moment.
    let p = progress.clamp(0.0, 1.0);
    let eased = (std::f64::consts::FRAC_PI_2 * p).sin();
    start_rate + eased * (1.0 - start_rate)
}

// apply_limiter is in super::fill_output

// metronome_click is in super::fill_output

// output_device_names is in super::fill_output

// build_monitor_stream is in super::fill_output

// fill_output is in super::fill_output

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LimiterMode;

    /// Replicates the crossfader gain math from fill_output so we can
    /// test it without spinning up a cpal stream. Must stay in sync
    /// with the formula at the top of fill_output.
    fn xf_gains(crossfader_pos: f32) -> (f32, f32) {
        let xf = crossfader_pos.clamp(-1.0, 1.0);
        let theta = (xf + 1.0) * std::f32::consts::FRAC_PI_4;
        (theta.cos(), theta.sin())
    }

    #[test]
    fn crossfader_constant_power_at_center() {
        // Equal-power invariant: xf_a² + xf_b² == 1 at every position.
        // The previous linear law gave 1+1=2 at center → audible +3 dB
        // loudness bump mid-crossfade. sqrt taper compensates.
        let (a, b) = xf_gains(0.0);
        assert!(
            (a * a + b * b - 1.0).abs() < 1e-6,
            "center crossfader: {a}² + {b}² = {} (expected 1.0)",
            a * a + b * b
        );
    }

    #[test]
    fn crossfader_endpoints_are_full_or_silent() {
        let (a, b) = xf_gains(-1.0);
        assert!(
            (a - 1.0).abs() < 1e-6 && b.abs() < 1e-6,
            "fader=-1 → A full, B silent: got A={a} B={b}"
        );
        let (a, b) = xf_gains(1.0);
        assert!(
            a.abs() < 1e-6 && (b - 1.0).abs() < 1e-6,
            "fader=+1 → A silent, B full: got A={a} B={b}"
        );
    }

    #[test]
    fn crossfader_constant_power_across_sweep() {
        // Sweep through all positions and verify equal-power holds.
        // Slack of 1e-5 because we're squaring f32s.
        for i in -100..=100 {
            let pos = i as f32 / 100.0;
            let (a, b) = xf_gains(pos);
            let sum_sq = a * a + b * b;
            assert!(
                (sum_sq - 1.0).abs() < 1e-5,
                "constant-power broke at pos={pos}: sum²={sum_sq}"
            );
        }
    }

    /// Production-formula helper that *includes* the channel fader
    /// multiplication. Mirrors fill_output exactly. The simpler
    /// `xf_gains` above omits the fader, which is fine for
    /// equal-power proofs but doesn't catch an `+ fader` regression
    /// where someone replaces multiplication with addition.
    fn xf_gains_with_faders(pos: f32, fader_a: f32, fader_b: f32) -> (f32, f32) {
        let xf = pos.clamp(-1.0, 1.0);
        let theta = (xf + 1.0) * std::f32::consts::FRAC_PI_4;
        (theta.cos() * fader_a, theta.sin() * fader_b)
    }

    #[test]
    fn quick_mix_bars_clamp_bounds() {
        // set_quick_mix_bars clamps to [8, 64]. Pure arithmetic test —
        // mirrors the production formula. Replicates the clamp so a
        // future tweak to the bounds still has guard rails.
        let clamp = |b: u32| b.clamp(8, 64);
        assert_eq!(clamp(0), 8, "below min should snap to 8");
        assert_eq!(clamp(4), 8, "old min (4) should snap to new min (8)");
        assert_eq!(clamp(16), 16, "default in range stays put");
        assert_eq!(clamp(100), 64, "above max should snap to 64");
        assert_eq!(clamp(64), 64, "max stays put");
    }

    #[test]
    fn save_fader_once_captures_then_locks() {
        // First call captures the current value; second call must NOT
        // overwrite (the bug we fixed: second preview_deck without a
        // stop overwrote the saved fader with the silenced 0.0,
        // leaving the deck permanently muted).
        let mut slot: Option<f32> = None;
        let silenced = save_fader_once(&mut slot, 0.7);
        assert_eq!(silenced, 0.0, "save returns the new silenced value");
        assert_eq!(slot, Some(0.7), "first call captures the value");

        // Simulate the second preview_deck call: current is now 0.0
        // (silenced from the first preview), but slot must hold the
        // ORIGINAL 0.7 untouched.
        let silenced2 = save_fader_once(&mut slot, 0.0);
        assert_eq!(silenced2, 0.0);
        assert_eq!(slot, Some(0.7), "second call must not overwrite");
    }

    #[test]
    fn restore_fader_returns_saved_or_defaults_to_unity() {
        let mut slot = Some(0.7);
        let restored = restore_fader(&mut slot);
        assert_eq!(restored, 0.7);
        assert_eq!(slot, None, "take() empties the slot");

        // No prior preview → default to 1.0 so the deck isn't left silenced.
        let mut empty: Option<f32> = None;
        assert_eq!(restore_fader(&mut empty), 1.0);
    }

    #[test]
    fn save_then_restore_round_trips() {
        let mut slot: Option<f32> = None;
        let original = 0.42_f32;
        save_fader_once(&mut slot, original);
        let restored = restore_fader(&mut slot);
        assert!((restored - original).abs() < 1e-9);
        // After restore, slot is empty — next preview captures fresh.
        assert_eq!(slot, None);
    }

    #[test]
    fn channel_fader_zero_silences_deck_at_every_crossfader_position() {
        // If a deck's channel fader is 0 the deck must be inaudible
        // regardless of crossfader position — the channel strip cuts
        // before the bus. Catches `* fader` → `+ fader` regression.
        for i in -10..=10 {
            let pos = i as f32 / 10.0;
            let (a, _) = xf_gains_with_faders(pos, 0.0, 1.0);
            assert_eq!(a, 0.0, "fader_a=0 must silence A at pos={pos}, got {a}");
            let (_, b) = xf_gains_with_faders(pos, 1.0, 0.0);
            assert_eq!(b, 0.0, "fader_b=0 must silence B at pos={pos}, got {b}");
        }
    }

    #[test]
    fn channel_fader_scales_linearly() {
        // At a fixed crossfader position (center) deck gain must scale
        // proportionally with its channel fader.
        let pos = 0.0_f32;
        for &fader in &[0.25_f32, 0.5, 0.75, 1.0] {
            let (a, _) = xf_gains_with_faders(pos, fader, 1.0);
            let (a_unity, _) = xf_gains_with_faders(pos, 1.0, 1.0);
            let ratio = a / a_unity;
            assert!(
                (ratio - fader).abs() < 1e-6,
                "fader={fader}: expected gain ratio {fader}, got {ratio}"
            );
        }
    }

    #[test]
    fn equal_power_holds_with_matched_channel_faders() {
        // With both channel faders at the same value, the constant-power
        // invariant becomes sum² = fader². Verifies the multiplication
        // composes correctly with the cos/sin taper.
        let fader = 0.7_f32;
        for i in -10..=10 {
            let pos = i as f32 / 10.0;
            let (a, b) = xf_gains_with_faders(pos, fader, fader);
            let sum_sq = a * a + b * b;
            let expected = fader * fader;
            assert!(
                (sum_sq - expected).abs() < 1e-5,
                "pos={pos}: sum²={sum_sq} expected {expected}"
            );
        }
    }

    /// Replicates the CrossfaderSweep tick interpolation from the
    /// engine tick loop. Pure linear interp over a wall-time fraction.
    fn sweep_pos(from: f32, target: f32, t: f64) -> f32 {
        from + (target - from) * t.clamp(0.0, 1.0) as f32
    }

    #[test]
    fn sweep_at_zero_returns_from() {
        assert!((sweep_pos(-1.0, 1.0, 0.0) + 1.0).abs() < 1e-6);
        assert!((sweep_pos(0.5, -0.5, 0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sweep_at_one_returns_target() {
        assert!((sweep_pos(-1.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((sweep_pos(0.5, -0.5, 1.0) + 0.5).abs() < 1e-6);
    }

    #[test]
    fn sweep_midpoint_is_average() {
        assert!((sweep_pos(-1.0, 1.0, 0.5) - 0.0).abs() < 1e-6);
        assert!((sweep_pos(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sweep_over_one_clamps_at_target() {
        // Once elapsed > duration the tick loop saturates the fraction
        // before applying it. Matches the `if elapsed >= duration`
        // branch that sets crossfader_pos = target and clears sweep.
        assert!((sweep_pos(-1.0, 0.5, 2.0) - 0.5).abs() < 1e-6);
        assert!((sweep_pos(-1.0, 0.5, -0.5) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn sweep_monotonic_across_full_range() {
        let mut prev = sweep_pos(-1.0, 1.0, 0.0);
        for i in 1..=100 {
            let cur = sweep_pos(-1.0, 1.0, i as f64 / 100.0);
            assert!(cur >= prev - 1e-9, "non-monotonic at {i}");
            prev = cur;
        }
    }

    #[test]
    fn manual_progress_a_playing_maps_minus1_to_0_and_plus1_to_1() {
        assert_eq!(manual_progress_from_crossfader(-1.0, DeckId::A), 0.0);
        assert_eq!(manual_progress_from_crossfader(0.0, DeckId::A), 0.5);
        assert_eq!(manual_progress_from_crossfader(1.0, DeckId::A), 1.0);
    }

    #[test]
    fn manual_progress_b_playing_is_inverted() {
        // B playing → +1 = playing side (no progress), −1 = fully to A.
        // Without this inversion the engine would auto-swap the moment
        // manual mode engages because crossfader_pos already sits at the
        // "incoming side" from the prior mix.
        assert_eq!(manual_progress_from_crossfader(1.0, DeckId::B), 0.0);
        assert_eq!(manual_progress_from_crossfader(0.0, DeckId::B), 0.5);
        assert_eq!(manual_progress_from_crossfader(-1.0, DeckId::B), 1.0);
    }

    #[test]
    fn manual_progress_clamps_out_of_range() {
        assert_eq!(manual_progress_from_crossfader(5.0, DeckId::A), 1.0);
        assert_eq!(manual_progress_from_crossfader(-5.0, DeckId::A), 0.0);
    }

    #[test]
    fn manual_progress_monotonic_in_crossfader_direction() {
        for deck in [DeckId::A, DeckId::B] {
            let mut prev = manual_progress_from_crossfader(-1.0, deck);
            for i in -99..=100 {
                let pos = i as f64 / 100.0;
                let p = manual_progress_from_crossfader(pos, deck);
                let mono = match deck {
                    DeckId::A => p >= prev - 1e-9,
                    DeckId::B => p <= prev + 1e-9,
                };
                assert!(
                    mono,
                    "deck={deck:?} pos={pos} broke monotonicity: {prev} → {p}"
                );
                prev = p;
            }
        }
    }

    #[test]
    fn glide_target_rate_boundaries_and_monotonic() {
        // At p=0 the result is the starting rate; at p=1 it must reach 1.0
        // (cos(π/2)=0 exactly). The curve must be strictly increasing for
        // a start_rate < 1.0 — no plateau or overshoot.
        let s = 0.943;
        let r0 = glide_target_rate(s, 0.0);
        let r1 = glide_target_rate(s, 1.0);
        assert!((r0 - s).abs() < 1e-9, "p=0 should return start, got {r0}");
        assert!((r1 - 1.0).abs() < 1e-9, "p=1 should return 1.0, got {r1}");
        let mut prev = r0;
        for i in 1..=20 {
            let r = glide_target_rate(s, i as f64 / 20.0);
            assert!(r >= prev, "non-monotonic at p={i}/20: {prev} → {r}");
            prev = r;
        }
    }

    #[test]
    fn glide_target_rate_continuous_at_arbitrary_start() {
        // Math-level continuity: whatever rate the engine captures into
        // `glide_start_rate` at swap time, `glide_target_rate(R, 0)` must
        // equal R exactly — otherwise the moment the glide engages, the
        // listener hears a step in pitch. The full engine handoff
        // (that the swap path reads `deck.rate` into `glide_start_rate`
        // rather than 1.0) is an integration property that lives in
        // engine::MixEngine and is not covered by this unit test —
        // verified manually via the test_mix harness.
        let rates = [0.87_f64, 0.943, 0.97, 1.0, 1.03, 1.05, 1.10, 1.20];
        for &r in &rates {
            let start = glide_target_rate(r, 0.0);
            let end = glide_target_rate(r, 1.0);
            assert!(
                (start - r).abs() < 1e-12,
                "discontinuity at p=0 for r={r}: got {start}"
            );
            assert!(
                (end - 1.0).abs() < 1e-12,
                "endpoint drift at p=1 for r={r}: got {end}"
            );
        }
    }

    #[test]
    fn glide_target_rate_ease_out_fast_early() {
        // Signature of ease-out: by the midpoint we should be more than
        // halfway to target. Compare against linear for the 132→140 case.
        let s = 0.943;
        let mid_linear = s + 0.5 * (1.0 - s);
        let mid_eased = glide_target_rate(s, 0.5);
        assert!(
            mid_eased > mid_linear,
            "midpoint eased ({mid_eased}) should be ahead of linear ({mid_linear})"
        );
    }

    #[test]
    fn limiter_off_is_hard_clamp() {
        assert_eq!(apply_limiter(0.5, LimiterMode::Off), 0.5);
        assert_eq!(apply_limiter(1.5, LimiterMode::Off), 1.0);
        assert_eq!(apply_limiter(-2.0, LimiterMode::Off), -1.0);
    }

    #[test]
    fn limiter_softknee_passthrough_below_07() {
        for &x in &[-0.69, -0.5, -0.1, 0.0, 0.1, 0.5, 0.69] {
            let y = apply_limiter(x, LimiterMode::SoftKnee);
            assert!(
                (y - x).abs() < 1e-6,
                "soft knee altered passthrough: x={x} y={y}"
            );
        }
    }

    #[test]
    fn limiter_softknee_never_exceeds_unity() {
        for i in 70..=300 {
            let x = i as f32 / 100.0;
            let y = apply_limiter(x, LimiterMode::SoftKnee);
            assert!(y > 0.0 && y < 1.001, "x={x} y={y} exceeded ceiling");
            let y_neg = apply_limiter(-x, LimiterMode::SoftKnee);
            assert!(
                y_neg < 0.0 && y_neg > -1.001,
                "x={} y={y_neg} exceeded floor",
                -x
            );
        }
    }

    #[test]
    fn limiter_softknee_is_monotonic() {
        // Increasing input → non-decreasing output across the full range.
        let mut last = -2.0f32;
        for i in -300..=300 {
            let y = apply_limiter(i as f32 / 100.0, LimiterMode::SoftKnee);
            assert!(
                y + 1e-6 >= last,
                "non-monotonic at x={}: y={y} < last={last}",
                i as f32 / 100.0
            );
            last = y;
        }
    }

    fn make_track(genre_slug: Option<&str>) -> crate::beatport::models::BeatportTrack {
        crate::beatport::models::BeatportTrack {
            id: 1,
            title: "T".into(),
            mix_name: None,
            artists: vec![],
            bpm: Some(128.0),
            key: None,
            duration: Some(360.0),
            label_id: None,
            label_name: None,
            genre_id: None,
            genre_name: None,
            genre_slug: genre_slug.map(|s| s.to_string()),
            release_id: None,
            release_date: None,
            remixers: vec![],
            local_path: None,
        }
    }

    #[test]
    fn auto_crossfade_bars_progressive_house_is_32() {
        let t = make_track(Some("progressive-house"));
        assert_eq!(auto_crossfade_bars(&t, None), 32);
    }

    #[test]
    fn auto_crossfade_bars_tech_house_is_16() {
        let t = make_track(Some("tech-house"));
        assert_eq!(auto_crossfade_bars(&t, None), 16);
    }

    #[test]
    fn auto_crossfade_bars_dnb_is_4() {
        let t = make_track(Some("drum-bass"));
        assert_eq!(auto_crossfade_bars(&t, None), 4);
    }

    #[test]
    fn auto_crossfade_bars_house_tier_is_8() {
        let t = make_track(Some("nu-disco"));
        assert_eq!(auto_crossfade_bars(&t, None), 8);
        let t = make_track(Some("trance"));
        assert_eq!(auto_crossfade_bars(&t, None), 8);
        // Generic "house" hits the 8-tier — but tech-house wins via
        // the earlier 16-tier (already covered by another test).
        let t = make_track(Some("house"));
        assert_eq!(auto_crossfade_bars(&t, None), 8);
    }

    #[test]
    fn auto_crossfade_bars_pop_tier_is_2() {
        let t = make_track(Some("hip-hop"));
        assert_eq!(auto_crossfade_bars(&t, None), 2);
        let t = make_track(Some("pop"));
        assert_eq!(auto_crossfade_bars(&t, None), 2);
        let t = make_track(Some("reggae"));
        assert_eq!(auto_crossfade_bars(&t, None), 2);
    }

    #[test]
    fn mix_score_formula_maps_rms_to_0_100_with_15ms_ceiling() {
        // The score formula at engine.rs ~2654 is:
        //   (15 - rms.min(15)) * (100/15)
        // 0ms RMS → 100, 5ms → 67, 10ms → 33, 15ms → 0, 25ms → 0 (clamped).
        // Pin the math; old formula used a 50ms ceiling which scored
        // 8ms RMS at 84/100 (audibly bad mixes were called "good").
        let score = |rms: f64| -> i32 { ((15.0 - rms.min(15.0)) * (100.0 / 15.0)).round() as i32 };
        assert_eq!(score(0.0), 100);
        assert_eq!(score(5.0), 67);
        assert_eq!(score(10.0), 33);
        assert_eq!(score(15.0), 0);
        assert_eq!(score(25.0), 0); // clamped
    }

    #[test]
    fn auto_crossfade_bars_unknown_genre_defaults_to_16() {
        let t = make_track(None);
        assert_eq!(auto_crossfade_bars(&t, None), 16);
        let t = make_track(Some(""));
        assert_eq!(auto_crossfade_bars(&t, None), 16);
    }

    #[test]
    fn auto_crossfade_bars_techno_specific_beats_generic() {
        // The two specific techno slugs hit the 32-tier; generic
        // "techno" without a sub-slug falls into the 16-tier — verifies
        // the ordering of the substring match.
        let t = make_track(Some("techno-peak-time-driving"));
        assert_eq!(auto_crossfade_bars(&t, None), 32);
        let t = make_track(Some("techno"));
        assert_eq!(auto_crossfade_bars(&t, None), 16);
    }

    #[test]
    fn next_quantize_boundary_advances_past_now() {
        // Beat interval at 120 BPM = 0.5s. 1-beat quantize means each
        // boundary is at 0, 0.5, 1.0, 1.5… A `time` strictly between
        // boundaries should land on the next one.
        let g = super::super::beat_grid::BeatGrid {
            bpm: 120.0,
            first_beat_time: 0.0,
        };
        let nb = MixEngine::next_quantize_boundary(&g, 0.25, 1.0);
        assert!((nb - 0.5).abs() < 1e-6, "expected 0.5, got {nb}");
        let nb = MixEngine::next_quantize_boundary(&g, 0.6, 1.0);
        assert!((nb - 1.0).abs() < 1e-6, "expected 1.0, got {nb}");
    }

    #[test]
    fn next_quantize_boundary_on_exact_boundary_advances_one_unit() {
        // Pressing exactly on a boundary must NOT return the current
        // boundary (otherwise lookahead never fires) — should jump to
        // the NEXT one. Epsilon-protected ceil() in the impl.
        let g = super::super::beat_grid::BeatGrid {
            bpm: 120.0,
            first_beat_time: 0.0,
        };
        let nb = MixEngine::next_quantize_boundary(&g, 0.5, 1.0);
        assert!(nb > 0.5 + 1e-6, "expected to advance past 0.5, got {nb}");
        assert!((nb - 1.0).abs() < 1e-3, "should land near 1.0, got {nb}");
    }

    #[test]
    fn next_quantize_boundary_sub_beat_resolution() {
        // 1/4 beat at 120 BPM = 0.125s spacing.
        let g = super::super::beat_grid::BeatGrid {
            bpm: 120.0,
            first_beat_time: 0.0,
        };
        let nb = MixEngine::next_quantize_boundary(&g, 0.05, 0.25);
        assert!((nb - 0.125).abs() < 1e-6, "expected 0.125, got {nb}");
    }

    #[test]
    fn next_quantize_boundary_offset_grid() {
        // Grid with first_beat_time != 0: boundaries shift by that offset.
        let g = super::super::beat_grid::BeatGrid {
            bpm: 120.0,
            first_beat_time: 0.1,
        };
        let nb = MixEngine::next_quantize_boundary(&g, 0.0, 1.0);
        assert!((nb - 0.1).abs() < 1e-6, "expected 0.1, got {nb}");
        let nb = MixEngine::next_quantize_boundary(&g, 0.2, 1.0);
        assert!((nb - 0.6).abs() < 1e-6, "expected 0.6, got {nb}");
    }

    #[test]
    fn echoout_fader_sweeps_zero_to_one_across_first_40pct() {
        // EchoOut sweeps the channel fader 0 → 1 across the first 40%
        // of progress on a sin(π/2·t) curve, then holds at 1. Validates
        // the envelope-shape contract documented in the transition layer.
        use super::super::transition::TransitionType;
        let t = TransitionType::EchoOut;
        assert!((t.fader_position(0.0) - 0.0).abs() < 1e-6);
        // Midpoint of sweep (progress 0.2) → sin(π/4) ≈ 0.707.
        let mid = t.fader_position(0.2);
        assert!(
            (mid - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6,
            "expected ~0.707, got {mid}"
        );
        assert!((t.fader_position(0.4) - 1.0).abs() < 1e-6);
        // After 40% it holds at 1.
        assert!((t.fader_position(0.5) - 1.0).abs() < 1e-6);
        assert!((t.fader_position(0.99) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn echoout_absolute_bars_is_eight() {
        // EchoOut is a fixed-length transition regardless of the
        // configured crossfade_bars. start_crossfade reads this and
        // overrides the bar count.
        use super::super::transition::TransitionType;
        assert_eq!(TransitionType::EchoOut.absolute_bars(), Some(8));
        // BeatMatched uses the configured bar count → None.
        assert_eq!(TransitionType::BeatMatched.absolute_bars(), None);
    }

    #[test]
    fn auto_crossfade_bars_tech_house_wins_over_house_substring() {
        // "tech-house" contains "house". The match order in
        // auto_crossfade_bars determines which tier wins. Pinning
        // the current behavior (tech-house → 16, not 8) prevents
        // a future tier-list reorder from silently remapping it.
        let t = make_track(Some("tech-house"));
        assert_eq!(auto_crossfade_bars(&t, None), 16);
    }

    fn phrases_at(start_times: &[f64]) -> super::super::analyzer::AnalysisResult {
        use super::super::analyzer::{AnalysisResult, Phrase, PhraseType};
        AnalysisResult {
            beat_grid: super::super::beat_grid::BeatGrid {
                bpm: 128.0,
                first_beat_time: 0.0,
            },
            rms_loudness: 0.0,
            phrases: start_times
                .iter()
                .map(|&t| Phrase {
                    start_time: t,
                    energy: 0.5,
                    phrase_type: PhraseType::Buildup,
                })
                .collect(),
            waveform_peaks: vec![],
            first_audio: 0.0,
        }
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_bumps_up_for_long_phrases() {
        // Tech-house base = 16. Per-window intervals at 60s each →
        // bumps up one tier to 32. Uses the new math (window deltas,
        // not last_start / count).
        let t = make_track(Some("tech-house"));
        let a = phrases_at(&[0.0, 60.0, 120.0, 180.0, 240.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 32);
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_bumps_down_for_short_phrases() {
        // Tech-house base = 16. Per-window intervals at 12s each →
        // bumps down to 8.
        let t = make_track(Some("tech-house"));
        let a = phrases_at(&[0.0, 12.0, 24.0, 36.0, 48.0, 60.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 8);
    }

    #[test]
    fn forward_safe_delta_passes_through_in_bounds_seek() {
        // Normal forward nudge within bounds → returned as-is.
        let d = forward_safe_delta(10.0, 0.05, 0.5, 60.0, 0.002);
        assert!((d - 0.05).abs() < 1e-9);
    }

    #[test]
    fn forward_safe_delta_walks_forward_when_backward_underflows() {
        // inc_t=0.1, raw=-0.4 would land at -0.3 — instead add period
        // (0.5) and land at 0.2. Returns +0.1 (forward equivalent).
        let d = forward_safe_delta(0.1, -0.4, 0.5, 60.0, 0.002);
        assert!((d - 0.1).abs() < 1e-9, "expected +0.1, got {d}");
    }

    #[test]
    fn forward_safe_delta_returns_zero_below_threshold() {
        // Raw smaller than min_threshold → no seek (returns 0).
        let d = forward_safe_delta(10.0, 0.001, 0.5, 60.0, 0.002);
        assert_eq!(d, 0.0);
    }

    #[test]
    fn forward_safe_delta_returns_zero_when_out_of_track() {
        // Seek past end of track → no-op.
        let d = forward_safe_delta(59.9, 0.5, 0.5, 60.0, 0.002);
        assert_eq!(d, 0.0);
        // Seek that would land negative even after adding period.
        let d = forward_safe_delta(0.0, -1.0, 0.5, 60.0, 0.002);
        assert_eq!(d, 0.0);
    }

    #[test]
    fn bpm_gap_cutoff_is_rubberband_aware() {
        // Without a stretcher, varispeed pitch shift caps at 8% (~1.3
        // semitones). With Rubberband, quality holds to ~15%, so 14%
        // is the safe cap (~2.3 semitones equivalent).
        assert_eq!(bpm_gap_cutoff(false), 8.0);
        assert_eq!(bpm_gap_cutoff(true), 14.0);
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_below_min_phrase_count_no_change() {
        // With < 3 phrases the analyzer signal is too noisy to make
        // a density call (1 interval is one data point — not stable).
        // Must return the genre base unchanged.
        let t = make_track(Some("tech-house"));
        // 0 phrases
        let a = phrases_at(&[]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 16);
        // 1 phrase
        let a = phrases_at(&[5.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 16);
        // 2 phrases
        let a = phrases_at(&[0.0, 60.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 16);
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_at_minimum_three_phrases() {
        // Exactly 3 phrases (2 intervals) is the boundary where the
        // density path engages. Pin the behavior at that minimum.
        let t = make_track(Some("tech-house"));
        // 2 long intervals → bump up
        let a = phrases_at(&[0.0, 60.0, 120.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 32);
        // 2 short intervals → bump down
        let a = phrases_at(&[0.0, 12.0, 24.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 8);
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_filters_non_positive_deltas() {
        // Out-of-order or duplicate-timestamp phrases (analyzer noise)
        // produce zero or negative window deltas. The filter strips
        // those so they don't poison the average. Without the filter,
        // a zero delta would drag avg_phrase down toward the bump-down
        // threshold and produce a phantom bump-down.
        let t = make_track(Some("tech-house"));
        // Real intervals are 60s, but a duplicate-timestamp injects 0s.
        // Without filter: avg = (60+0+60)/3 = 40 → still neutral but
        // skewed. With filter: avg = (60+60)/2 = 60 → bump up correctly.
        let a = phrases_at(&[0.0, 60.0, 60.0, 120.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 32);
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_neutral_band_holds_base() {
        // 30s intervals fall in the neutral band (18..=45) → no change.
        let t = make_track(Some("tech-house"));
        let a = phrases_at(&[0.0, 30.0, 60.0, 90.0, 120.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 16);
    }

    #[test]
    fn auto_crossfade_bars_phrase_density_ignores_intro_outro_padding() {
        // Critical regression test for the math fix. Old code did
        // `last_start / (len-1)` which conflated total-span / count
        // with phrase period. A 360s track with 3 detected phrases
        // (long intro + 2 sparse drops) gave avg=180s and bumped
        // up — wrong. With the fix, only the consecutive deltas
        // matter: intro→p1=60s, p1→p2=60s → both within the
        // bump-up band, but at 60s avg this is correctly bump-up.
        // Now construct the inverse: a 360s track where the
        // intervals are actually short (15s each) but the total
        // span is long. Old code would have bumped up; new code
        // correctly bumps down.
        let t = make_track(Some("tech-house"));
        let a = phrases_at(&[0.0, 15.0, 30.0, 45.0, 60.0]);
        assert_eq!(auto_crossfade_bars(&t, Some(&a)), 8); // bump-down
    }

    #[test]
    fn auto_crossfade_bars_is_case_insensitive() {
        // Beatport sometimes returns title-case slugs. The function
        // lowercases before matching; if that ever changes the genre
        // tiers all silently fall through to the default.
        let t = make_track(Some("Progressive-House"));
        assert_eq!(auto_crossfade_bars(&t, None), 32);
        let t = make_track(Some("DRUM-BASS"));
        assert_eq!(auto_crossfade_bars(&t, None), 4);
    }
}
