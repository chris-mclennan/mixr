/// Crossfade controller — volume curves and phase sync correction.
pub struct CrossfadeController {
    pub crossfade_bars: u32,
    pub playing_bpm: f64,
    pub incoming_bpm: f64,
    smoothed_correction: f64,
    /// Integral term accumulator — drives steady-state phase error to zero.
    /// Clamped to ±0.005 to prevent windup; decayed in the dead zone.
    integrated_error: f64,
    /// Throttle rate corrections so we re-evaluate at most once per beat —
    /// a smaller deck stays nearly in sync between corrections (rate is held)
    /// and we avoid wiggling the rate at 60 Hz when the system is stable.
    last_correction_at: Option<std::time::Instant>,
    last_returned: f64,
    /// True until the first rate_correction call completes. Used for the
    /// one-shot initial kick on large offsets.
    first_call: bool,
}

impl CrossfadeController {
    pub fn new(playing_bpm: f64, incoming_bpm: f64, crossfade_bars: u32) -> Self {
        Self {
            crossfade_bars,
            playing_bpm,
            incoming_bpm,
            smoothed_correction: 0.0,
            integrated_error: 0.0,
            last_correction_at: None,
            last_returned: 0.0,
            first_call: true,
        }
    }

    fn beat_interval_secs(&self) -> f64 {
        let b = if self.playing_bpm > 0.0 {
            self.playing_bpm
        } else {
            128.0
        };
        60.0 / b
    }

    /// Duration in seconds (N bars at playing deck's BPM).
    pub fn duration(&self) -> f64 {
        let bpm = if self.playing_bpm > 0.0 {
            self.playing_bpm
        } else {
            128.0
        };
        let bar_duration = (60.0 / bpm) * 4.0;
        bar_duration * self.crossfade_bars as f64
    }

    // -- Sync Correction --

