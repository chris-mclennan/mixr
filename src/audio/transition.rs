use std::f64::consts::FRAC_PI_2;
use super::deck::DeckPlayer;
use super::mixer::Mixer;

/// Transition types control how two decks blend during a crossfade.
/// Each type defines: prepare (one-shot setup), apply (per-tick automation),
/// and volume curves (per-sample in audio callback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionType {
    /// Equal-power crossfade with beat sync. Duration: crossfade_bars.
    ///
    /// ```text
    /// playing_volume:  cos(pi/2 * progress)  — equal-power fade out
    /// incoming_volume: sin(pi/2 * progress)  — equal-power fade in
    /// incoming_rate:   playing_bpm / incoming_bpm
    /// phase_sync:      proportional correction, EMA smoothed, ±1% cap, 3ms dead zone
    /// ```
    BeatMatched,

    /// Echo tail on playing deck, hard cut, then bring in incoming.
    /// Duration: ~8.25 bars (0.515625 * crossfade_bars).
    EchoOut,

    /// Both decks at full volume; swap EQ lows at the midpoint for a classic
    /// bass-swap drop moment, then equal-power crossfade the top end.
    /// Duration: crossfade_bars. Requires beat-sync.
    BassSwap,

    /// Playing deck's filter sweeps up to HP while incoming's filter sweeps
    /// from LP up to bypass. Equal-power crossfade on top. Beat-sync.
    /// Duration: crossfade_bars.
    FilterSweep,

    /// Loop the playing deck's current bar while bringing in the incoming.
    /// Useful when the outgoing track runs out of runway — the 4-beat loop
    /// stalls it so the incoming has room to fade in. Beat-sync.
    /// Duration: crossfade_bars.
    LoopRoll,
}

impl TransitionType {
    /// Auto-select transition based on BPM + Camelot key compatibility.
    /// Mismatched BPMs → EchoOut. Matched BPMs route by key distance:
    ///   0–1 (same / ±1 step) → BassSwap (drop-swap works with harmony)
    ///   2            → FilterSweep (smooth mood shift)
    ///   else         → BeatMatched (safe equal-power)
    pub fn choose(
        playing_bpm: f64,
        incoming_bpm: f64,
        playing_key: Option<&str>,
        incoming_key: Option<&str>,
    ) -> Self {
        let min_bpm = playing_bpm.min(incoming_bpm);
        if min_bpm < 1.0 {
            return Self::EchoOut;
        }
        let ratio = playing_bpm.max(incoming_bpm) / min_bpm;
        let normalized = if (1.8..=2.2).contains(&ratio) { ratio / 2.0 } else { ratio };
        if normalized > 1.08 {
            return Self::EchoOut;
        }
        match (playing_key, incoming_key) {
            (Some(a), Some(b)) => match camelot_distance(a, b) {
                0 | 1 => Self::BassSwap,
                2 => Self::FilterSweep,
                _ => Self::BeatMatched,
            },
            _ => Self::BeatMatched,
        }
    }

    /// Duration relative to crossfade_bars.
    pub fn duration_multiplier(&self) -> f64 {
        match self {
            Self::BeatMatched | Self::BassSwap | Self::FilterSweep | Self::LoopRoll => 1.0,
            Self::EchoOut => 0.515625, // 8 bars + 1 beat
        }
    }

    /// Fixed bar count for transitions whose length shouldn't scale with
    /// the user's global `crossfade_bars` setting. EchoOut is a hard
    /// cut + echo tail by design — stretching it to "16 bars of slow
    /// echo" at crossfade_bars=32 turns it into a slow fade, which
    /// isn't what the transition is for. Returning `Some(n)` lets
    /// `start_crossfade` ignore the multiplier and use `n` directly.
    pub fn absolute_bars(&self) -> Option<u32> {
        match self {
            // Fixed 8-bar echo-out regardless of the user's global
            // crossfade_bars setting. The visual-envelope mismatch
            // on longer settings was the real complaint — now solved
            // by the updated `fader_position` sweep — not the length.
            Self::EchoOut => Some(8),
            _ => None,
        }
    }

