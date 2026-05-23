/// Arithmetic beat grid — constant BPM, regular beat/bar intervals.
/// All times in seconds.
#[derive(Debug, Clone, Copy)]
pub struct BeatGrid {
    pub bpm: f64,
    pub first_beat_time: f64,
}

impl BeatGrid {
    pub fn beat_interval(&self) -> f64 {
        if self.bpm > 0.0 { 60.0 / self.bpm } else { 0.5 }
    }

    pub fn bar_interval(&self) -> f64 {
        self.beat_interval() * 4.0
    }

    pub fn bar_index(&self, time: f64) -> i64 {
        ((time - self.first_beat_time) / self.bar_interval()).floor() as i64
    }

    // -- Phase (0.0–1.0) --

    pub fn phase(&self, time: f64) -> f64 {
        let raw = (time - self.first_beat_time) / self.beat_interval();
        let m = raw % 1.0;
        if m < 0.0 { m + 1.0 } else { m }
    }

    pub fn bar_phase(&self, time: f64) -> f64 {
        let raw = (time - self.first_beat_time) / self.bar_interval();
        let m = raw % 1.0;
        if m < 0.0 { m + 1.0 } else { m }
    }

    #[allow(dead_code)] // used in #[cfg(test)] assertions only
    pub fn next_downbeat(&self, time: f64) -> f64 {
        // `ceil()` returns the same value when the input is already an
        // integer (i.e. `time` sits exactly on a bar boundary). Add a small
        // epsilon before the ceil so "exactly on a bar" advances to the
        // next bar rather than returning the current one — otherwise a
        // crossfade trigger that lands on beat 1 can stall by a full bar.
        let bars = (time - self.first_beat_time) / self.bar_interval();
        let bar_idx = (bars + 1e-9).ceil() as i64;
        self.first_beat_time + bar_idx as f64 * self.bar_interval()
    }

    /// Phase offset in seconds between two grids (beat-level).
    /// Positive = grid_a is ahead of grid_b.
    pub fn phase_offset(
        grid_a: &BeatGrid, time_a: f64,
        grid_b: &BeatGrid, time_b: f64,
    ) -> f64 {
        let phase_a = grid_a.phase(time_a);
        let phase_b = grid_b.phase(time_b);
        let mut delta = phase_a - phase_b;
        if delta > 0.5 { delta -= 1.0; }
        if delta < -0.5 { delta += 1.0; }
        delta * grid_a.beat_interval()
    }

    /// Which beat of the 4-beat bar we're in (0..=3). Bar beat 0 = the
    /// downbeat — what DJs mean when they say "line up the 1s."
    pub fn beat_in_bar(&self, time: f64) -> u32 {
        let raw = (time - self.first_beat_time) / self.beat_interval();
        // `rem_euclid` on f64 wraps negatives into [0, 4), so floor()
        // always gives 0..=3 — second rem_euclid was redundant.
        raw.rem_euclid(4.0).floor() as u32
    }

    /// Signed beat count needed to shift `grid_a` at `time_a` so its
    /// bar-position (which of the 4 beats in the bar) matches grid_b's
    /// at `time_b`. Returns the shortest-path rotation in ±2 beats so
    /// a 3-beat shift comes back as −1 beat.
    ///
    /// This is what `phase_offset` misses: two decks can be beat-aligned
    /// (same within-beat phase) but off by 1–3 beats in the bar — the
    /// classic "everything locks but the 1s don't hit together" trap.
    pub fn bar_offset_beats(
        grid_a: &BeatGrid, time_a: f64,
        grid_b: &BeatGrid, time_b: f64,
    ) -> i32 {
        let a = grid_a.beat_in_bar(time_a) as i32;
        let b = grid_b.beat_in_bar(time_b) as i32;
        let mut delta = b - a;
        if delta > 2 { delta -= 4; }
        if delta < -2 { delta += 4; }
        delta
    }

    /// Seconds to advance grid_a's time so its bar-position matches
    /// grid_b's. Combines `bar_offset_beats` × `beat_interval` so the
    /// engine can seek the incoming deck to land on the correct 1.
    /// Stays within ±2 beats of source time, so a "+3 beats" correction
    /// becomes a "−1 beat" seek.
    pub fn bar_aligned_seek_offset(
        grid_a: &BeatGrid, time_a: f64,
        grid_b: &BeatGrid, time_b: f64,
    ) -> f64 {
        Self::bar_offset_beats(grid_a, time_a, grid_b, time_b) as f64 * grid_a.beat_interval()
    }