    pub fn rate_correction(&mut self, phase_offset_ms: f64) -> f64 {
        // Re-evaluate at most once per beat. Between calls, return the most
        // recent correction so the deck rate stays stable (no 60 Hz wobble).
        let now = std::time::Instant::now();
        let beat = self.beat_interval_secs();
        if let Some(prev) = self.last_correction_at
            && now.duration_since(prev).as_secs_f64() < beat
        {
            return self.last_returned;
        }
        self.last_correction_at = Some(now);

        let abs_offset = phase_offset_ms.abs();

        // First-call kick: when the initial offset is large (>20ms), the
        // normal ±1% clamp takes too many beats to converge. Apply a
        // one-shot 3% correction to close the gap quickly, then hand off
        // to the gentle EMA on subsequent calls.
        if self.first_call && abs_offset > 20.0 {
            self.first_call = false;
            let kick = (phase_offset_ms * 0.001).clamp(-0.03, 0.03);
            self.smoothed_correction = kick;
            self.last_returned = kick;
            return kick;
        }
        self.first_call = false;

        // Dead zone: < 3ms is close enough
        if abs_offset < 3.0 {
            self.smoothed_correction *= 0.95;
            self.last_returned = self.smoothed_correction;
            return self.smoothed_correction;
        }

        // Gentle proportional gain. The 3–15 ms band is the common case for
        // cross-BPM mixes (e.g. a 7–8 ms residual when BPM ratios don't divide
        // cleanly). Previously kp here was ~0 at 3 ms, making that residual
        // persist audibly for many bars. Floor raised to 0.0001 at 3 ms so
        // correction actually works in the band where most real mixes land.
        let kp = if abs_offset > 50.0 {
            0.0005
        } else if abs_offset > 15.0 {
            0.0002 + 0.0003 * (abs_offset - 15.0) / 35.0
        } else {
            0.0001 + 0.0001 * (abs_offset - 3.0) / 12.0
        };

        // Integral term: accumulate error to drive steady-state offset to zero.
        // Only integrate above the dead zone to prevent windup at small offsets.
        if abs_offset >= 3.0 {
            self.integrated_error += phase_offset_ms * 0.00001;
            self.integrated_error = self.integrated_error.clamp(-0.005, 0.005);
        } else {
            // Decay integrator in dead zone to prevent windup
            self.integrated_error *= 0.9;
        }
        let raw_correction = phase_offset_ms * kp + self.integrated_error;
        let clamped = raw_correction.clamp(-0.01, 0.01); // ±1% max

        // Heavy smoothing to prevent oscillation
        let smooth_factor = if abs_offset < 10.0 { 0.85 } else { 0.75 };
        self.smoothed_correction =
            self.smoothed_correction * smooth_factor + clamped * (1.0 - smooth_factor);

        self.last_returned = self.smoothed_correction;
        self.smoothed_correction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> CrossfadeController {
        CrossfadeController::new(128.0, 128.0, 16)
    }

    #[test]
    fn rate_correction_dead_zone_decays_to_zero() {
        let mut c = fresh();
        c.smoothed_correction = 0.005;
        // Offset below the 3 ms dead zone: EMA should decay (× 0.95 per call).
        // Force the throttle to always allow by resetting last_correction_at each call.
        for _ in 0..50 {
            c.last_correction_at = None;
            c.rate_correction(1.5);
        }
        assert!(
            c.smoothed_correction.abs() < 1e-3,
            "expected near-zero after decay, got {}",
            c.smoothed_correction
        );
    }

    #[test]
    fn rate_correction_3_to_15ms_band_is_nonzero() {
        // Regression guard: previously kp was ~0 at 3 ms, which left 7 ms
        // residuals persisting for many bars. Sweep the full band so a
        // future edit can't reintroduce a zero at an endpoint.
        for ms in [3.0, 7.0, 10.0, 14.0] {
            let mut c = fresh();
            c.last_correction_at = None;
            let r = c.rate_correction(ms);
            assert!(r.abs() > 1e-9, "expected non-zero at {ms}ms, got {r}");
            assert!(
                r.abs() < 0.01,
                "should stay clamped under 1% at {ms}ms, got {r}"
            );
        }
    }

    #[test]
    fn rate_correction_clamps_large_offsets() {
        let mut c = fresh();
        c.last_correction_at = None;
        let r = c.rate_correction(100.0);
        // First call with >20ms triggers the one-shot kick (up to ±3%).
        // At 100ms: 100 * 0.001 = 0.1, clamped to 0.03.
        assert!(
            r.abs() <= 0.03,
            "first-call kick should clamp to 3%, got {r}"
        );
        // Subsequent calls use normal proportional+EMA path (no kick).
        // The smoothed value decays from the kick's 0.03 over several beats.
        c.last_correction_at = None;
        let r2 = c.rate_correction(100.0);
        assert!(r2.abs() <= 0.03, "post-kick should stay bounded, got {r2}");
    }

    #[test]
    fn rate_correction_first_call_large_offset_returns_kick() {
        let mut c = fresh();
        let r = c.rate_correction(25.0);
        assert!(
            (r - 0.025).abs() < 1e-9,
            "25ms kick should be 0.025, got {r}"
        );

        let mut c = fresh();
        let r = c.rate_correction(100.0);
        assert!(
            (r - 0.03).abs() < 1e-9,
            "100ms kick should clamp to 0.03, got {r}"
        );

        let mut c = fresh();
        let r = c.rate_correction(-50.0);
        assert!(
            (r - (-0.03)).abs() < 1e-9,
            "-50ms kick should clamp to -0.03, got {r}"
        );
    }

    #[test]
    fn rate_correction_second_call_uses_ema_not_kick() {
        let mut c = fresh();
        let _kick = c.rate_correction(100.0);
        c.last_correction_at = None;
        let r2 = c.rate_correction(100.0);
        assert!(r2 < 0.03, "second call must use EMA (< 0.03), got {r2}");
    }

    #[test]
    fn rate_correction_first_call_small_offset_skips_kick() {
        for &ms in &[3.0_f64, 10.0, 15.0, 20.0] {
            let mut c = fresh();
            let r = c.rate_correction(ms);
            assert!(
                r.abs() <= 0.01 + 1e-9,
                "first call at {ms}ms must stay within ±1%, got {r}"
            );
        }
    }

    /// Repeatedly apply the same offset with the per-beat throttle bypassed,
    /// returning the converged correction. Mirrors what happens when the
    /// engine calls rate_correction once per beat across a sustained
    /// phase error — the production path we actually care about.
    fn converge(c: &mut CrossfadeController, offset_ms: f64, iters: u32) -> f64 {
        let mut last = 0.0;
        for _ in 0..iters {
            c.last_correction_at = None;
            last = c.rate_correction(offset_ms);
        }
        last
    }

    #[test]
    fn rate_correction_magnitude_monotonic_across_bands() {
        // Converged correction must grow (or stay equal) as the phase error
        // grows — otherwise the 3–15 / 15–50 / 50+ kp curves have a dip and
        // the controller under-corrects at some offsets. Guards against
        // off-by-constant tweaks to the kp ladder.
        let offsets = [4.0_f64, 8.0, 14.0, 25.0, 45.0, 75.0];
        let mut last_mag = 0.0;
        for &ms in &offsets {
            let mut c = fresh();
            let r = converge(&mut c, ms, 80).abs();
            assert!(
                r >= last_mag - 1e-9,
                "non-monotonic: offset {ms}ms gave {r}, previous band was {last_mag}"
            );
            last_mag = r;
        }
    }

    #[test]
    fn rate_correction_50ms_band_hits_upper_kp() {
        // At 60 ms we're in the flat-top kp=0.0005 band; converged correction
        // should approach kp*offset = 0.03, clamped to 0.01.
        let mut c = fresh();
        let r = converge(&mut c, 60.0, 200);
        assert!(
            (r.abs() - 0.01).abs() < 1e-4,
            "60ms offset should converge to the 1% clamp, got {r}"
        );
    }

    #[test]
    fn rate_correction_sign_tracks_offset() {
        // Positive offset → positive correction, negative → negative. This
        // is the invariant that keeps the deck chasing the right direction.
        let mut c = fresh();
        let pos = converge(&mut c, 20.0, 50);
        let mut c = fresh();
        let neg = converge(&mut c, -20.0, 50);
        assert!(
            pos > 0.0,
            "pos offset should give pos correction, got {pos}"
        );
        assert!(
            neg < 0.0,
            "neg offset should give neg correction, got {neg}"
        );
        assert!((pos + neg).abs() < 1e-9, "sign flip should be symmetric");
    }

    #[test]
    fn rate_correction_smoothing_flip_at_10ms_is_faster_above() {
        // Under 10ms we use 0.85 smoothing (slow), at/above 10ms we use 0.75
        // (faster response). After a single step from rest, the high-error
        // controller must have moved more of the way toward its clamp than
        // the low-error one — verifies the branch actually changes behavior.
        let step_low = {
            let mut c = fresh();
            c.last_correction_at = None;
            c.rate_correction(9.0).abs()
        };
        let step_high = {
            let mut c = fresh();
            c.last_correction_at = None;
            c.rate_correction(11.0).abs()
        };
        // Higher-offset step should cover more ground per tick even though
        // the kp at 9 vs 11 ms is nearly identical — the difference comes
        // from 1-smooth_factor flipping 0.15 → 0.25.
        assert!(
            step_high > step_low,
            "expected 11ms first step > 9ms first step, got high={step_high} low={step_low}"
        );
    }

    #[test]
    fn rate_correction_converges_to_steady_state() {
        // With a sustained offset the correction should settle — successive
        // values asymptotically equal. Checks the EMA actually converges
        // rather than wandering.
        let mut c = fresh();
        converge(&mut c, 20.0, 200);
        c.last_correction_at = None;
        let a = c.rate_correction(20.0);
        c.last_correction_at = None;
        let b = c.rate_correction(20.0);
        assert!((a - b).abs() < 1e-6, "not converged: a={a} b={b}");
    }

    #[test]
    fn duration_scales_inversely_with_bpm() {
        // duration() = 16 bars at the playing BPM. At 90 BPM one bar = 8/3 s,
        // at 128 BPM = 1.875 s, at 150 BPM = 1.6 s. Guards the playback-
        // length calculation used for crossfade scheduling.
        for &(bpm, expected) in &[
            (90.0_f64, 16.0 * 4.0 * 60.0 / 90.0),
            (128.0, 16.0 * 4.0 * 60.0 / 128.0),
            (150.0, 16.0 * 4.0 * 60.0 / 150.0),
        ] {
            let c = CrossfadeController::new(bpm, bpm, 16);
            assert!(
                (c.duration() - expected).abs() < 1e-6,
                "bpm={bpm}: expected {expected}s duration, got {}",
                c.duration()
            );
        }
    }

    #[test]
    fn rate_correction_feedback_loop_drives_offset_perceptually_below_10ms() {
        // Real mix-quality invariant: a sustained phase offset fed through
        // the correction → integrate → remeasure loop must shrink to the
        // dead-zone within one 16-bar crossfade (~50 beat ticks). The
        // geometric half-beat bound (`phase_offset_always_within_half_beat`)
        // says the controller is *stable*; this one says the controller
        // is *effective* — mixes shouldn't flam at the end of a bar.
        let mut c = CrossfadeController::new(128.0, 128.0, 16);
        // Simulate the incoming deck's rate being nudged by the correction
        // each beat. At 128 BPM, one beat = 468.75 ms. A correction `r`
        // applied for one beat pulls source time ahead by `r * beat_ms` ms,
        // which closes the offset by that amount.
        let beat_ms = 60_000.0 / 128.0;
        let mut offset_ms = 25.0_f64;
        for _ in 0..80 {
            c.last_correction_at = None;
            let r = c.rate_correction(offset_ms);
            offset_ms -= r * beat_ms;
        }
        assert!(
            offset_ms.abs() < 10.0,
            "controller failed to converge under 10ms: residual={offset_ms}ms"
        );
    }

    #[test]
    fn rate_correction_beat_throttle_holds_last_value() {
        // Inside one beat of the last correction the controller must return
        // the cached value — prevents 60 Hz wobble that jitters the deck.
        let mut c = CrossfadeController::new(128.0, 128.0, 16);
        c.last_correction_at = None;
        let first = c.rate_correction(20.0);
        // Immediately re-call without clearing last_correction_at → should
        // short-circuit to the cached value regardless of the new offset.
        let second = c.rate_correction(-50.0);
        assert_eq!(first, second, "within-beat call must return cached value");
    }
}
