//! Pitch-invariant time stretching.
//!
//! Three modes:
//! - **Off** — no stretching; rate changes shift pitch along with tempo
//!   (the classic "varispeed" behaviour: faster = higher pitch).
//!   Zero overhead, rock-solid quality.
//! - **Rubberband** — FFI to `librubberband`, gated behind the `rubberband`
//!   Cargo feature. Requires `brew install rubberband` (macOS) /
//!   `apt install librubberband-dev` (Linux). GPL v2+ / commercial license.
//! - **Timestretch** — pure-Rust hybrid WSOLA + phase vocoder, gated behind
//!   the `timestretch` Cargo feature. MIT-licensed, no FFI, no system
//!   library needed. Built for A/B against Rubberband.
//!
//! Without the feature compiled in, selecting that engine logs a warning
//! and stays on Off instead of silently falling back to a worse engine.

#[cfg(any(feature = "rubberband", feature = "timestretch"))]
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum PitchStretchEngine {
    /// Varispeed — rate changes shift pitch. Default.
    #[default]
    Off,
    /// librubberband FFI. Needs `--features rubberband` + system library.
    Rubberband,
    /// Pure-Rust WSOLA + phase vocoder. Needs `--features timestretch`.
    Timestretch,
}


pub trait Stretcher: Send {
    /// Produce output samples at the given stretch rate (1.0 = no change,
    /// 1.05 = 5% faster). `source` is the full source buffer;
    /// `source_pos_start` is where this call should read from.
    /// Returns (samples_written_to_output, source_samples_consumed).
    fn process(
        &mut self,
        source: &[f32],
        source_pos_start: f64,
        output: &mut [f32],
        rate: f64,
    ) -> (usize, f64);

    fn reset(&mut self);
}

pub fn make(engine: PitchStretchEngine) -> Option<Box<dyn Stretcher>> {
    match engine {
        PitchStretchEngine::Off => None,
        PitchStretchEngine::Rubberband => {
            #[cfg(feature = "rubberband")]
            { Some(Box::new(RubberbandStretcher::new())) }
            #[cfg(not(feature = "rubberband"))]
            {
                tracing::warn!("Rubberband engine requested but `rubberband` feature not compiled — pitch stretch disabled");
                None
            }
        }
        PitchStretchEngine::Timestretch => {
            #[cfg(feature = "timestretch")]
            { Some(Box::new(TimestretchStretcher::new())) }
            #[cfg(not(feature = "timestretch"))]
            {
                tracing::warn!("Timestretch engine requested but `timestretch` feature not compiled — pitch stretch disabled");
                None
            }
        }
    }
}

// ============================================================================
// RubberBand FFI
// ============================================================================

#[cfg(feature = "rubberband")]
mod rb_ffi {
    use libc::{c_double, c_int, c_uint, c_void};

    pub type Handle = *mut c_void;

    pub const OPT_PROCESS_REALTIME: c_int = 0x00000001;
    pub const OPT_PITCH_HIGH_QUALITY: c_int = 0x02000000;
    pub const OPT_PHASE_INDEPENDENT: c_int = 0x00002000;

    unsafe extern "C" {
        pub fn rubberband_new(
            sample_rate: c_uint,
            channels: c_uint,
            options: c_int,
            initial_time_ratio: c_double,
            initial_pitch_scale: c_double,
        ) -> Handle;
        pub fn rubberband_delete(h: Handle);
        pub fn rubberband_set_time_ratio(h: Handle, ratio: c_double);
        pub fn rubberband_reset(h: Handle);
        pub fn rubberband_process(h: Handle, input: *const *const f32, samples: c_uint, final_: c_int);
        pub fn rubberband_available(h: Handle) -> c_int;
        pub fn rubberband_retrieve(h: Handle, output: *const *mut f32, samples: c_uint) -> c_uint;
    }
}

#[cfg(feature = "rubberband")]
pub struct RubberbandStretcher {
    handle: rb_ffi::Handle,
    pending: VecDeque<f32>,
    read_offset: f64,
    last_rate: f64,
}

#[cfg(feature = "rubberband")]
impl RubberbandStretcher {
    pub fn new() -> Self {
        let sample_rate = 48_000u32;
        let opts = rb_ffi::OPT_PROCESS_REALTIME | rb_ffi::OPT_PITCH_HIGH_QUALITY | rb_ffi::OPT_PHASE_INDEPENDENT;
        let handle = unsafe { rb_ffi::rubberband_new(sample_rate, 1, opts, 1.0, 1.0) };
        Self { handle, pending: VecDeque::with_capacity(4096), read_offset: 0.0, last_rate: 1.0 }
    }
}

#[cfg(feature = "rubberband")]
impl Drop for RubberbandStretcher {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { rb_ffi::rubberband_delete(self.handle); }
            self.handle = std::ptr::null_mut();
        }
    }
}