    // ── One-shot: called once at crossfade start ──

    /// Set up deck state for this transition. Called once when crossfade
    /// begins. Any variant that *writes* EQ / filter on the incoming deck
    /// is paired with a neutral reset in the variants that don't — so if
    /// the rule engine (or an IPC override) swaps transition types between
    /// preparations, stale kills don't bleed into the next mix. Without
    /// this, a prior `BassSwap::prepare` leaving `eq_low=-24` would keep
    /// the bass dropped throughout a subsequent BeatMatched.
    pub fn prepare(&self, playing: &mut DeckPlayer, incoming: &mut DeckPlayer, playing_bpm: f64, _incoming_bpm: f64) {
        // Defensive reset: every transition starts from a clean incoming
        // tone stack. Variants that want a non-neutral start override below.
        Mixer::set_eq_low(incoming, 0.0);
        Mixer::set_eq_mid(incoming, 0.0);
        Mixer::set_eq_high(incoming, 0.0);
        Mixer::set_filter(incoming, 0.0);

        match self {
            Self::BeatMatched => {
                Mixer::match_bpm(incoming, playing_bpm);
                Mixer::play(incoming);
            }
            Self::EchoOut => {
                Mixer::set_rate(incoming, 1.0);
                Mixer::set_delay_wet(playing, 1.0);
                // Don't start incoming yet — incoming_volume is 0
            }
            Self::BassSwap => {
                Mixer::match_bpm(incoming, playing_bpm);
                // Incoming starts with lows killed so the drop swap is dramatic.
                Mixer::set_eq_low(incoming, -24.0);
                Mixer::play(incoming);
            }
            Self::FilterSweep => {
                Mixer::match_bpm(incoming, playing_bpm);
                // Incoming starts fully low-passed; reveals as the sweep progresses.
                Mixer::set_filter(incoming, -1.0);
                Mixer::play(incoming);
            }
            Self::LoopRoll => {
                Mixer::match_bpm(incoming, playing_bpm);
                // 4-beat loop on the outgoing deck so it keeps churning while
                // incoming fades in. Loop is released before the crossfade ends.
                Mixer::loop_beats(playing, 4.0);
                Mixer::play(incoming);
            }
        }
    }

    // ── Per-tick: called every tick during crossfade ──

    /// Update deck parameters based on crossfade progress. Called every tick.
    pub fn apply(&self, progress: f64, playing: &mut DeckPlayer, incoming: &mut DeckPlayer) {
        match self {
            Self::BeatMatched => {
                // Phase sync correction is handled separately by CrossfadeController
            }
            Self::EchoOut => {
                Mixer::set_delay_wet(playing, echoout_delay_wet(progress));
            }
            Self::BassSwap => {
                // Crossfade the lows over a tight window at the midpoint.
                // Before: playing has lows, incoming doesn't. After: reversed.
                // Shape: -24 dB (kill) to 0 dB (unity) via cosine easing.
                let (playing_low, incoming_low) = if progress < 0.48 {
                    (0.0, -24.0)
                } else if progress < 0.52 {
                    let t = (progress - 0.48) / 0.04; // 0..1 across the swap
                    // Cosine ease: smooth-in smooth-out
                    let eased = 0.5 - 0.5 * (std::f64::consts::PI * t).cos();
                    let down = -24.0 * eased;       // 0 → -24
                    let up = -24.0 + 24.0 * eased;  // -24 → 0
                    (down, up)
                } else {
                    (-24.0, 0.0)
                };
                Mixer::set_eq_low(playing, playing_low as f32);
                Mixer::set_eq_low(incoming, incoming_low as f32);
            }
            Self::FilterSweep => {
                // Playing sweeps from bypass (0) up to full HP (+1).
                // Incoming sweeps from full LP (-1) up to bypass (0).
                // Meet in the middle around progress 0.5.
                let p = progress as f32;
                Mixer::set_filter(playing, p);
                Mixer::set_filter(incoming, -1.0 + p);
            }
            Self::LoopRoll => {
                // Release the loop near the end so the playing deck can fade
                // out naturally instead of ending abruptly mid-loop.
                if progress >= 0.85 && playing.loop_active {
                    Mixer::loop_release(playing);
                }
            }
        }

        // Start incoming deck when its volume first goes above 0.
        // Skip when the user has paused (deck.paused=true) — otherwise the
        // auto-start here fires every tick and effectively cancels a pause
        // during a crossfade.
        let iv = self.incoming_volume(progress);
        if iv > 0.0 && !incoming.playing && !incoming.paused && incoming.is_loaded() {
            Mixer::play(incoming);
            // Deliberately at debug: this runs inside tick() while the audio
            // state mutex is held; any tracing subscriber that does file I/O
            // would block the audio callback. Use `RUST_LOG=mixr=debug` if
            // you need the event.
            tracing::debug!("Incoming deck started at progress {progress:.2}");
        }
    }

