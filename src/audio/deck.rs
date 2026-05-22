use super::analyzer::AnalysisResult;
use super::beat_grid::BeatGrid;
use crate::beatport::models::BeatportTrack;

/// Per-deck audio state. Managed exclusively by the engine — not thread-safe on its own.
pub struct DeckPlayer {
    /// Decoded audio samples (mono f32).
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// Output device sample rate — used to compute resample ratio.
    pub output_sample_rate: u32,

    /// Playback position in source samples (fractional for interpolation).
    pub position: f64,
    pub playing: bool,
    pub paused: bool,

    /// Playback rate (1.0 = normal, affected by tempo matching + nudge).
    /// This is the *currently-applied* rate — the audio callback slews it
    /// toward `rate_target` over ~20ms so glide / rate-correction jumps
    /// don't produce audible pitch stairs when tick updates fire at 60Hz.
    pub rate: f64,
    /// External setters (`Mixer::set_rate`, glide, crossfade controller)
    /// write this. `fill_buffer` converges `rate` toward `rate_target`
    /// per-sample so the user hears a continuous sweep instead of
    /// stepped pitch changes on buffer boundaries.
    pub rate_target: f64,
    pub volume: f32,

    /// Track metadata.
    pub track: Option<std::sync::Arc<BeatportTrack>>,
    pub beat_grid: Option<BeatGrid>,
    pub analysis: Option<std::sync::Arc<AnalysisResult>>,

    /// Feedback delay for echo-out transitions.
    pub delay_buffer: Vec<f32>,
    pub delay_write_pos: usize,
    pub delay_samples: usize,
    pub delay_wet: f32,
    pub delay_feedback: f32,
    /// High-pass filter state for echo (two-pole for 12dB/octave)
    delay_hp_prev: f32,
    delay_hp_out: f32,
    delay_hp_prev2: f32,
    delay_hp_out2: f32,
    /// Low-pass filter state for echo (sweeps down)
    delay_lp_out: f32,
    delay_lp_out2: f32,

    /// Metering.
    pub level: f32,
    pub peak: f32,

    /// Real-time kick onset detection (like a hardware BPM counter LED).
    /// Detects sharp increases in low-frequency energy between audio
    /// callbacks — pulses true on kick transients, independent of grid.
    pub kick_active: bool,
    kick_lp: f32,         // single-pole LPF state for bass isolation
    kick_prev_energy: f32,// previous buffer's low-band energy
    kick_avg_delta: f32,  // running average of energy deltas for threshold
    kick_hold: u32,       // hold counter (frames remaining to stay lit)

    /// 3-band EQ gains in dB. 0.0 = unity, -inf = kill.
    pub eq_low_db: f32,
    pub eq_mid_db: f32,
    pub eq_high_db: f32,
    /// Biquad filter state — per-band input/output history.
    eq_low: Biquad,
    eq_mid: Biquad,
    eq_high: Biquad,

    /// Filter knob: -1.0 = full low-pass, 0.0 = bypass, +1.0 = full high-pass.
    pub filter_pos: f32,
    filter: Biquad,

    /// Loop points in source samples. When loop_active and both set,
    /// playback wraps from loop_out back to loop_in.
    pub loop_in: Option<u64>,
    pub loop_out: Option<u64>,
    pub loop_active: bool,

    /// 4 hot cue points (source-sample positions). None = unset.
    pub cues: [Option<u64>; 4],

    /// Optional pitch-invariant stretcher. Set by the engine from config.
    pub pitch_stretch: Option<Box<dyn super::pitch_stretch::Stretcher>>,

    /// Persistent scratch buffers for the stretcher → resampler chain.
    /// Pre-allocated and reused across callbacks so `fill_buffer` never
    /// hits the allocator on the RT thread. `Vec::resize` is free once
    /// capacity is reached (buffer sizes are stable across callbacks).
    stretch_src: Vec<f32>,
    stretch_out: Vec<f32>,
}