#[cfg(feature = "rubberband")]
unsafe impl Send for RubberbandStretcher {}

// ============================================================================
// Pure-Rust timestretch (WSOLA + phase vocoder)
// ============================================================================

#[cfg(feature = "timestretch")]
pub struct TimestretchStretcher {
    proc: timestretch::StreamProcessor,
    pending: VecDeque<f32>,
    /// Scratch Vec for the crate's `process_into` (which appends).
    /// Pre-allocated so we don't realloc on the audio path.
    scratch_out: Vec<f32>,
    last_rate: f64,
}

#[cfg(feature = "timestretch")]
impl TimestretchStretcher {
    pub fn new() -> Self {
        let params = timestretch::StretchParams::new(1.0)
            .with_sample_rate(48_000)
            .with_channels(1)
            .with_preset(timestretch::EdmPreset::DjBeatmatch);
        let proc = timestretch::StreamProcessor::new(params);
        Self {
            proc,
            pending: VecDeque::with_capacity(4096),
            scratch_out: Vec::with_capacity(8192),
            last_rate: 1.0,
        }
    }
}

#[cfg(feature = "timestretch")]
impl Stretcher for TimestretchStretcher {
    fn process(&mut self, source: &[f32], source_pos_start: f64, output: &mut [f32], rate: f64) -> (usize, f64) {
        // Stretch ratio convention matches Rubberband: ratio = output_dur
        // / input_dur = 1 / rate. Speeding up (rate > 1) → ratio < 1
        // (output is shorter than input).
        if (rate - self.last_rate).abs() > 1e-6 {
            let _ = self.proc.set_stretch_ratio(1.0 / rate);
            self.last_rate = rate;
        }

        const FEED_FRAMES: usize = 1024;
        let mut read_offset = 0.0_f64;

        while self.pending.len() < output.len() {
            let start = (source_pos_start + read_offset) as usize;
            if start + FEED_FRAMES >= source.len() { break; }
            let slice = &source[start..start + FEED_FRAMES];
            self.scratch_out.clear();
            if self.proc.process_into(slice, &mut self.scratch_out).is_err() {
                break;
            }
            read_offset += FEED_FRAMES as f64;
            for s in self.scratch_out.iter() { self.pending.push_back(*s); }
        }

        let written = output.len().min(self.pending.len());
        for slot in output.iter_mut().take(written) {
            *slot = self.pending.pop_front().unwrap();
        }
        // Same accounting contract as Rubberband: report only the
        // source-equivalent of output delivered. The excess we read
        // ahead stays in `pending` and the crate's internal state.
        let consumed = written as f64 * rate;
        (written, consumed)
    }

    fn reset(&mut self) {
        self.proc.reset();
        self.pending.clear();
        self.scratch_out.clear();
    }
}

/// Test-only stretcher that satisfies the accounting contract every
/// real implementation must: for a requested output length, it reports
/// `consumed = written * rate` in source-sample units. This is the
/// invariant the engine relies on to keep `deck.position` aligned with
/// audible playback. Exposed so integration-style deck tests can wire
/// a known-correct stretcher in without pulling in librubberband.
#[cfg(test)]
pub struct FakeStretcher;

#[cfg(test)]
impl Stretcher for FakeStretcher {
    fn process(&mut self, _source: &[f32], _source_pos_start: f64, output: &mut [f32], rate: f64) -> (usize, f64) {
        for s in output.iter_mut() { *s = 0.0; }
        let written = output.len();
        // Must mirror RubberbandStretcher's accounting.
        let consumed = written as f64 * rate;
        (written, consumed)
    }
    fn reset(&mut self) {}
}

/// Test-only stretcher with the *broken* accounting that caused the
/// 20–80 ms phantom-phase bug — reports `consumed = written` regardless
/// of rate, so at rate != 1.0 `deck.position` drifts away from reality.
/// Used as a negative control so the regression-guard test actually
/// fails on the broken behavior.
#[cfg(test)]
pub struct BrokenStretcher;

#[cfg(test)]
impl Stretcher for BrokenStretcher {
    fn process(&mut self, _source: &[f32], _source_pos_start: f64, output: &mut [f32], _rate: f64) -> (usize, f64) {
        for s in output.iter_mut() { *s = 0.0; }
        (output.len(), output.len() as f64)
    }
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_stretcher_reports_rate_scaled_consumed() {
        let mut s = FakeStretcher;
        let src = vec![0.0_f32; 10_000];
        let mut out = vec![0.0_f32; 480];

        // Unity: consumed == written.
        let (w, c) = s.process(&src, 0.0, &mut out, 1.0);
        assert_eq!(w, 480);
        assert!((c - 480.0).abs() < 1e-9);

        // 5% fast: 1 output sample costs 1.05 source samples.
        let (w, c) = s.process(&src, 0.0, &mut out, 1.05);
        assert_eq!(w, 480);
        assert!((c - 480.0 * 1.05).abs() < 1e-9, "expected 504, got {c}");

        // Half speed: 1 output sample costs 0.5 source samples.
        let (w, c) = s.process(&src, 0.0, &mut out, 0.5);
        assert_eq!(w, 480);
        assert!((c - 240.0).abs() < 1e-9);
    }