    // ── Per-sample: called in audio callback ──

    /// Playing deck volume at this progress.
    pub fn playing_volume(&self, progress: f64) -> f32 {
        match self {
            Self::BeatMatched | Self::FilterSweep | Self::LoopRoll => (FRAC_PI_2 * progress).cos() as f32,
            // Very fast cut — 0.5% ramp to avoid single-sample click
            Self::EchoOut => if progress < 0.005 { (1.0 - progress / 0.005) as f32 } else { 0.0 },
            // Both full until the last ~10%, then fade the (now bass-less) playing out.
            Self::BassSwap => if progress < 0.9 { 1.0 } else {
                let t = (progress - 0.9) / 0.1;
                (FRAC_PI_2 * t).cos() as f32
            },
        }
    }

    /// Incoming deck volume at this progress.
    pub fn incoming_volume(&self, progress: f64) -> f32 {
        match self {
            Self::BeatMatched | Self::FilterSweep | Self::LoopRoll => (FRAC_PI_2 * progress).sin() as f32,
            Self::EchoOut => if progress < 0.40625 {
                0.0
            } else if progress < 0.41125 {
                // Click-free ramp 0 → 0.9 over ~0.5% of the crossfade (~40 ms
                // at 8-bar / 128 BPM). Fast enough to still feel like a drop,
                // continuous enough to avoid the step discontinuity.
                let r = (progress - 0.40625) / 0.005;
                (0.9 * r) as f32
            } else if progress < 0.75 {
                let fade = (progress - 0.41125) / (0.75 - 0.41125);
                (0.9 + 0.1 * fade) as f32
            } else {
                1.0
            },
            // Incoming at full from the very start (lows are EQ'd out until the swap).
            Self::BassSwap => 1.0,
        }
    }

    pub fn use_phase_sync(&self) -> bool {
        matches!(self, Self::BeatMatched | Self::BassSwap | Self::FilterSweep | Self::LoopRoll)
    }

    /// Whether crossfade progress is driven by wall-clock time rather
    /// than the playing deck's source-time delta. True for the two
    /// transitions that decouple the playing deck from the audible mix
    /// — LoopRoll locks it in a 4-beat loop, EchoOut hard-cuts it to
    /// silence — so a playing deck that stops advancing (loop, pause,
    /// end-of-track) can't freeze the crossfade. The other transitions
    /// stay on source time so a pause freezes the needle correctly.
    /// Drives the `new_progress` computation in `engine.rs::tick`.
    pub fn progress_from_wall_clock(&self) -> bool {
        matches!(self, Self::LoopRoll | Self::EchoOut)
    }

