use std::sync::Arc;

use super::beat_grid::BeatGrid;
use super::engine::{DeckId, EngineState, MixEngine, QueueEntry, HistoryEntry};
use crate::beatport::models::BeatportTrack;

/// Alignment readout for manual-mode beatmatching. `beat_phase_ms` is
/// the within-beat offset (positive = playing ahead of incoming).
/// `beat_in_bar_*` are 0..=3 (the bar slot); if they differ, the 1s
/// aren't lined up even if phase is tight. `bar_in_phrase_*` are
/// 0..=15 against a 16-bar phrase convention — if they differ, drops
/// won't land together.
#[derive(Debug, Clone, Default)]
pub struct AlignmentPeaks {
    pub playing_peaks: Vec<f32>,
    pub incoming_peaks: Vec<f32>,
    pub playing_bpm: f64,
    pub incoming_bpm: f64,
}

pub struct AlignmentReadout {
    pub beat_phase_ms: f64,
    /// Phase delta as a fraction of one beat (0.0–0.5, unsigned).
    /// BPM-independent — useful for making phase decisions without
    /// knowing the absolute BPM of either deck.
    pub beat_phase_fraction: f64,
    pub beat_in_bar_a: u32,
    pub beat_in_bar_b: u32,
    pub bar_in_phrase_a: u32,
    pub bar_in_phrase_b: u32,
}

#[derive(Debug, Clone, Default)]
pub struct NowPlayingInfo {
    pub playing_track: Option<Arc<BeatportTrack>>,
    pub playing_bpm: Option<f64>,
    pub playing_time: f64,
    pub playing_duration: f64,
    pub incoming_track: Option<Arc<BeatportTrack>>,
    pub incoming_bpm: Option<f64>,
    pub playing_analysis: Option<Arc<super::analyzer::AnalysisResult>>,
    pub state: EngineState,
    pub crossfade_progress: f64,
    /// Physical crossfader position (0.0 = full deck A, 1.0 = full deck B).
    /// Idle: 0 if A is the active deck, 1 if B. During a crossfade the
    /// needle moves toward the incoming side — direction flips every mix.
    pub crossfader_visual: f64,
    /// Normalized fader-sweep progress (0 = A side, 1 = fader has
    /// landed on B). Computed from `transition.fader_position`, so
    /// transitions with a short sweep (EchoOut completes at 40%)
    /// reach 1.0 well before crossfade_progress does. Used by the
    /// dashboard countdown so the bars remaining hits 0 when the
    /// fader is done moving, not when the post-sweep tail ends.
    pub fader_sweep_progress: f64,
    /// True when both decks' downbeats land on the same bar position
    /// (bar_offset_beats == 0). Green phase dots with `false` here
    /// means "within-beat aligned but the 1s are off" — the classic
    /// "beats match but the phrase is shifted" trap.
    pub downbeat_aligned: bool,
    /// True when deck A is the currently-playing deck. Use this to resolve
    /// any role-based field (playing_*, incoming_*) back to a physical deck.
    pub playing_is_a: bool,
    // Physical deck snapshots — stable regardless of which deck is playing.
    // UI uses these for the Controller rendering so deck A stays on the left.
    pub deck_a_track: Option<Arc<BeatportTrack>>,
    pub deck_a_bpm: Option<f64>,
    pub deck_a_time: f64,
    pub deck_a_duration: f64,
    pub deck_a_level: f32,
    pub deck_a_beat_pos: f64,
    pub deck_a_analysis: Option<Arc<super::analyzer::AnalysisResult>>,
    pub deck_a_is_playing: bool,
    pub deck_a_rate: f64,
    pub deck_a_kick: bool,
    pub deck_b_track: Option<Arc<BeatportTrack>>,
    pub deck_b_bpm: Option<f64>,
    pub deck_b_time: f64,
    pub deck_b_duration: f64,
    pub deck_b_level: f32,
    pub deck_b_beat_pos: f64,
    pub deck_b_analysis: Option<Arc<super::analyzer::AnalysisResult>>,
    pub deck_b_is_playing: bool,
    pub deck_b_rate: f64,
    pub deck_b_kick: bool,
    // Per-deck mixer state (dB / [-1,+1]).
    pub deck_a_eq_low_db: f32,
    pub deck_a_eq_mid_db: f32,
    pub deck_a_eq_high_db: f32,
    pub deck_a_filter_pos: f32,
    pub deck_a_loop_active: bool,
    pub deck_b_eq_low_db: f32,
    pub deck_b_eq_mid_db: f32,
    pub deck_b_eq_high_db: f32,
    pub deck_b_filter_pos: f32,
    pub deck_b_loop_active: bool,
    /// Which hot-cue slots (0..=3) have a position stored on each deck.
    pub deck_a_cues: [bool; 4],
    pub deck_b_cues: [bool; 4],
    // Mixer-wide channel faders and crossfader position (physical, 0 = A, 1 = B).
    pub channel_fader_a: f32,
    pub channel_fader_b: f32,
    pub crossfader_pos: f32, // -1..+1
    pub transition_type_name: &'static str,
    /// Active controller's bar count while crossfading, otherwise the
    /// count the next crossfade would use for the current transition
    /// type (16 × duration_multiplier, matching start_crossfade).
    pub transition_bars: u32,
    /// Current `<` / `>` jump distance in bars (mirror of
    /// `AppConfig.jump_bars`). Rendered in the JUMP button label
    /// so the user sees what a jump press will do.
    pub jump_bars: u32,
    pub phase_offset_ms: f64,
    pub mix_point_time: Option<f64>, // when crossfade will trigger (seconds into playing track)
    /// Minutes since the first track started playing this session.
    pub session_time_min: u32,
    pub queue: Vec<QueueEntry>,
    pub history: Vec<HistoryEntry>,
}

