//! Virtual mixer — single control surface for automix engine and Claude DJ.
//! All deck and mixer controls go through here. Direct writes to DeckPlayer fields.

use super::deck::DeckPlayer;

/// Virtual mixer controls. Operates on DeckPlayer references directly.
/// Both the automix engine (transitions) and Claude DJ use these same methods.
pub struct Mixer;

impl Mixer {
    // ── Transport ──

    pub fn play(deck: &mut DeckPlayer) {
        deck.play();
    }

    pub fn pause(deck: &mut DeckPlayer) {
        deck.paused = true;
        deck.playing = false;
    }

    pub fn stop(deck: &mut DeckPlayer) {
        deck.stop();
    }

    pub fn seek(deck: &mut DeckPlayer, time: f64) {
        deck.seek(time);
    }

    // ── Tempo ──

    /// Set the target playback rate (1.0 = native BPM). `fill_buffer`
    /// slews `deck.rate` toward this target per-sample over ~20ms so
    /// glide / rate-correction updates at 60Hz don't produce audible
    /// pitch stairs.
    pub fn set_rate(deck: &mut DeckPlayer, rate: f64) {
        deck.rate_target = rate;
    }

    /// Rate-match deck to a target BPM.
    pub fn match_bpm(deck: &mut DeckPlayer, target_bpm: f64) {
        if let Some(grid) = deck.beat_grid
            && grid.bpm > 0.0 {
                deck.rate_target = target_bpm / grid.bpm;
            }
    }

    // ── Volume ──

    /// Set deck volume. Clamped to [0.0, 1.0] so IPC callers can't blow
    /// the limiter by writing a raw 1.5 through — EQ and filter setters
    /// clamp; this now matches.
    pub fn set_volume(deck: &mut DeckPlayer, volume: f32) {
        deck.volume = volume.clamp(0.0, 1.0);
    }

    // ── 3-band EQ (gain in dB; 0 = unity, -24 = kill) ──

    pub fn set_eq_low(deck: &mut DeckPlayer, gain_db: f32) {
        deck.eq_low_db = gain_db.clamp(-24.0, 12.0);
        deck.update_eq_low();
    }
    pub fn set_eq_mid(deck: &mut DeckPlayer, gain_db: f32) {
        deck.eq_mid_db = gain_db.clamp(-24.0, 12.0);
        deck.update_eq_mid();
    }
    pub fn set_eq_high(deck: &mut DeckPlayer, gain_db: f32) {
        deck.eq_high_db = gain_db.clamp(-24.0, 12.0);
        deck.update_eq_high();
    }

    // ── Filter sweep (−1 LP, 0 bypass, +1 HP) ──

    pub fn set_filter(deck: &mut DeckPlayer, pos: f32) {
        deck.filter_pos = pos.clamp(-1.0, 1.0);
        deck.update_filter();
    }

    // ── FX: Delay/Echo ──

    /// Set delay wet mix (0.0 = dry, 1.0 = full wet).
    pub fn set_delay_wet(deck: &mut DeckPlayer, wet: f32) {
        deck.delay_wet = wet;
    }

    /// Set delay feedback (0.0 to 1.0).
    pub fn set_delay_feedback(deck: &mut DeckPlayer, feedback: f32) {
        deck.delay_feedback = feedback;
    }

    /// Set delay time in samples.
    pub fn set_delay_samples(deck: &mut DeckPlayer, samples: usize) {
        deck.delay_samples = samples.min(deck.delay_buffer.len().saturating_sub(1));
    }

    /// Set delay time synced to BPM (dotted eighth = 0.75 beats).
    pub fn set_delay_sync(deck: &mut DeckPlayer, beat_fraction: f64) {
        if let Some(grid) = deck.beat_grid {
            let beat_secs = 60.0 / grid.bpm;
            let delay = (beat_secs * beat_fraction * deck.output_sample_rate as f64) as usize;
            Self::set_delay_samples(deck, delay);
        }
    }

    // ── Hot Cues ──

    /// Store the deck's current position as hot cue `slot` (0..=3).
    pub fn cue_set(deck: &mut DeckPlayer, slot: usize) {
        if let Some(cell) = deck.cues.get_mut(slot) {
            *cell = Some(deck.position as u64);
        }
    }

    /// Clear a single hot cue.
    pub fn cue_clear(deck: &mut DeckPlayer, slot: usize) {
        if let Some(cell) = deck.cues.get_mut(slot) {
            *cell = None;
        }
    }