    /// Visual crossfader position (0.0 = full A/playing, 1.0 = full B/incoming).
    /// Models where the *fader itself* is, not which deck is audible —
    /// EchoOut slams the fader over at the cut even though the incoming is
    /// silent until later (echo tail is post-fader).
    pub fn fader_position(&self, progress: f64) -> f64 {
        match self {
            // Smooth equal-power sweep matching the sin volume curve.
            Self::BeatMatched | Self::FilterSweep | Self::LoopRoll => (std::f64::consts::FRAC_PI_2 * progress).sin(),
            // Fader sweeps A → B during the echo tail (0..40% of
            // progress) and holds at B while incoming fades up.
            // Matches what a DJ's hand actually does: move the fader
            // over while the echo rings, then it's just sitting there
            // as the new track drops.
            Self::EchoOut => if progress < 0.40 {
                let t = progress / 0.40;
                (FRAC_PI_2 * t).sin()
            } else { 1.0 },
            // Needle steps at the bass-swap midpoint, then sweeps the fade-out tail.
            Self::BassSwap => if progress < 0.48 {
                progress * (0.5 / 0.48)
            } else if progress < 0.52 {
                0.5 + (progress - 0.48) / 0.04 * 0.4
            } else if progress < 0.9 {
                0.9 + (progress - 0.52) / 0.38 * 0.05
            } else {
                0.95 + (progress - 0.9) / 0.1 * 0.05
            },
        }
    }
}

/// EchoOut delay-wet curve. Held at 1.0 for the first 12.5% of the
/// crossfade (echo tail rings out at full level), then quadratic decay
/// `(1-fade)²` from 12.5%-50%, then 0 for the back half (incoming
/// fades up cleanly with no residual echo). Pure function so the
/// shape can be unit-tested without a DeckPlayer.
pub fn echoout_delay_wet(progress: f64) -> f32 {
    if progress < 0.125 {
        1.0
    } else if progress < 0.4999 {
        let fade = (progress - 0.125) / 0.3749;
        let curve = (1.0 - fade) * (1.0 - fade);
        curve as f32
    } else {
        0.0
    }
}