/// Transposed-direct-form-II biquad. Coefficients recomputed on EQ change.
#[derive(Default, Clone, Copy)]
struct Biquad {
    b0: f32, b1: f32, b2: f32, a1: f32, a2: f32,
    z1: f32, z2: f32,
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) { self.z1 = 0.0; self.z2 = 0.0; }

    /// RBJ low-shelf.
    fn low_shelf(fs: f32, fc: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * fc / fs;
        let (sn, cs) = w0.sin_cos();
        let s = 1.0; // shelf slope
        let alpha = sn / 2.0 * ((a + 1.0/a) * (1.0/s - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) + (a - 1.0) * cs + two_sqrt_a_alpha;
        let b0 =  a * ((a + 1.0) - (a - 1.0) * cs + two_sqrt_a_alpha);
        let b1 =  2.0 * a * ((a - 1.0) - (a + 1.0) * cs);
        let b2 =  a * ((a + 1.0) - (a - 1.0) * cs - two_sqrt_a_alpha);
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cs);
        let a2 = (a + 1.0) + (a - 1.0) * cs - two_sqrt_a_alpha;
        Self { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0, z1: 0.0, z2: 0.0 }
    }

    /// RBJ high-shelf.
    fn high_shelf(fs: f32, fc: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * fc / fs;
        let (sn, cs) = w0.sin_cos();
        let s = 1.0;
        let alpha = sn / 2.0 * ((a + 1.0/a) * (1.0/s - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a0 =  (a + 1.0) - (a - 1.0) * cs + two_sqrt_a_alpha;
        let b0 =  a * ((a + 1.0) + (a - 1.0) * cs + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cs);
        let b2 =  a * ((a + 1.0) + (a - 1.0) * cs - two_sqrt_a_alpha);
        let a1 =  2.0 * ((a - 1.0) - (a + 1.0) * cs);
        let a2 = (a + 1.0) - (a - 1.0) * cs - two_sqrt_a_alpha;
        Self { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0, z1: 0.0, z2: 0.0 }
    }

    /// RBJ lowpass (Q=0.707).
    fn lowpass(fs: f32, fc: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * fc / fs;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * 0.707);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 - cs) / 2.0;
        let b1 = 1.0 - cs;
        let b2 = b0;
        let a1 = -2.0 * cs;
        let a2 = 1.0 - alpha;
        Self { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0, z1: 0.0, z2: 0.0 }
    }

    /// RBJ highpass (Q=0.707).
    fn highpass(fs: f32, fc: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * fc / fs;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * 0.707);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + cs) / 2.0;
        let b1 = -(1.0 + cs);
        let b2 = b0;
        let a1 = -2.0 * cs;
        let a2 = 1.0 - alpha;
        Self { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0, z1: 0.0, z2: 0.0 }
    }

    /// RBJ peaking EQ (Q=1.0).
    fn peaking(fs: f32, fc: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * fc / fs;
        let (sn, cs) = w0.sin_cos();
        let q = 1.0;
        let alpha = sn / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cs;
        let b2 = 1.0 - alpha * a;
        let a1 = -2.0 * cs;
        let a2 = 1.0 - alpha / a;
        Self { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0, z1: 0.0, z2: 0.0 }
    }
}