impl MixEngine {
    pub fn read_alignment(&self) -> AlignmentReadout {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        let (playing, incoming) = match s.playing_deck {
            DeckId::A => (&s.deck_a, &s.deck_b),
            DeckId::B => (&s.deck_b, &s.deck_a),
        };
        let (beat_phase_ms, beat_phase_fraction) = match (playing.beat_grid, incoming.beat_grid) {
            (Some(pg), Some(ig)) => {
                let phase_s = BeatGrid::phase_offset(
                    &pg, playing.current_time(),
                    &ig, incoming.current_time(),
                );
                let pa = pg.phase(playing.current_time());
                let pb = ig.phase(incoming.current_time());
                let mut frac = (pa - pb).abs();
                if frac > 0.5 { frac = 1.0 - frac; }
                (phase_s * 1000.0, frac)
            }
            _ => (0.0, 0.0),
        };
        let beat_in_bar_a = s.deck_a.beat_grid
            .map(|g| g.beat_in_bar(s.deck_a.current_time())).unwrap_or(0);
        let beat_in_bar_b = s.deck_b.beat_grid
            .map(|g| g.beat_in_bar(s.deck_b.current_time())).unwrap_or(0);
        let bar_in_phrase = |d: &super::deck::DeckPlayer| -> u32 {
            d.beat_grid.map(|g| (g.bar_index(d.current_time()).rem_euclid(16)) as u32).unwrap_or(0)
        };
        AlignmentReadout {
            beat_phase_ms,
            beat_phase_fraction,
            beat_in_bar_a,
            beat_in_bar_b,
            bar_in_phrase_a: bar_in_phrase(&s.deck_a),
            bar_in_phrase_b: bar_in_phrase(&s.deck_b),
        }
    }