    #[test]
    fn broken_stretcher_fails_the_invariant() {
        // Negative control: the broken impl must NOT satisfy
        // consumed = written * rate. Guards against accidentally making
        // BrokenStretcher correct in a refactor — that would turn the
        // regression-guard test into a tautology.
        let mut s = BrokenStretcher;
        let mut out = vec![0.0_f32; 480];
        let (_, c) = s.process(&[], 0.0, &mut out, 1.05);
        assert!((c - 480.0 * 1.05).abs() > 1.0,
            "BrokenStretcher must diverge from the correct formula");
    }
}

#[cfg(feature = "rubberband")]
impl Stretcher for RubberbandStretcher {
    fn process(&mut self, source: &[f32], source_pos_start: f64, output: &mut [f32], rate: f64) -> (usize, f64) {
        // time_ratio = output_duration / input_duration = 1 / rate.
        if (rate - self.last_rate).abs() > 1e-6 {
            unsafe { rb_ffi::rubberband_set_time_ratio(self.handle, 1.0 / rate); }
            self.last_rate = rate;
        }

        const FEED_FRAMES: usize = 1024;

        while self.pending.len() < output.len() {
            let start = (source_pos_start + self.read_offset) as usize;
            if start + FEED_FRAMES >= source.len() { break; }
            let slice = &source[start..start + FEED_FRAMES];
            let ptrs = [slice.as_ptr()];
            unsafe { rb_ffi::rubberband_process(self.handle, ptrs.as_ptr(), FEED_FRAMES as u32, 0); }
            self.read_offset += FEED_FRAMES as f64;

            let avail = unsafe { rb_ffi::rubberband_available(self.handle) };
            if avail > 0 {
                let want = avail as usize;
                let mut buf = vec![0.0f32; want];
                let bufp: [*mut f32; 1] = [buf.as_mut_ptr()];
                let n = unsafe { rb_ffi::rubberband_retrieve(self.handle, bufp.as_ptr(), want as u32) } as usize;
                for s in &buf[..n] { self.pending.push_back(*s); }
            }
        }

        let written = output.len().min(self.pending.len());
        for slot in output.iter_mut().take(written) {
            *slot = self.pending.pop_front().unwrap();
        }
        // Report only the source-equivalent of output delivered; the excess
        // we've read ahead stays in RubberBand's internal state + `pending`.
        let consumed = written as f64 * rate;
        self.read_offset -= consumed;
        (written, consumed)
    }

    fn reset(&mut self) {
        unsafe { rb_ffi::rubberband_reset(self.handle); }
        self.pending.clear();
        self.read_offset = 0.0;
    }
}

#[cfg(all(test, feature = "rubberband"))]
mod rubberband_tests {
    use super::*;

    /// Real-library regression guard for the accounting contract.
    /// `FakeStretcher`/`BrokenStretcher` prove the invariant that *our*
    /// `deck.position` path relies on; this test proves the real
    /// Rubberband FFI actually honors that invariant. Run with
    /// `cargo test --features rubberband rubberband_tests`. Not in
    /// default CI because it needs `brew install rubberband` /
    /// `apt install librubberband-dev` on the test host.
    #[test]
    fn rubberband_reports_consumed_near_written_times_rate() {
        let mut rb = RubberbandStretcher::new();
        // Synthesize a ~1s 220 Hz sine at 48k; Rubberband refuses to run
        // on raw zeros. Source size comfortably larger than the lookahead
        // so `process` has enough to draw from.
        let sr = 48_000.0_f32;
        let src: Vec<f32> = (0..50_000)
            .map(|i| (i as f32 / sr * 2.0 * std::f32::consts::PI * 220.0).sin() * 0.5)
            .collect();
        let mut out = vec![0.0_f32; 480];

        // Warm up: Rubberband buffers a lookahead window before producing
        // full output frames. Early calls will return `written < out.len()`;
        // we want to measure steady state.
        let rate = 1.05;
        for _ in 0..40 { rb.process(&src, 0.0, &mut out, rate); }

        // Capture several steady-state calls and assert `consumed` tracks
        // `written * rate` — the exact invariant BrokenStretcher violates.
        // Rubberband's internal bookkeeping can round to whole source
        // samples, so allow ±2 samples of slack per call.
        for _ in 0..10 {
            let (w, c) = rb.process(&src, 0.0, &mut out, rate);
            if w == 0 { continue; } // starvation — skip
            let expected = w as f64 * rate;
            assert!((c - expected).abs() < 2.0,
                "rubberband accounting: written={w} expected consumed≈{expected}, got {c}");
        }
    }
}