impl DeckPlayer {
    pub fn new(output_sample_rate: u32) -> Self {
        // Default delay buffer: 1 second max
        let delay_buf_size = output_sample_rate as usize;
        Self {
            samples: Vec::new(),
            sample_rate: 44100,
            output_sample_rate,
            position: 0.0,
            playing: false,
            paused: false,
            rate: 1.0,
            rate_target: 1.0,
            volume: 1.0,
            track: None,
            beat_grid: None,
            analysis: None,
            delay_buffer: vec![0.0; delay_buf_size],
            delay_write_pos: 0,
            delay_samples: output_sample_rate as usize / 2, // default 500ms
            delay_wet: 0.0,
            delay_feedback: 0.50,
            delay_hp_prev: 0.0,
            delay_hp_out: 0.0,
            delay_hp_prev2: 0.0,
            delay_hp_out2: 0.0,
            delay_lp_out: 0.0,
            delay_lp_out2: 0.0,
            level: 0.0,
            peak: 0.0,
            kick_active: false,
            kick_lp: 0.0,
            kick_prev_energy: 0.0,
            kick_avg_delta: 0.0,
            kick_hold: 0,
            eq_low_db: 0.0,
            eq_mid_db: 0.0,
            eq_high_db: 0.0,
            eq_low: Biquad::low_shelf(output_sample_rate as f32, 250.0, 0.0),
            eq_mid: Biquad::peaking(output_sample_rate as f32, 1000.0, 0.0),
            eq_high: Biquad::high_shelf(output_sample_rate as f32, 4000.0, 0.0),
            filter_pos: 0.0,
            filter: Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 },
            loop_in: None,
            loop_out: None,
            loop_active: false,
            cues: [None; 4],
            pitch_stretch: None,
            // Pre-size scratch for a generous 4096-frame callback. cpal
            // usually hands us 256–1024; resize is free after this.
            stretch_src: Vec::with_capacity(4096 + 2),
            stretch_out: Vec::with_capacity(4096),
        }
    }

    /// Recompute the single-knob filter biquad from filter_pos.
    /// pos in [-1, 0) → LP with fc from 20 kHz (near 0) down to 100 Hz (-1).
    /// pos in (0, +1] → HP with fc from 20 Hz (near 0) up to 4 kHz (+1).
    /// pos == 0 → bypass (zero coeffs that pass signal through).
    pub fn update_filter(&mut self) {
        let fs = self.output_sample_rate as f32;
        let p = self.filter_pos.clamp(-1.0, 1.0);
        // Bypass and active arms are deliberately separate. Bypass is
        // an identity biquad (b0=1, all others 0) with z1/z2 cleared —
        // preserving prior delay state would inject a one-sample DC
        // transient (the formula `y = b0*x + z1` lights up with
        // non-zero z1). Active LP/HP go through rebuild_biquad to
        // preserve z1/z2 across coefficient changes, avoiding clicks.
        if p.abs() < 0.01 {
            self.filter = Biquad { b0: 1.0, ..Default::default() };
            return;
        }
        let new_filter = if p < 0.0 {
            // Logarithmic: 100 Hz at p=-1, 20000 Hz at p=0.
            let fc = 100.0 * (200.0_f32).powf(1.0 + p);
            Biquad::lowpass(fs, fc)
        } else {
            // Logarithmic: 20 Hz at p=0, 4000 Hz at p=+1.
            let fc = 20.0 * (200.0_f32).powf(p);
            Biquad::highpass(fs, fc)
        };
        Self::rebuild_biquad(&mut self.filter, new_filter);
    }

    /// Replace `dst` with `new_filter`, preserving its z1/z2 delay-line
    /// state. EQ coefficient updates would otherwise pop on every
    /// knob movement; copying the state across keeps the IIR continuous.
    fn rebuild_biquad(dst: &mut Biquad, new_filter: Biquad) {
        let z1 = dst.z1;
        let z2 = dst.z2;
        *dst = new_filter;
        dst.z1 = z1;
        dst.z2 = z2;
    }

    pub fn update_eq_low(&mut self) {
        let fs = self.output_sample_rate as f32;
        Self::rebuild_biquad(&mut self.eq_low, Biquad::low_shelf(fs, 250.0, self.eq_low_db));
    }
    pub fn update_eq_mid(&mut self) {
        let fs = self.output_sample_rate as f32;
        Self::rebuild_biquad(&mut self.eq_mid, Biquad::peaking(fs, 1000.0, self.eq_mid_db));
    }
    pub fn update_eq_high(&mut self) {
        let fs = self.output_sample_rate as f32;
        Self::rebuild_biquad(&mut self.eq_high, Biquad::high_shelf(fs, 4000.0, self.eq_high_db));
    }

    pub fn load(&mut self, samples: Vec<f32>, sample_rate: u32, analysis: AnalysisResult, track: BeatportTrack) {
        let bpm = analysis.beat_grid.bpm;
        self.samples = samples;
        self.sample_rate = sample_rate;
        self.beat_grid = Some(analysis.beat_grid);
        self.analysis = Some(std::sync::Arc::new(analysis));
        self.track = Some(std::sync::Arc::new(track));
        self.position = 0.0;
        self.playing = false;
        self.paused = false;
        self.rate = 1.0;
        self.rate_target = 1.0;
        self.volume = 1.0;
        self.level = 0.0;
        self.peak = 0.0;
        // Set delay time to 3/4 beat (dotted eighth feel)
        let beat_secs = if bpm > 0.0 { 60.0 / bpm } else { 0.5 };
        self.delay_samples = (beat_secs * 0.5 * self.output_sample_rate as f64) as usize;
        self.delay_samples = self.delay_samples.min(self.delay_buffer.len().saturating_sub(1));
        self.delay_wet = 0.0;
        self.delay_feedback = 0.50;
        self.delay_hp_prev = 0.0;
        self.delay_hp_out = 0.0;
        self.delay_hp_prev2 = 0.0;
        self.delay_hp_out2 = 0.0;
        self.delay_lp_out = 0.0;
        self.delay_lp_out2 = 0.0;
        self.delay_buffer.fill(0.0);
        self.delay_write_pos = 0;
        self.eq_low_db = 0.0;
        self.eq_mid_db = 0.0;
        self.eq_high_db = 0.0;
        self.update_eq_low();
        self.update_eq_mid();
        self.update_eq_high();
        // Track boundary: zero biquad delay lines so the new track starts clean.
        self.eq_low.reset();
        self.eq_mid.reset();
        self.eq_high.reset();
        self.filter_pos = 0.0;
        self.update_filter();
        self.filter.reset();
        self.loop_in = None;
        self.loop_out = None;
        self.loop_active = false;
        self.cues = [None; 4];
        if let Some(ref mut p) = self.pitch_stretch { p.reset(); }
    }

    /// Ratio to convert from output samples to source samples.
    /// e.g. source=44100, output=48000 → 0.91875 (consume fewer source samples per output sample)
    pub fn resample_ratio(&self) -> f64 {
        self.sample_rate as f64 / self.output_sample_rate as f64
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn stop(&mut self) {
        self.playing = false;
    }

    /// Clear all state — deck is empty and ready for a new track.
    /// Returns the old samples Vec so the caller can drop it outside any lock.
    pub fn unload(&mut self) -> Vec<f32> {
        self.stop();
        let old_samples = std::mem::take(&mut self.samples);
        self.track = None;
        self.beat_grid = None;
        self.analysis = None;
        self.paused = false;
        self.delay_buffer.fill(0.0);
        self.delay_wet = 0.0;
        self.delay_write_pos = 0;
        self.delay_hp_prev = 0.0;
        self.delay_hp_out = 0.0;
        self.delay_hp_prev2 = 0.0;
        self.delay_hp_out2 = 0.0;
        self.delay_lp_out = 0.0;
        self.delay_lp_out2 = 0.0;
        // Reset tone-stack state too so a deck that just played a
        // BassSwap or FilterSweep doesn't carry eq_low=-24 or filter=1.0
        // forward to the next load. Samples are already gone so there's
        // no audible bleed, but the next prepare() should see a neutral
        // starting point.
        self.eq_low_db = 0.0;
        self.eq_mid_db = 0.0;
        self.eq_high_db = 0.0;
        self.update_eq_low();
        self.update_eq_mid();
        self.update_eq_high();
        self.eq_low.reset();
        self.eq_mid.reset();
        self.eq_high.reset();
        self.filter_pos = 0.0;
        self.update_filter();
        self.filter.reset();
        self.loop_active = false;
        self.loop_in = None;
        self.loop_out = None;
        old_samples
    }

    pub fn seek(&mut self, time: f64) {
        self.position = (time * self.sample_rate as f64).min((self.samples.len().saturating_sub(1)) as f64);
    }

    /// Current playback time in seconds.
    pub fn current_time(&self) -> f64 {
        self.position / self.sample_rate as f64
    }

    /// Duration in seconds.
    pub fn duration(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate as f64
    }

    /// Time remaining.
    pub fn time_remaining(&self) -> f64 {
        self.duration() - self.current_time()
    }

    /// Fill output buffer with samples, applying rate and volume.
    /// Returns number of frames written.
    pub fn fill_buffer(&mut self, output: &mut [f32]) -> usize {
        if !self.playing || self.samples.is_empty() {
            output.fill(0.0);
            return 0;
        }

        // Slew `rate` toward `rate_target` over ~20ms. The stretched
        // path (Rubberband) gets a single per-buffer step — Rubberband
        // smooths its own rate transitions internally — but the direct
        // (no-stretch) path slews per-sample to avoid an audible click
        // at the buffer boundary on big jumps (e.g. CrossfadeController's
        // one-shot ±3% kick). Skip the alpha computation entirely when
        // rate is already at target — `exp()` is ~25ns/call and runs on
        // every fill_buffer otherwise.
        let need_slew = (self.rate_target - self.rate).abs() > 1e-9;
        let rate_alpha_per_sample = if need_slew {
            1.0 - (-1.0 / (0.020 * self.output_sample_rate as f64)).exp()
        } else {
            0.0
        };
        if need_slew && self.pitch_stretch.is_some() {
            let buf_samples = output.len() as f64;
            let buffer_alpha = 1.0 - (1.0 - rate_alpha_per_sample).powf(buf_samples);
            self.rate += (self.rate_target - self.rate) * buffer_alpha;
            if (self.rate_target - self.rate).abs() < 1e-6 {
                self.rate = self.rate_target;
            }
        }

        let mut rms_sum = 0.0f32;
        let mut written = 0;
        // Advance through source samples at the correct ratio for the output sample rate
        let resample = self.resample_ratio();
        let step = resample * self.rate;

        // Pitch-invariant path. The stretcher operates entirely in the
        // SOURCE sample-rate domain: it takes source samples and produces
        // source-rate samples, stretched by 1/self.rate. We then linearly
        // interpolate that source-rate stream to the device output rate
        // at `resample_ratio` per output sample.
        //
        // Derivation: for Y output samples we want playback speed = r (self.rate).
        // Playback speed = 1/f where f = stretch factor (hop_out/hop_in).
        // So f = 1/r, meaning hop_in = hop_out × r — i.e., pass `self.rate`.
        // Always go through the stretcher when one is configured — even at
        // unity rate. Bypassing only on the playing deck (rate=1) creates a
        // latency mismatch vs the incoming deck (rate≠1, going through the
        // stretcher's internal lookahead), and the crossfade phase sync
        // overcorrects against the fake offset → trainwreck.
        // Stretch + resample into persistent scratch. `resize(0, ..)` + `resize(n, ..)`
        // is a no-op after the first growth — these Vecs never reallocate on the RT
        // thread after warm-up. `stretched` flags whether to read from the scratch
        // or fall through to the direct-source path below.
        let stretched = if self.pitch_stretch.is_some() && self.rate > 0.0 {
            let resample = self.resample_ratio();
            let need = (output.len() as f64 * resample).ceil() as usize + 2;
            self.stretch_src.resize(need, 0.0);
            let (n_src, consumed) = {
                // Combined `is_some` + `rate > 0.0` guard above means the
                // unwrap is infallible. Lint suppression rather than a
                // pattern-match restructure because the rate guard is the
                // semantic gate, not the Option's variant alone.
                #[allow(clippy::unnecessary_unwrap)]
                let stretcher = self.pitch_stretch.as_mut().unwrap();
                stretcher.process(&self.samples, self.position, &mut self.stretch_src, self.rate)
            };
            self.position += consumed;
            if n_src + 2 < need { self.playing = false; }

            self.stretch_out.resize(output.len(), 0.0);
            let mut rp: f64 = 0.0;
            for slot in self.stretch_out.iter_mut() {
                let idx = rp as usize;
                if idx + 1 >= n_src { *slot = 0.0; continue; }
                let frac = (rp - idx as f64) as f32;
                *slot = self.stretch_src[idx] * (1.0 - frac) + self.stretch_src[idx + 1] * frac;
                rp += resample;
            }
            true
        } else {
            false
        };

        for (frame_i, sample) in output.iter_mut().enumerate() {
            let raw = if stretched {
                if frame_i >= self.stretch_out.len() { *sample = 0.0; break; }
                self.stretch_out[frame_i]
            } else {
                let idx = self.position as usize;
                if idx + 1 >= self.samples.len() {
                    *sample = 0.0;
                    self.playing = false;
                    break;
                }
                let frac = (self.position - idx as f64) as f32;
                self.samples[idx] * (1.0 - frac) + self.samples[idx + 1] * frac
            };
            // 3-band EQ (low-shelf → peaking mid → high-shelf) then filter sweep
            let eq = self.eq_high.process(self.eq_mid.process(self.eq_low.process(raw)));
            let filtered = self.filter.process(eq);
            let dry = filtered * self.volume;

            // Feed dry signal into delay buffer (post-fader send)
            let delay_read = (self.delay_write_pos + self.delay_buffer.len() - self.delay_samples) % self.delay_buffer.len();
            let delayed = self.delay_buffer[delay_read];
            // Only feed dry into delay when wet > 0 (echo is armed)
            let feed = if self.delay_wet > 0.0 { dry } else { 0.0 };
            self.delay_buffer[self.delay_write_pos] = feed + delayed * self.delay_feedback;
            self.delay_write_pos = (self.delay_write_pos + 1) % self.delay_buffer.len();

            // Output DRY only — echo is mixed separately in fill_output
            *sample = dry;

            rms_sum += dry * dry;
            written += 1;

            // The stretcher already advanced position in bulk above.
            // Direct path: slew rate per-sample so a big jump (e.g.
            // ±3% kick) doesn't land as an audible step at the buffer
            // boundary. Recompute step from the slewed rate each frame.
            if !stretched {
                if need_slew {
                    self.rate += (self.rate_target - self.rate) * rate_alpha_per_sample;
                    if (self.rate_target - self.rate).abs() < 1e-6 {
                        self.rate = self.rate_target;
                    }
                    self.position += resample * self.rate;
                } else {
                    self.position += step;
                }
            }

            // Loop wrap: if active and position passes loop_out, jump back to loop_in
            if self.loop_active
                && let (Some(lin), Some(lout)) = (self.loop_in, self.loop_out)
                    && self.position >= lout as f64 {
                        let overshoot = self.position - lout as f64;
                        self.position = lin as f64 + overshoot;
                    }
        }

        // Update metering
        if written > 0 {
            self.level = (rms_sum / written as f32).sqrt();
            self.peak = self.peak * 0.995 + self.level * 0.005; // peak decay
            if self.level > self.peak {
                self.peak = self.level;
            }

            // Kick onset detection — like a hardware BPM counter LED.
            // Heavy LPF (~100Hz) isolates kick from snares/hats, then
            // energy-delta onset detection with adaptive threshold.
            // On a silent / muted deck (volume=0) the loop is skipped
            // and detector state is reset, so when audio resumes the
            // first buffer doesn't fire a spurious kick from the stale
            // kick_prev_energy baseline.
            if self.volume > 0.001 {
                let alpha = 0.014_f32; // ~100Hz cutoff at 44100Hz
                let mut low_energy = 0.0f32;
                for i in 0..written {
                    self.kick_lp = self.kick_lp * (1.0 - alpha) + output[i] * alpha;
                    low_energy += self.kick_lp * self.kick_lp;
                }
                low_energy /= written as f32;
                let delta = (low_energy - self.kick_prev_energy).max(0.0);
                self.kick_prev_energy = low_energy;
                self.kick_avg_delta = self.kick_avg_delta * 0.97 + delta * 0.03;
                let threshold = (self.kick_avg_delta * 5.0).max(1e-6);
                if delta > threshold && low_energy > 0.001 && self.kick_hold == 0 {
                    self.kick_active = true;
                    self.kick_hold = 10;
                }
            } else {
                // Silent: clear state so resume starts cold. Cheap (a
                // few stores) and prevents the first-buffer false-fire.
                self.kick_lp = 0.0;
                self.kick_prev_energy = 0.0;
            }
            if self.kick_hold > 0 {
                self.kick_hold -= 1;
                if self.kick_hold == 0 {
                    self.kick_active = false;
                }
            }
        }

        written
    }

    /// Read echo output from the delay buffer with sweeping high-pass filter.
    /// Starts at 300Hz and sweeps up to 2kHz as progress increases.
    pub fn read_echo(&mut self, output: &mut [f32], num_frames: usize, crossfade_progress: f64) {
        // Paused decks are silent — don't leak the delay tail.
        if self.delay_wet == 0.0 || self.paused || !self.playing {
            let len = num_frames.min(output.len());
            output[..len].fill(0.0);
            return;
        }
        let buf_len = self.delay_buffer.len();
        // Sweep HPF up and LPF down based on echo lifetime
        let echo_life = (crossfade_progress / 0.4999).min(1.0);
        // HPF: 200Hz → 6000Hz
        let fc_hp = 200.0 + 5800.0 * echo_life * echo_life;
        // LPF: 16000Hz → 2000Hz
        let fc_lp = 16000.0 - 14000.0 * echo_life * echo_life;
        let alpha_hp = (1.0 / (1.0 + 2.0 * std::f64::consts::PI * fc_hp / self.output_sample_rate as f64)) as f32;
        // LPF alpha: dt / (rc + dt)
        let rc_lp = 1.0 / (2.0 * std::f64::consts::PI * fc_lp);
        let dt = 1.0 / self.output_sample_rate as f64;
        let alpha_lp = (dt / (rc_lp + dt)) as f32;

        for i in 0..num_frames.min(output.len()) {
            // Read from the most recently written delay buffer content
            // (fill_buffer already mixed dry + delayed*feedback into these positions)
            let read_pos = (self.delay_write_pos + buf_len - num_frames + i) % buf_len;
            let raw = self.delay_buffer[read_pos] * self.delay_wet;

            // HPF: two-pole (12dB/octave)
            self.delay_hp_out = alpha_hp * (self.delay_hp_out + raw - self.delay_hp_prev);
            self.delay_hp_prev = raw;
            self.delay_hp_out2 = alpha_hp * (self.delay_hp_out2 + self.delay_hp_out - self.delay_hp_prev2);
            self.delay_hp_prev2 = self.delay_hp_out;

            // LPF: two-pole (12dB/octave)
            self.delay_lp_out += alpha_lp * (self.delay_hp_out2 - self.delay_lp_out);
            self.delay_lp_out2 += alpha_lp * (self.delay_lp_out - self.delay_lp_out2);

            output[i] = self.delay_lp_out2;
        }
    }

    pub fn is_loaded(&self) -> bool {
        !self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At 0 dB all three EQ biquads should be near-unity: a DC-ish input
    /// should pass through close to its original amplitude.
    #[test]
    fn biquads_are_passthrough_at_zero_db() {
        let fs = 48_000.0;
        let mut low = Biquad::low_shelf(fs, 250.0, 0.0);
        let mut mid = Biquad::peaking(fs, 1000.0, 0.0);
        let mut high = Biquad::high_shelf(fs, 4000.0, 0.0);
        // Drive a 440 Hz tone for ~10 ms and check the peak amplitude.
        let mut peak = 0.0f32;
        for n in 0..480 {
            let t = n as f32 / fs;
            let x = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            let y = high.process(mid.process(low.process(x)));
            peak = peak.max(y.abs());
        }
        assert!((peak - 1.0).abs() < 0.05, "peak={peak} — should be ~1.0 at unity gain");
    }

    /// Low-shelf at −24 dB should strongly attenuate sub-bass content.
    #[test]
    fn low_shelf_kills_bass_at_minus_24() {
        let fs = 48_000.0;
        let mut lp = Biquad::low_shelf(fs, 250.0, -24.0);
        let mut peak = 0.0f32;
        for n in 0..4800 {
            let t = n as f32 / fs;
            let x = (2.0 * std::f32::consts::PI * 60.0 * t).sin();
            let y = lp.process(x);
            peak = peak.max(y.abs());
        }
        // 60 Hz is well below the 250 Hz shelf; −24 dB ≈ 0.063 linear.
        assert!(peak < 0.15, "peak={peak} should be heavily attenuated");
    }

    /// fill_buffer should wrap playback back to loop_in when position passes loop_out.
    #[test]
    fn loop_wraps_inside_fill_buffer() {
        let mut d = DeckPlayer::new(48_000);
        d.sample_rate = 48_000;
        d.samples = (0..1000).map(|i| i as f32).collect(); // unique sentinel per sample
        d.position = 100.0;
        d.rate = 1.0;
        d.playing = true;
        d.loop_in = Some(100);
        d.loop_out = Some(120);
        d.loop_active = true;

        let mut out = [0.0f32; 80];
        d.fill_buffer(&mut out);

        // Position should have wrapped at least once (3x through a 20-sample loop).
        assert!(d.loop_in.is_some() && d.loop_out.is_some());
        assert!(d.position >= 100.0 && d.position < 120.0,
            "position {} should wrap back into [100,120)", d.position);
    }

    /// Paused decks produce silence in read_echo, even if delay_wet > 0.
    #[test]
    fn read_echo_silent_when_paused() {
        let mut d = DeckPlayer::new(48_000);
        d.delay_wet = 1.0;
        d.delay_buffer.fill(0.5);
        d.playing = false;
        d.paused = true;
        let mut out = [1.0f32; 64];
        d.read_echo(&mut out, 64, 0.5);
        assert!(out.iter().all(|&s| s == 0.0), "echo should be zeroed when paused");
    }

    /// Stretcher-path regression guard: with a correct Stretcher impl,
    /// `deck.position` must advance by exactly `output.len() * rate`
    /// per fill. The 20–80 ms phantom-phase bug was this exact
    /// invariant failing — the stretcher read ahead for its lookahead
    /// but the deck reported the larger read as `consumed`, so
    /// `deck.position` outran audible playback and the crossfade phase
    /// sync overcorrected. Installing `FakeStretcher` lets us assert
    /// the contract without pulling in librubberband; `BrokenStretcher`
    /// is the negative control so a regression here actually fails.
    #[test]
    fn stretcher_position_accounting_matches_written_times_rate() {
        use super::super::pitch_stretch::{FakeStretcher, BrokenStretcher};

        // The deck grows `stretch_src` to `ceil(output.len() * resample) + 2`
        // and passes it to the stretcher; a correct stretcher fills the whole
        // buffer and reports `consumed = stretch_src.len() * rate`. So the
        // position delta scales linearly with rate — that's the invariant
        // we're guarding.
        let out_len = 480;
        let mut unity_delta = 0.0;
        for &rate in &[1.0_f64, 1.05, 0.95, 1.20, 0.50] {
            let mut d = DeckPlayer::new(48_000);
            d.sample_rate = 48_000;
            d.samples = vec![0.0; 50_000];
            d.position = 1000.0;
            d.rate = rate;
            d.rate_target = rate; // suppress fill_buffer's slew toward default target=1.0
            d.playing = true;
            d.pitch_stretch = Some(Box::new(FakeStretcher));

            let before = d.position;
            let mut out = vec![0.0_f32; out_len];
            d.fill_buffer(&mut out);
            let delta = d.position - before;
            if rate == 1.0 { unity_delta = delta; }
            let expected = unity_delta * rate;
            assert!((delta - expected).abs() < 1e-6,
                "correct stretcher at rate={rate}: expected Δ={expected}, got {delta}");
        }

        // Negative control: with the broken stretcher, at non-unity rate
        // `deck.position` drifts from `written * rate` — exactly the bug.
        let mut d = DeckPlayer::new(48_000);
        d.sample_rate = 48_000;
        d.samples = vec![0.0; 50_000];
        d.position = 1000.0;
        d.rate = 1.05;
        d.rate_target = 1.05;
        d.playing = true;
        d.pitch_stretch = Some(Box::new(BrokenStretcher));
        let before = d.position;
        let mut out = vec![0.0_f32; out_len];
        d.fill_buffer(&mut out);
        let delta = d.position - before;
        // Broken stretcher reports consumed = written regardless of rate.
        // At rate=1.05 that's a ~5% drift per fill — the exact bug.
        let rate_scaled = unity_delta * 1.05;
        assert!((delta - rate_scaled).abs() > 1.0,
            "broken stretcher must diverge from rate-scaled expected {rate_scaled}, got Δ={delta}");
    }

    /// unload() must return the deck to a neutral tone stack. Without
    /// this, a deck that just finished a BassSwap or FilterSweep would
    /// carry eq_low=-24 or filter=1.0 forward to the next prepare().
    #[test]
    fn unload_resets_eq_filter_and_loop_state() {
        let mut d = DeckPlayer::new(48_000);
        d.samples = vec![0.0; 1000];
        d.eq_low_db = -24.0;
        d.eq_mid_db = 6.0;
        d.eq_high_db = -12.0;
        d.filter_pos = 1.0;
        d.loop_active = true;
        d.loop_in = Some(100);
        d.loop_out = Some(200);

        let _ = d.unload();

        assert_eq!(d.eq_low_db, 0.0);
        assert_eq!(d.eq_mid_db, 0.0);
        assert_eq!(d.eq_high_db, 0.0);
        assert_eq!(d.filter_pos, 0.0);
        assert!(!d.loop_active);
        assert!(d.loop_in.is_none());
        assert!(d.loop_out.is_none());
    }

    #[test]
    fn rebuild_biquad_preserves_delay_state_across_coefficient_swap() {
        // The whole point of the helper: copy z1/z2 across so EQ knob
        // moves don't pop. If the copy ever gets dropped, this test
        // catches it before users hear the click. Init with non-zero
        // gain so the biquad actually has filtering coefficients
        // (a 0 dB low-shelf is identity, leaves z1/z2 at zero).
        let mut deck = DeckPlayer::new(48_000);
        deck.eq_low_db = 6.0;
        deck.update_eq_low();
        for _ in 0..16 {
            deck.eq_low.process(0.5);
        }
        let z1_before = deck.eq_low.z1;
        let z2_before = deck.eq_low.z2;
        assert!(z1_before.abs() + z2_before.abs() > 1e-6,
            "biquad delay line was zero, can't verify state preservation");
        // Bump again — triggers rebuild_biquad inside update_eq_low
        // with new coefficients but state must carry.
        deck.eq_low_db = -6.0;
        deck.update_eq_low();
        assert!((deck.eq_low.z1 - z1_before).abs() < 1e-9,
            "rebuild_biquad lost z1: {} vs {}", deck.eq_low.z1, z1_before);
        assert!((deck.eq_low.z2 - z2_before).abs() < 1e-9,
            "rebuild_biquad lost z2: {} vs {}", deck.eq_low.z2, z2_before);
    }

    #[test]
    fn update_filter_bypass_resets_state_to_avoid_dc_injection() {
        // After LP→bypass transition, z1/z2 must be cleared. Otherwise
        // the bypass biquad's `y = b0*x + z1` formula injects the
        // residual delay-line state from the previous LP onto the
        // first sample of identity output — a 1-2 sample DC click.
        let mut deck = DeckPlayer::new(48_000);
        // Drive an LP filter to non-zero state.
        deck.filter_pos = -0.5;
        deck.update_filter();
        for _ in 0..32 {
            deck.filter.process(0.5);
        }
        assert!(deck.filter.z1.abs() + deck.filter.z2.abs() > 1e-6,
            "LP filter delay state was zero, can't verify reset");
        // Switch to bypass.
        deck.filter_pos = 0.0;
        deck.update_filter();
        // Reset must have fired; identity coefficients pass cleanly.
        assert_eq!(deck.filter.z1, 0.0, "bypass z1 not cleared");
        assert_eq!(deck.filter.z2, 0.0, "bypass z2 not cleared");
        // Identity check: y must equal x exactly with zero state.
        let y = deck.filter.process(0.42);
        assert!((y - 0.42).abs() < 1e-6, "bypass should pass through, got {y}");
    }

    #[test]
    fn update_filter_preserves_delay_state_via_rebuild_biquad() {
        // Same property for the single-knob filter: changing filter_pos
        // (LP → HP traversal) must not drop z1/z2.
        let mut deck = DeckPlayer::new(48_000);
        deck.filter_pos = -0.5; // start in LP
        deck.update_filter();
        for _ in 0..16 {
            deck.filter.process(0.5);
        }
        let z1_before = deck.filter.z1;
        let z2_before = deck.filter.z2;
        assert!(z1_before.abs() + z2_before.abs() > 1e-6,
            "filter delay line was zero, can't verify state preservation");
        // Sweep to HP — different coefficients, but state must carry.
        deck.filter_pos = 0.5;
        deck.update_filter();
        assert!((deck.filter.z1 - z1_before).abs() < 1e-9);
        assert!((deck.filter.z2 - z2_before).abs() < 1e-9);
    }
}