/// Camelot distance on the wheel (0..=6). Returns 99 if either key
/// unparseable. Used by `TransitionType::choose` for routing and by
/// the queue-track compat feedback in the TUI bridge.
pub(crate) fn camelot_distance(a: &str, b: &str) -> usize {
    fn p(k: &str) -> Option<(i32, u8)> {
        let k = k.trim();
        let l = *k.as_bytes().last()?;
        if l != b'A' && l != b'B' { return None; }
        k[..k.len()-1].parse().ok().map(|n| (n, l))
    }
    let (na, la) = match p(a) { Some(v) => v, None => return 99 };
    let (nb, lb) = match p(b) { Some(v) => v, None => return 99 };
    let wheel = |n: i32, m: i32| {
        let d = (n - m).unsigned_abs() as usize;
        d.min(12 - d)
    };
    if la == lb {
        wheel(na, nb)
    } else if na == nb {
        1
    } else {
        wheel(na, nb) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beatmatched_volumes_are_equal_power() {
        // At any progress p, sin² + cos² ≈ 1 (equal-power curve).
        let t = TransitionType::BeatMatched;
        for i in 0..=20 {
            let p = i as f64 / 20.0;
            let pv = t.playing_volume(p) as f64;
            let iv = t.incoming_volume(p) as f64;
            let sum = pv * pv + iv * iv;
            assert!((sum - 1.0).abs() < 0.01, "p={p} sum²={sum}");
        }
    }

    #[test]
    fn progress_from_wall_clock_only_for_looproll_and_echoout() {
        // Regression: LoopRoll + EchoOut decouple the playing deck from
        // the audible mix, so their crossfade progress must run on
        // wall-clock — a playing deck that stops advancing (loop /
        // end-of-track) must not freeze the crossfade. Dropping EchoOut
        // from this set is exactly the "echo never decays, decks never
        // swap" stuck-transition bug.
        assert!(TransitionType::LoopRoll.progress_from_wall_clock());
        assert!(TransitionType::EchoOut.progress_from_wall_clock());
        // The phase-synced transitions stay on playing-deck source time
        // so a pause freezes the needle.
        assert!(!TransitionType::BeatMatched.progress_from_wall_clock());
        assert!(!TransitionType::BassSwap.progress_from_wall_clock());
        assert!(!TransitionType::FilterSweep.progress_from_wall_clock());
    }

    #[test]
    fn echoout_fader_sweeps_then_holds() {
        // EchoOut used to slam the fader over in 0.5% of progress.
        // Now it sweeps A → B over the first 40% (echo-tail window)
        // so the visual matches a DJ's actual hand motion, then holds
        // at B while incoming fades up.
        let t = TransitionType::EchoOut;
        assert!(t.fader_position(0.0) <= 0.01);
        // Mid-sweep: well past 0, well shy of 1.
        let mid = t.fader_position(0.20);
        assert!((0.3..0.9).contains(&mid), "expected mid-sweep ≈ 0.3..0.9, got {mid}");
        // By 40% we should be at (or very near) full B.
        assert!((t.fader_position(0.40) - 1.0).abs() < 1e-6);
        // Held at B for the rest of the crossfade.
        assert!((t.fader_position(0.5) - 1.0).abs() < 1e-6);
        assert!((t.fader_position(1.0) - 1.0).abs() < 1e-6);
    }

    // bassswap_is_full_volume_through_midpoint was subsumed by the
    // incoming_volumes_monotonic_non_decreasing +
    // bassswap_playing_volume_continuous_at_swap_boundary sweeps below.

    #[test]
    fn choose_echoout_threshold_is_at_8_percent() {
        // choose() picks EchoOut when the BPM gap exceeds 8%. Walk across
        // the threshold to pin the behavior — a ratio of 1.07 must stay
        // BeatMatched, 1.09 must flip to EchoOut. Catches accidental
        // constant tweaks that would silently shift the routing.
        let base = 128.0;
        // Just under 8% — matched.
        assert_eq!(TransitionType::choose(base, base * 1.07, None, None), TransitionType::BeatMatched);
        // Just over 8% — EchoOut.
        assert_eq!(TransitionType::choose(base, base * 1.09, None, None), TransitionType::EchoOut);
        // Large gap — EchoOut.
        assert_eq!(TransitionType::choose(base, base * 1.5, None, None), TransitionType::EchoOut);
        // Symmetric on the negative side.
        assert_eq!(TransitionType::choose(base, base * 0.91, None, None), TransitionType::EchoOut);
    }

    #[test]
    fn choose_falls_back_to_beatmatched_when_keys_unknown() {
        assert_eq!(TransitionType::choose(128.0, 130.0, None, None), TransitionType::BeatMatched);
    }

    #[test]
    fn choose_routes_by_camelot_distance() {
        // Matched BPMs, same key → BassSwap
        assert_eq!(TransitionType::choose(128.0, 130.0, Some("8A"), Some("8A")), TransitionType::BassSwap);
        // Dist 2 → FilterSweep
        assert_eq!(TransitionType::choose(128.0, 130.0, Some("8A"), Some("10A")), TransitionType::FilterSweep);
        // Far key → BeatMatched
        assert_eq!(TransitionType::choose(128.0, 130.0, Some("8A"), Some("2A")), TransitionType::BeatMatched);
    }

    #[test]
    fn double_tempo_boundary_is_inclusive() {
        // Previously `ratio > 1.8 && ratio < 2.2` (strict) skipped halving
        // at the exact endpoints, so ratio=1.8 or 2.0 would be treated as
        // a ~100% BPM gap and fire EchoOut. With inclusive bounds, both
        // ratio=1.8 (halves to 0.9) and ratio=2.0 (halves to 1.0) stay
        // inside the ≤1.08 normalized gap and choose BeatMatched.
        assert_eq!(TransitionType::choose(128.0, 256.0, None, None), TransitionType::BeatMatched);
        assert_eq!(TransitionType::choose(128.0, 230.4, None, None), TransitionType::BeatMatched);
        // ratio just past 2.2 halves to > 1.1 and correctly fires EchoOut.
        assert_eq!(TransitionType::choose(128.0, 300.0, None, None), TransitionType::EchoOut);
    }

    #[test]
    fn echoout_incoming_volume_no_step_at_ramp_start() {
        // Regression: before the fix, incoming_volume jumped 0 → 0.9 at
        // progress=0.40625. After the fix it ramps 0 → 0.9 over ~0.5%.
        let t = TransitionType::EchoOut;
        assert_eq!(t.incoming_volume(0.40624), 0.0);
        // Just past the ramp start should still be near zero (monotonic rise).
        let just_past = t.incoming_volume(0.40626);
        assert!((0.0..0.1).contains(&just_past),
            "expected tiny ramp value near zero, got {just_past}");
        // By the end of the short ramp we've reached 0.9.
        let after_ramp = t.incoming_volume(0.41125);
        assert!((after_ramp - 0.9).abs() < 0.01,
            "expected ~0.9 after ramp, got {after_ramp}");
        // And it continues up to 1.0 by progress 0.75.
        assert!((t.incoming_volume(0.75) - 1.0).abs() < 0.01);
    }

    #[test]
    fn filter_sweep_and_loop_roll_share_equal_power_curve() {
        // Guards the match arm that groups these three under the same sin/cos
        // curve — if someone splits them out without rewriting the curve,
        // equal-power is trivially broken and mix loudness will dip/bump.
        for t in [TransitionType::FilterSweep, TransitionType::LoopRoll] {
            for i in 0..=20 {
                let p = i as f64 / 20.0;
                let pv = t.playing_volume(p) as f64;
                let iv = t.incoming_volume(p) as f64;
                assert!((pv * pv + iv * iv - 1.0).abs() < 0.01,
                    "{t:?} p={p} sum²={}", pv * pv + iv * iv);
            }
        }
    }

    #[test]
    fn echoout_incoming_volume_piecewise_continuous() {
        // The 3-piece stitch (0 → ramp 0.40625..0.41125 → fade 0.41125..0.75 → 1.0)
        // has bitten us before. Sample densely and assert no step > 0.05 between
        // consecutive samples — a click-free curve must stay continuous even
        // when we edit the break-points.
        let t = TransitionType::EchoOut;
        let mut prev = t.incoming_volume(0.0);
        for i in 1..=10_000 {
            let p = i as f64 / 10_000.0;
            let v = t.incoming_volume(p);
            assert!((v - prev).abs() < 0.05,
                "discontinuity at p={p}: jumped {prev} → {v}");
            prev = v;
        }
    }

    #[test]
    fn bassswap_playing_volume_continuous_at_swap_boundary() {
        // The 0.9 break-point splits a flat 1.0 region from a cos fade. Both
        // sides must be ~1.0 at p=0.9 — otherwise the last-10% fade starts
        // with an audible notch.
        let t = TransitionType::BassSwap;
        let just_before = t.playing_volume(0.8999);
        let just_after = t.playing_volume(0.9001);
        assert!((just_before - 1.0).abs() < 1e-6);
        assert!((just_after - 1.0).abs() < 0.01,
            "bassswap notch at 0.9 boundary: before={just_before} after={just_after}");
    }

    #[test]
    fn incoming_volumes_monotonic_non_decreasing() {
        // Sanity across all transitions: the incoming deck should never
        // get *quieter* as the crossfade progresses. Catches accidental
        // sign flips or break-point misorderings.
        for t in [
            TransitionType::BeatMatched,
            TransitionType::FilterSweep,
            TransitionType::LoopRoll,
            TransitionType::BassSwap,
        ] {
            let mut prev = t.incoming_volume(0.0);
            for i in 1..=1000 {
                let p = i as f64 / 1000.0;
                let v = t.incoming_volume(p);
                assert!(v + 1e-5 >= prev,
                    "{t:?} incoming went backwards at p={p}: {prev} → {v}");
                prev = v;
            }
        }
    }

    #[test]
    fn fader_position_monotonic_non_decreasing() {
        // The visual needle should never retreat — if it does, the UI
        // jumps backwards mid-mix. Applies to all transitions.
        for t in [
            TransitionType::BeatMatched,
            TransitionType::EchoOut,
            TransitionType::FilterSweep,
            TransitionType::LoopRoll,
            TransitionType::BassSwap,
        ] {
            let mut prev = t.fader_position(0.0);
            for i in 1..=1000 {
                let p = i as f64 / 1000.0;
                let v = t.fader_position(p);
                assert!(v + 1e-6 >= prev,
                    "{t:?} fader retreated at p={p}: {prev} → {v}");
                prev = v;
            }
        }
    }

    #[test]
    fn fader_position_endpoints() {
        // Every transition must start at the playing side (0) and end at
        // the incoming side (1) — otherwise the needle "sticks" and the
        // deck roles won't appear to swap.
        for t in [
            TransitionType::BeatMatched,
            TransitionType::EchoOut,
            TransitionType::FilterSweep,
            TransitionType::LoopRoll,
            TransitionType::BassSwap,
        ] {
            assert!(t.fader_position(0.0) <= 0.01, "{t:?} start not at 0");
            assert!(t.fader_position(1.0) >= 0.99, "{t:?} end not at 1, got {}", t.fader_position(1.0));
        }
    }

    #[test]
    fn camelot_distance_edges() {
        assert_eq!(camelot_distance("8A", "8A"), 0);
        assert_eq!(camelot_distance("8A", "9A"), 1);
        assert_eq!(camelot_distance("1A", "12A"), 1);
        assert_eq!(camelot_distance("12A", "1A"), 1);
        assert_eq!(camelot_distance("8A", "8B"), 1);
        assert_eq!(camelot_distance("??", "8A"), 99);
        assert_eq!(camelot_distance("8A", "8C"), 99);
    }

    #[test]
    fn camelot_distance_cross_mode_cross_number() {
        assert_eq!(camelot_distance("8A", "9B"), 2);
        assert_eq!(camelot_distance("8A", "2B"), 7);
        assert_eq!(camelot_distance("1A", "12B"), 2);
    }

    #[test]
    fn echoout_playing_volume_ramp_to_silence() {
        let t = TransitionType::EchoOut;
        assert!((t.playing_volume(0.0) - 1.0).abs() < 1e-6);
        assert!(t.playing_volume(0.003) > 0.0 && t.playing_volume(0.003) < 1.0);
        assert_eq!(t.playing_volume(0.005), 0.0);
        assert_eq!(t.playing_volume(0.5), 0.0);
        let mut prev = t.playing_volume(0.0);
        for i in 1..=1000 {
            let p = i as f64 / 1000.0;
            let v = t.playing_volume(p);
            assert!(v <= prev + 1e-6, "went up at p={p}: {prev} → {v}");
            prev = v;
        }
    }

    #[test]
    fn echoout_delay_wet_curve_holds_then_decays_then_zeros() {
        // Held at 1.0 for the first 12.5% (echo tail rings at full).
        assert!((echoout_delay_wet(0.0) - 1.0).abs() < 1e-6);
        assert!((echoout_delay_wet(0.05) - 1.0).abs() < 1e-6);
        assert!((echoout_delay_wet(0.1249) - 1.0).abs() < 1e-6);
        // Quadratic decay between 0.125 and 0.5.
        let mid = echoout_delay_wet(0.3125); // halfway through fade
        // fade=0.5 → curve=(1-0.5)²=0.25
        assert!((mid - 0.25).abs() < 1e-3, "expected ~0.25 at p=0.3125, got {mid}");
        // Approaches 0 at the end of the fade window.
        assert!(echoout_delay_wet(0.499) < 0.01);
        // Strict zero past 50%.
        assert_eq!(echoout_delay_wet(0.5), 0.0);
        assert_eq!(echoout_delay_wet(0.99), 0.0);
    }

    #[test]
    fn echoout_delay_wet_is_monotonic_decreasing() {
        // The wet curve never goes back up — once the echo starts
        // fading it must continue to zero. Catches accidental
        // sign-flip on the (1-fade)² term.
        let mut prev = echoout_delay_wet(0.0);
        for i in 1..=1000 {
            let p = i as f64 / 1000.0;
            let v = echoout_delay_wet(p);
            assert!(v <= prev + 1e-6, "wet went up at p={p}: {prev} → {v}");
            prev = v;
        }
    }
}