    /// Extract per-ms peak amplitude data for AI mix alignment analysis.
    /// Computes the reduction under the lock (O(window_ms) iterations,
    /// ~30KB output) rather than cloning raw samples (~5MB).
    pub fn alignment_peaks(&self) -> Option<AlignmentPeaks> {
        let s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());
        if s.state != EngineState::Crossfading { return None; }
        let (playing, incoming) = match s.playing_deck {
            DeckId::A => (&s.deck_a, &s.deck_b),
            DeckId::B => (&s.deck_b, &s.deck_a),
        };
        if playing.samples.is_empty() || incoming.samples.is_empty() { return None; }
        let p_bpm = playing.beat_grid.map(|g| g.bpm).unwrap_or(128.0);
        let i_bpm = incoming.beat_grid.map(|g| g.bpm).unwrap_or(p_bpm);
        let sr = playing.sample_rate as usize;
        let bin = (sr / 1000).max(1);
        let bar_ms = ((60.0 / p_bpm) * 4000.0) as usize;
        let window_ms = bar_ms * 4; // 4 bars
        let p_start_sample = (playing.current_time() * sr as f64) as usize;
        let i_start_sample = (incoming.current_time() * sr as f64) as usize;
        let mut p_peaks = Vec::with_capacity(window_ms);
        let mut i_peaks = Vec::with_capacity(window_ms);
        for ms in 0..window_ms {
            let ps = p_start_sample + ms * bin;
            let pe = (ps + bin).min(playing.samples.len());
            p_peaks.push(if pe > ps && pe <= playing.samples.len() {
                playing.samples[ps..pe].iter().map(|s| s.abs()).fold(0.0f32, f32::max)
            } else { 0.0 });
            let is = i_start_sample + ms * bin;
            let ie = (is + bin).min(incoming.samples.len());
            i_peaks.push(if ie > is && ie <= incoming.samples.len() {
                incoming.samples[is..ie].iter().map(|s| s.abs()).fold(0.0f32, f32::max)
            } else { 0.0 });
        }
        Some(AlignmentPeaks {
            playing_peaks: p_peaks,
            incoming_peaks: i_peaks,
            playing_bpm: p_bpm,
            incoming_bpm: i_bpm,
        })
    }

    pub fn now_playing(&self) -> NowPlayingInfo {
        struct DeckSnap {
            track: Option<Arc<BeatportTrack>>,
            bpm: Option<f64>,
            time: f64,
            duration: f64,
            level: f32,
            beat_first: f64,
            analysis: Option<Arc<super::analyzer::AnalysisResult>>,
            playing: bool,
            kick: bool,
            rate: f64,
            eq_low_db: f32,
            eq_mid_db: f32,
            eq_high_db: f32,
            filter_pos: f32,
            loop_active: bool,
            cues: [bool; 4],
        }
        fn snap_of(d: &super::deck::DeckPlayer) -> DeckSnap {
            DeckSnap {
                track: d.track.clone(),
                // Effective playback BPM = native × rate. Reflects nudge,
                // rate glide, and crossfade BPM-match live, so the dashboard
                // reading tracks what you actually hear.
                bpm: d.beat_grid.map(|g| g.bpm * d.rate),
                time: d.current_time(),
                duration: d.duration(),
                level: d.level,
                beat_first: d.beat_grid.map(|g| g.first_beat_time).unwrap_or(0.0),
                analysis: d.analysis.clone(),
                playing: d.playing,
                kick: d.kick_active,
                // `rate_target` is the commanded value — stable across
                // audio-callback slewing, so the fader indicator
                // doesn't flicker while rate converges.
                rate: d.rate_target,
                eq_low_db: d.eq_low_db,
                eq_mid_db: d.eq_mid_db,
                eq_high_db: d.eq_high_db,
                filter_pos: d.filter_pos,
                loop_active: d.loop_active,
                cues: [d.cues[0].is_some(), d.cues[1].is_some(),
                       d.cues[2].is_some(), d.cues[3].is_some()],
            }
        }


        // Phase 1 — hold the audio state lock only long enough to pull out
        // scalar values + cheap Arc clones. No iteration, no allocation-heavy
        // work here. Anything derivable off-lock (queue/history snapshot,
        // string labels, transition visuals) is computed in phase 2.
        let snap = {
            let mut s = self.audio_state.lock().unwrap_or_else(|e| e.into_inner());

            let (raw_phase_ms, downbeat_aligned) = {
                let (playing, incoming) = match s.playing_deck {
                    DeckId::A => (&s.deck_a, &s.deck_b),
                    DeckId::B => (&s.deck_b, &s.deck_a),
                };
                let fader_at_b = s.state == EngineState::Crossfading
                    && s.transition_type.fader_position(s.crossfade_progress) >= 1.0;
                match (&playing.beat_grid, &incoming.beat_grid) {
                    (Some(pg), Some(ig)) if incoming.playing && !fader_at_b => {
                        let ph = BeatGrid::phase_offset(pg, playing.current_time(), ig, incoming.current_time()) * 1000.0;
                        // Bar-level: 1s land together (mod 4 beats).
                        // Phrase-level alignment was tried here too but
                        // `bar_index % 16` is just arithmetic from beat 0
                        // with no actual phrase-boundary anchor — and
                        // mixing into bar 8 vs bar 0 is normal DJing
                        // anyway. Keep the indicator focused on the "1s
                        // land together" claim so the green dot is
                        // useful, not punitive.
                        let aligned = BeatGrid::bar_offset_beats(pg, playing.current_time(), ig, incoming.current_time()) == 0;
                        (ph, aligned)
                    }
                    // Single deck (or pre-mix): treat as aligned so the
                    // idle indicator stays neutral.
                    _ => (0.0, true),
                }
            };
            let phase_offset_ms = if raw_phase_ms == 0.0 {
                s.phase_display_ema = 0.0;
                0.0
            } else {
                let wrapped = s.phase_display_ema.signum() != raw_phase_ms.signum()
                    && s.phase_display_ema.abs() > 10.0 && raw_phase_ms.abs() > 10.0;
                if !wrapped {
                    s.phase_display_ema = s.phase_display_ema * 0.95 + raw_phase_ms * 0.05;
                }
                s.phase_display_ema
            };

            (
                snap_of(&s.deck_a),
                snap_of(&s.deck_b),
                s.playing_deck,
                s.state,
                s.crossfade_progress,
                s.transition_type,
                s.channel_fader_a,
                s.channel_fader_b,
                s.crossfader_pos,
                s.cached_trigger_time,
                phase_offset_ms,
                s.session_start.map(|t| (t.elapsed().as_secs() / 60) as u32).unwrap_or(0),
                s.crossfade_controller.as_ref().map(|c| c.crossfade_bars),
                s.crossfade_bars,
                downbeat_aligned,
                s.jump_bars,
            )
        }; // lock released here

        let (a, b, playing_deck, state, crossfade_progress, transition_type,
             channel_fader_a, channel_fader_b, crossfader_pos, mix_point_time,
             phase_offset_ms, session_time_min,
             active_crossfade_bars, config_crossfade_bars, downbeat_aligned,
             jump_bars) = snap;

        // Snap to zero on the formatter's noise floor — avoids -0.0/+0.0 flicker.
        let phase_offset_ms = if phase_offset_ms.abs() < 0.1 { 0.0 } else { phase_offset_ms };

        let beat_pos = |s: &DeckSnap| -> f64 {
            match s.bpm {
                Some(bpm) if bpm > 0.0 => (s.time - s.beat_first) * bpm / 60.0,
                _ => 0.0,
            }
        };
        let (playing_snap, incoming_snap) = if playing_deck == DeckId::A { (&a, &b) } else { (&b, &a) };

        let transition_type_name = match transition_type {
            super::transition::TransitionType::BeatMatched => "BeatMatched",
            super::transition::TransitionType::EchoOut => "EchoOut",
            super::transition::TransitionType::BassSwap => "BassSwap",
            super::transition::TransitionType::FilterSweep => "FilterSweep",
            super::transition::TransitionType::LoopRoll => "LoopRoll",
        };
        // Active bars if crossfading; otherwise the count the next mix
        // would use (matches start_crossfade's hardcoded base of 16).
        // Active controller's bars if a crossfade is running; otherwise
        // what the next mix would use. Transitions with a fixed bar
        // count (EchoOut = 8) bypass the crossfade_bars setting.
        let transition_bars = active_crossfade_bars.unwrap_or_else(|| {
            transition_type.absolute_bars().unwrap_or_else(||
                (config_crossfade_bars as f64 * transition_type.duration_multiplier()).max(1.0) as u32
            )
        });
        let fader_sweep_progress = if state == EngineState::Crossfading {
            transition_type.fader_position(crossfade_progress)
        } else { 0.0 };
        let crossfader_visual = {
            // During a crossfade: follow the transition's volume curve
            // so the needle moves naturally with the mix.
            // When idle: follow `crossfader_pos` if the user has moved
            // it (manual mix / MIDI / IPC); otherwise park the needle
            // on the active deck's side as a visual hint of which deck
            // is making sound.
            let base = if playing_deck == DeckId::A { 0.0 } else { 1.0 };
            if state == EngineState::Crossfading {
                if playing_deck == DeckId::A { fader_sweep_progress } else { 1.0 - fader_sweep_progress }
            } else if crossfader_pos.abs() > 1e-3 {
                // crossfader_pos: -1.0 = full A (left), +1.0 = full B (right)
                // visual:          0.0 = full A (left), 1.0 = full B (right)
                ((crossfader_pos as f64 + 1.0) / 2.0).clamp(0.0, 1.0)
            } else {
                base
            }
        };

        NowPlayingInfo {
            playing_track: playing_snap.track.clone(),
            playing_bpm: playing_snap.bpm,
            playing_time: playing_snap.time,
            playing_duration: playing_snap.duration,
            incoming_track: incoming_snap.track.clone(),
            incoming_bpm: incoming_snap.bpm,
            playing_analysis: playing_snap.analysis.clone(),
            state,
            crossfade_progress,
            playing_is_a: playing_deck == DeckId::A,
            deck_a_track: a.track.clone(),
            deck_a_bpm: a.bpm,
            deck_a_time: a.time,
            deck_a_duration: a.duration,
            deck_a_level: a.level,
            deck_a_beat_pos: beat_pos(&a),
            deck_a_analysis: a.analysis.clone(),
            deck_a_is_playing: a.playing,
            deck_a_rate: a.rate,
            deck_a_kick: a.kick,
            deck_b_track: b.track.clone(),
            deck_b_bpm: b.bpm,
            deck_b_time: b.time,
            deck_b_duration: b.duration,
            deck_b_level: b.level,
            deck_b_beat_pos: beat_pos(&b),
            deck_b_analysis: b.analysis.clone(),
            deck_b_is_playing: b.playing,
            deck_b_rate: b.rate,
            deck_b_kick: b.kick,
            deck_a_eq_low_db: a.eq_low_db,
            deck_a_eq_mid_db: a.eq_mid_db,
            deck_a_eq_high_db: a.eq_high_db,
            deck_a_filter_pos: a.filter_pos,
            deck_a_loop_active: a.loop_active,
            deck_b_eq_low_db: b.eq_low_db,
            deck_b_eq_mid_db: b.eq_mid_db,
            deck_b_eq_high_db: b.eq_high_db,
            deck_b_filter_pos: b.filter_pos,
            deck_b_loop_active: b.loop_active,
            deck_a_cues: a.cues,
            deck_b_cues: b.cues,
            channel_fader_a,
            channel_fader_b,
            crossfader_pos,
            transition_type_name,
            transition_bars,
            jump_bars,
            crossfader_visual,
            fader_sweep_progress,
            downbeat_aligned,
            phase_offset_ms,
            mix_point_time,
            session_time_min,
            queue: self.queue.iter().take(20).cloned().collect(),
            history: self.history.iter().rev().take(20).cloned().collect(),
        }
    }
}