    // ── Loop ──

    /// Set loop in-point at deck's current position.
    pub fn loop_in(deck: &mut DeckPlayer) {
        deck.loop_in = Some(deck.position as u64);
    }

    /// Set loop out-point at deck's current position and activate the loop.
    pub fn loop_out(deck: &mut DeckPlayer) {
        let pos = deck.position as u64;
        if let Some(lin) = deck.loop_in
            && pos > lin {
                deck.loop_out = Some(pos);
                deck.loop_active = true;
            }
    }

    /// Set a beat-aligned loop of N beats starting at current position.
    pub fn loop_beats(deck: &mut DeckPlayer, beats: f64) {
        if let Some(grid) = deck.beat_grid
            && grid.bpm > 0.0 {
                let beat_secs = 60.0 / grid.bpm;
                let len = (beat_secs * beats * deck.sample_rate as f64) as u64;
                let start = deck.position as u64;
                deck.loop_in = Some(start);
                deck.loop_out = Some(start + len);
                deck.loop_active = true;
            }
    }

    /// Release the loop — playback continues past the out-point.
    pub fn loop_release(deck: &mut DeckPlayer) {
        deck.loop_active = false;
        deck.loop_in = None;
        deck.loop_out = None;
    }

    // ── Beat Grid ──

    /// Shift the beat grid first_beat_time by ms (for nudge/correction).
    pub fn shift_grid(deck: &mut DeckPlayer, shift_ms: f64) {
        if let Some(ref mut grid) = deck.beat_grid {
            grid.first_beat_time += shift_ms / 1000.0;
        }
    }


    /// Tap-nudge: multiply rate by (1 + percent/100). Returns the new rate.
    /// Caller is responsible for restoring the base rate after the nudge window.
    pub fn nudge_rate(deck: &mut DeckPlayer, base_rate: f64, percent: f64) -> f64 {
        let new_rate = base_rate * (1.0 + percent / 100.0);
        deck.rate = new_rate;
        new_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_deck() -> DeckPlayer { DeckPlayer::new(48_000) }

    #[test]
    fn set_volume_clamps_to_0_1() {
        // Regression: set_volume was previously unclamped while EQ and
        // filter were clamped. Ensure IPC callers can't overshoot 1.0.
        let mut d = fresh_deck();
        Mixer::set_volume(&mut d, 1.5);
        assert!((d.volume - 1.0).abs() < 1e-6);
        Mixer::set_volume(&mut d, -0.3);
        assert!(d.volume.abs() < 1e-6);
        Mixer::set_volume(&mut d, 0.75);
        assert!((d.volume - 0.75).abs() < 1e-6);
    }

    #[test]
    fn set_eq_low_clamps_to_range() {
        let mut d = fresh_deck();
        Mixer::set_eq_low(&mut d, 30.0);
        assert!((d.eq_low_db - 12.0).abs() < 1e-6);
        Mixer::set_eq_low(&mut d, -40.0);
        assert!((d.eq_low_db - -24.0).abs() < 1e-6);
    }

    #[test]
    fn set_filter_clamps_to_neg1_to_1() {
        let mut d = fresh_deck();
        Mixer::set_filter(&mut d, 3.0);
        assert!((d.filter_pos - 1.0).abs() < 1e-6);
        Mixer::set_filter(&mut d, -3.0);
        assert!((d.filter_pos - -1.0).abs() < 1e-6);
    }

    #[test]
    fn match_bpm_computes_rate_ratio() {
        // match_bpm(deck, target) sets deck.rate such that the deck's
        // native BPM plays at `target`. With native=128, target=132,
        // rate should be 132/128 = 1.03125.
        let mut d = fresh_deck();
        d.beat_grid = Some(crate::audio::beat_grid::BeatGrid {
            bpm: 128.0, first_beat_time: 0.0,
        });
        Mixer::match_bpm(&mut d, 132.0);
        // `match_bpm` writes `rate_target` now (audio thread slews
        // `rate` toward it). Verify the commanded ratio is right.
        assert!((d.rate_target - 1.03125).abs() < 1e-6, "rate_target={}", d.rate_target);
    }

    #[test]
    fn set_rate_writes_through() {
        let mut d = fresh_deck();
        Mixer::set_rate(&mut d, 1.05);
        assert!((d.rate_target - 1.05).abs() < 1e-6);
    }
}