    /// Signed shortest-path within-beat phase advance (in seconds) to
    /// shift a deck currently at `current_phase` so it ends up at
    /// `target_phase`, scaled by that deck's own `beat_interval`.
    ///
    /// Returns a value in `(-beat_interval/2, +beat_interval/2]`. The
    /// engine's phase-align step uses this to seek the incoming deck
    /// regardless of where it's currently sitting (first beat, mid-
    /// preview, quick-mix offset). Older code assumed `current_phase
    /// == 0`, which left a residual equal to the incoming's existing
    /// within-beat offset.
    pub fn phase_align_advance(
        target_phase: f64,
        current_phase: f64,
        beat_interval: f64,
    ) -> f64 {
        let mut delta = target_phase - current_phase;
        if delta >  0.5 { delta -= 1.0; }
        if delta < -0.5 { delta += 1.0; }
        delta * beat_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(bpm: f64, fbt: f64) -> BeatGrid {
        BeatGrid { bpm, first_beat_time: fbt }
    }

    #[test]
    fn phase_at_beat_boundaries() {
        let grid = g(120.0, 0.0); // beat_interval = 0.5s
        assert!((grid.phase(0.0) - 0.0).abs() < 1e-9);
        assert!((grid.phase(0.25) - 0.5).abs() < 1e-9);
        // One beat later should wrap to 0, not 1.
        assert!(grid.phase(0.5) < 1e-6 || grid.phase(0.5) > 0.999999);
    }

    #[test]
    fn phase_before_first_beat_wraps_into_0_1() {
        let grid = g(120.0, 1.0); // first beat at 1.0s
        // Half a beat before first_beat_time → phase 0.5 (via the m+1.0 branch).
        let p = grid.phase(0.75);
        assert!(p > 0.49 && p < 0.51, "expected ≈0.5, got {p}");
        // Exactly one full beat before → wraps to 0.
        let p = grid.phase(0.5);
        assert!(!(1e-6..=0.999999).contains(&p));
    }

    #[test]
    fn bar_phase_midpoint() {
        let grid = g(120.0, 0.0); // bar = 2.0s
        assert!((grid.bar_phase(1.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn next_downbeat_advances_on_exact_boundary() {
        // Regression: previously ceil(integer) returned the same bar so a
        // trigger on an exact downbeat didn't advance. Epsilon fix.
        let grid = g(120.0, 0.0); // bar = 2.0s
        // Time exactly on bar 2 (t=4.0) should return bar 3 at t=6.0, not 4.0.
        let nd = grid.next_downbeat(4.0);
        assert!((nd - 6.0).abs() < 1e-6, "expected 6.0, got {nd}");
    }

    #[test]
    fn phase_offset_identical_grids_is_zero() {
        let grid = g(128.0, 1.0);
        let off = BeatGrid::phase_offset(&grid, 10.0, &grid, 10.0);
        assert!(off.abs() < 1e-9, "identical grids should give 0.0, got {off}");
    }

    #[test]
    fn phase_offset_same_bpm_time_shift_recovers_shift() {
        // Sanity: at equal BPM, offsetting time_b by +5ms means grid_a is
        // effectively 5ms AHEAD of grid_b — phase_offset should return ~+0.005s
        // across the full BPM range we ship.
        for &bpm in &[90.0_f64, 128.0, 150.0, 180.0] {
            let grid = g(bpm, 0.0);
            let off = BeatGrid::phase_offset(&grid, 1.0, &grid, 0.995);
            assert!((off - 0.005).abs() < 1e-6,
                "bpm={bpm}: expected +5ms, got {} ms", off * 1000.0);
        }
    }

    #[test]
    fn phase_offset_always_within_half_beat() {
        // The wrap-to-shortest-path branches guarantee |delta| ≤ 0.5 phase
        // units, so the returned offset magnitude must be ≤ half a beat of
        // grid_a. Verify across mismatched BPM pairs — this is the guarantee
        // the rate_correction controller depends on to stay stable.
        let pairs = [(90.0_f64, 128.0), (128.0, 150.0), (150.0, 90.0), (128.0, 180.0)];
        for (a, b) in pairs {
            let grid_a = g(a, 0.0);
            let grid_b = g(b, 0.123); // arbitrary offset
            // Sample across a bar to exercise all phase relationships.
            let bar_a = grid_a.bar_interval();
            for step in 0..40 {
                let t = step as f64 * bar_a / 40.0;
                let off = BeatGrid::phase_offset(&grid_a, t, &grid_b, t);
                let half_beat_a = grid_a.beat_interval() * 0.5 + 1e-9;
                assert!(off.abs() <= half_beat_a,
                    "a={a} b={b} t={t}: |off|={} > half_beat_a={half_beat_a}", off.abs());
            }
        }
    }

    #[test]
    fn phase_stays_in_unit_interval_across_bpm_range() {
        // Regression guard: phase() must return [0,1) for any time/BPM —
        // we've been burned before when negative-time wrap was off-by-epsilon.
        for &bpm in &[60.0_f64, 90.0, 128.0, 150.0, 180.0, 220.0] {
            let grid = g(bpm, 0.7);
            for step in -50..500 {
                let t = step as f64 * 0.037; // irregular step to avoid lucky multiples
                let p = grid.phase(t);
                assert!((0.0..1.0).contains(&p) || (p - 1.0).abs() < 1e-9,
                    "bpm={bpm} t={t}: phase={p} out of [0,1)");
                let bp = grid.bar_phase(t);
                assert!((0.0..1.0).contains(&bp) || (bp - 1.0).abs() < 1e-9,
                    "bpm={bpm} t={t}: bar_phase={bp} out of [0,1)");
            }
        }
    }

    #[test]
    fn beat_interval_matches_bpm_across_range() {
        for &bpm in &[90.0_f64, 128.0, 150.0, 180.0] {
            let grid = g(bpm, 0.0);
            assert!((grid.beat_interval() - 60.0 / bpm).abs() < 1e-12);
            assert!((grid.bar_interval() - 4.0 * 60.0 / bpm).abs() < 1e-12);
        }
    }

    #[test]
    fn beat_in_bar_cycles_0_to_3() {
        let grid = g(120.0, 0.0); // beat = 0.5s, bar = 2.0s
        assert_eq!(grid.beat_in_bar(0.0), 0, "downbeat = 0");
        assert_eq!(grid.beat_in_bar(0.5), 1);
        assert_eq!(grid.beat_in_bar(1.0), 2);
        assert_eq!(grid.beat_in_bar(1.5), 3);
        assert_eq!(grid.beat_in_bar(2.0), 0, "next bar wraps back to downbeat");
        assert_eq!(grid.beat_in_bar(2.5), 1);
        // Negative time: still lives in 0..=3.
        assert!(grid.beat_in_bar(-0.25) <= 3);
    }

    #[test]
    fn bar_offset_beats_detects_the_1s_mismatch() {
        // Classic "off by N beats" scenario — two tracks at the same BPM,
        // same within-beat phase, but sitting on different beats of the
        // bar. Auto-DJ's phase_offset wraps within ±0.5 beat and misses
        // this; bar_offset_beats must return the signed beat distance.
        let grid = g(120.0, 0.0);
        // a at downbeat (beat 0), b at beat 2 → +2 beat shift → shortest
        // path can go +2 or −2; we resolve to +2 (deterministic).
        let shift = BeatGrid::bar_offset_beats(&grid, 4.0, &grid, 5.0);
        assert!(shift == 2 || shift == -2, "expected ±2, got {shift}");

        // a at beat 1, b at beat 0 → shortest is −1.
        assert_eq!(BeatGrid::bar_offset_beats(&grid, 0.5, &grid, 0.0), -1);
        // a at beat 0, b at beat 3 → shortest is −1 (not +3).
        assert_eq!(BeatGrid::bar_offset_beats(&grid, 0.0, &grid, 1.5), -1);
        // Already aligned → 0.
        assert_eq!(BeatGrid::bar_offset_beats(&grid, 2.0, &grid, 0.0), 0);
    }

    #[test]
    fn phase_align_advance_zeroes_when_phases_match() {
        // Common path: incoming sitting at first_beat (phase 0) and
        // playing at phase 0. No seek needed.
        let adv = BeatGrid::phase_align_advance(0.0, 0.0, 0.5);
        assert!(adv.abs() < 1e-12, "matching phases must give 0, got {adv}");
        // Also true mid-beat: both at phase 0.42.
        let adv = BeatGrid::phase_align_advance(0.42, 0.42, 0.5);
        assert!(adv.abs() < 1e-12, "matching mid-beat phases must give 0, got {adv}");
    }

    #[test]
    fn phase_align_advance_handles_nonzero_incoming_phase() {
        // The bug the new helper fixes: old code assumed incoming was
        // at phase 0. Here incoming is at phase 0.6 and playing at 0.0.
        // Shortest path: shift incoming FORWARD by 0.4 beats so its
        // next-beat lands on the next playing beat (both at phase 0).
        let interval = 60.0 / 128.0; // ≈0.469s
        let adv = BeatGrid::phase_align_advance(0.0, 0.6, interval);
        let expected = 0.4 * interval; // forward (shorter than -0.6 backward)
        assert!((adv - expected).abs() < 1e-9,
            "playing=0, incoming=0.6 → expected {expected}, got {adv}");

        // Mirror: incoming at 0.3, playing at 0.0 → backward 0.3 (shorter
        // than forward 0.7).
        let adv = BeatGrid::phase_align_advance(0.0, 0.3, interval);
        let expected = -0.3 * interval;
        assert!((adv - expected).abs() < 1e-9,
            "playing=0, incoming=0.3 → expected {expected}, got {adv}");
    }

    #[test]
    fn phase_align_advance_picks_shortest_path() {
        // Whether to seek forward or backward must minimize |advance|.
        // A 0.9-phase delta should resolve as −0.1, not +0.9.
        let interval = 0.5;
        let adv = BeatGrid::phase_align_advance(0.95, 0.05, interval);
        // delta = 0.9 → wraps to −0.1
        assert!((adv - (-0.1 * interval)).abs() < 1e-9,
            "0.9 delta should resolve as −0.1, got {adv}");
        // Symmetric: −0.9 delta wraps to +0.1.
        let adv = BeatGrid::phase_align_advance(0.05, 0.95, interval);
        assert!((adv - (0.1 * interval)).abs() < 1e-9,
            "−0.9 delta should resolve as +0.1, got {adv}");
    }

    #[test]
    fn phase_align_advance_bounded_by_half_beat() {
        // Output magnitude must never exceed half a beat — guarantees
        // the seek can't walk past the next/previous beat boundary.
        let interval = 60.0 / 128.0;
        for target_x100 in 0..100 {
            for current_x100 in 0..100 {
                let target = target_x100 as f64 / 100.0;
                let current = current_x100 as f64 / 100.0;
                let adv = BeatGrid::phase_align_advance(target, current, interval);
                assert!(adv.abs() <= interval * 0.5 + 1e-9,
                    "target={target} current={current}: |adv|={} > half-beat", adv.abs());
            }
        }
    }

    #[test]
    fn phase_align_advance_negative_seek_scenario() {
        // Regression: the trainwreck-every-other-mix bug. Incoming at
        // position ~0 (just loaded at first_beat), playing at phase 0.94.
        // Shortest path = -0.06 beats backward → advance is -28ms.
        // Engine must detect inc_time + advance < 0 and flip to the
        // +0.94 forward path instead. This test validates that the
        // raw advance IS negative (so the engine's fallback fires).
        let interval = 60.0 / 128.0; // 0.469s
        let adv = BeatGrid::phase_align_advance(0.94, 0.0, interval);
        assert!(adv < 0.0,
            "shortest path for target=0.94 current=0.0 should be backward, got {adv}");
        assert!((adv - (-0.06 * interval)).abs() < 1e-6,
            "expected -0.06 * interval = {}, got {adv}", -0.06 * interval);
        // The forward alternative (what the engine uses when backward
        // would go past buffer start) is advance + beat_interval.
        let forward = adv + interval;
        assert!(forward > 0.0);
        assert!((forward - 0.94 * interval).abs() < 1e-6);
    }

    #[test]
    fn bar_aligned_seek_offset_matches_beat_interval() {
        // Applying the returned offset to grid_a's time must put it on
        // the same beat-in-bar as grid_b.
        let grid = g(128.0, 0.0);
        let t_a = 3.17; // arbitrary
        let t_b = 7.89;
        let offset = BeatGrid::bar_aligned_seek_offset(&grid, t_a, &grid, t_b);
        assert_eq!(grid.beat_in_bar(t_a + offset), grid.beat_in_bar(t_b),
            "after applying seek offset, beat_in_bar must match");
        // Offset must be within ±2 beats of source time.
        assert!(offset.abs() <= 2.0 * grid.beat_interval() + 1e-9,
            "offset {offset} exceeds ±2 beats");
    }

    #[test]
    fn bar_align_and_phase_offset_are_independent_signals() {
        // Crucial invariant: two grids at identical BPM with an integer
        // beat offset have *zero* phase_offset (beats line up) but
        // nonzero bar_offset_beats. Proves the bar check catches what
        // phase_offset can't.
        let grid = g(128.0, 0.0);
        let bi = grid.beat_interval();
        // grid_a at time t, grid_b at t + 2 beats (same deck, 2 beats earlier).
        let t = 1.0;
        let phase = BeatGrid::phase_offset(&grid, t, &grid, t + 2.0 * bi);
        let bar = BeatGrid::bar_offset_beats(&grid, t, &grid, t + 2.0 * bi);
        assert!(phase.abs() < 1e-9, "beats line up, phase should be 0; got {phase}");
        assert!(bar != 0, "bars don't line up, bar_offset should be nonzero; got {bar}");
    }

    #[test]
    fn next_downbeat_advances_across_bpm_range() {
        // The epsilon fix must hold at every BPM — regression caught the
        // 128 BPM case originally; at 90/150/180 the absolute bar size
        // differs and we want to confirm the fix isn't tuned to 128.
        for &bpm in &[90.0_f64, 128.0, 150.0, 180.0] {
            let grid = g(bpm, 0.0);
            let bar = grid.bar_interval();
            // Exactly on bar 3 — must return bar 4.
            let nd = grid.next_downbeat(3.0 * bar);
            assert!((nd - 4.0 * bar).abs() < 1e-6,
                "bpm={bpm}: expected {}, got {nd}", 4.0 * bar);
        }
    }

    #[test]
    fn beat_in_bar_with_nonzero_first_beat_time() {
        let grid = g(120.0, 1.0); // first beat at 1.0s, beat = 0.5s
        assert_eq!(grid.beat_in_bar(1.0), 0, "at first_beat_time → beat 0");
        assert_eq!(grid.beat_in_bar(1.5), 1);
        assert_eq!(grid.beat_in_bar(2.0), 2);
        assert_eq!(grid.beat_in_bar(2.5), 3);
        assert_eq!(grid.beat_in_bar(3.0), 0, "next bar wraps");
    }

    #[test]
    fn bar_offset_beats_uses_each_grids_own_beat_interval() {
        let ga = g(120.0, 0.0); // beat = 0.5s
        let gb = g(130.0, 0.3); // beat = 0.462s, first_beat offset
        // ga at 2.0s: beat_in_bar = ((2.0 - 0.0) / 0.5) % 4 = 4.0 % 4 = 0
        // gb at 2.0s: beat_in_bar = ((2.0 - 0.3) / 0.462) % 4 = 3.68 % 4 = 3.68 → floor = 3
        let shift = BeatGrid::bar_offset_beats(&ga, 2.0, &gb, 2.0);
        // delta = b(3) - a(0) = 3 → wraps to -1
        assert_eq!(shift, -1, "cross-BPM bar offset should use each grid's own interval");
    }

    #[test]
    fn phase_align_advance_at_exactly_half_delta_goes_forward() {
        let interval = 0.5;
        let adv = BeatGrid::phase_align_advance(0.75, 0.25, interval);
        // delta = 0.5 — exactly half. Convention: > 0.5 wraps, so 0.5 stays positive.
        assert!(adv > 0.0, "delta=0.5 should go forward, got {adv}");
        assert!((adv - 0.5 * interval).abs() < 1e-9);
    }

    #[test]
    fn phase_align_advance_forward_fallback_arithmetic() {
        // Verify: advance + beat_interval lands on the correct phase
        // for all targets where advance is negative.
        let interval = 60.0 / 128.0;
        for target_pct in 51..100 {
            let target = target_pct as f64 / 100.0;
            let adv = BeatGrid::phase_align_advance(target, 0.0, interval);
            assert!(adv < 0.0, "target={target}: shortest path should be negative");
            let forward = adv + interval;
            assert!((forward - target * interval).abs() < 1e-9,
                "target={target}: advance+interval should equal target*interval");
        }
    }

    #[test]
    fn bar_aligned_seek_offset_negative_plus_bar_interval_is_valid_fallback() {
        let grid = g(128.0, 0.0);
        // Force a scenario where bar_seek is negative: playing at beat 3,
        // incoming at beat 0.
        let p_time = 3.0 * grid.beat_interval(); // beat 3
        let i_time = 0.0; // beat 0
        let raw = BeatGrid::bar_aligned_seek_offset(&grid, i_time, &grid, p_time);
        assert!(raw < 0.0, "should need backward seek, got {raw}");
        let fallback = raw + grid.bar_interval();
        assert!(fallback > 0.0, "fallback must be positive, got {fallback}");
        // After fallback seek, beat_in_bar should match.
        assert_eq!(grid.beat_in_bar(i_time + fallback), grid.beat_in_bar(p_time),
            "fallback seek must land on the same beat_in_bar");
    }
}
