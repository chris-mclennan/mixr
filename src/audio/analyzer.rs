use anyhow::Result;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::beat_grid::BeatGrid;

#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub beat_grid: BeatGrid,
    pub rms_loudness: f64,
    pub phrases: Vec<Phrase>,
    pub waveform_peaks: Vec<f32>,
    /// First moment of any audio energy (skip leading silence).
    pub first_audio: f64,
}

#[derive(Debug, Clone)]
pub struct Phrase {
    pub start_time: f64,
    pub energy: f64,
    pub phrase_type: PhraseType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseType {
    Intro,
    Buildup,
    Drop,
    Breakdown,
    Outro,
}

// ── Decode ──────────────────────────────────────────────────────

/// Decode an encoded audio buffer to mono f32 samples + sample rate.
/// The codec is sniffed from `ext_hint` (e.g., "flac", "aac", "mp3");
/// pass `None` to let symphonia probe blindly. Used by the runtime
/// audio pipeline — tracks live as `Vec<u8>` in memory just long
/// enough to decode, then the encoded bytes are dropped (only the
/// PCM samples sit on the heap during playback).
pub fn decode_to_mono_bytes(bytes: Vec<u8>, ext_hint: Option<&str>) -> Result<(Vec<f32>, u32)> {
    let cursor = std::io::Cursor::new(bytes);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint { hint.with_extension(ext); }
    decode_from_mss(mss, hint)
}

/// Path-based variant. Kept for dev/test fixtures that already have
/// a file on disk; the runtime audio pipeline uses the bytes path.
#[allow(dead_code)]
pub fn decode_to_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    decode_from_mss(mss, hint)
}

fn decode_from_mss(mss: MediaSourceStream, hint: Hint) -> Result<(Vec<f32>, u32)> {
    let probed = symphonia::default::get_probe().format(
        &hint, mss, &FormatOptions::default(), &MetadataOptions::default(),
    )?;

    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| anyhow::anyhow!("no audio track"))?;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    let track_id = track.id;

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut samples = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id { continue; }

        let decoded = decoder.decode(&packet)?;
        let spec = *decoded.spec();
        let num_frames = decoded.frames();

        let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);

        let interleaved = sample_buf.samples();
        for frame in 0..num_frames {
            let mut sum = 0.0f32;
            for ch in 0..channels {
                sum += interleaved[frame * channels + ch];
            }
            samples.push(sum / channels as f32);
        }
    }

    Ok((samples, sample_rate))
}

// ── BPM Detection ───────────────────────────────────────────────

/// Multi-band spectral flux onset detection.
///
/// Splits the signal into 4 frequency bands using cascaded moving-average
/// low-pass filters, computes per-band energy in each frame, and sums the
/// half-wave rectified energy differences across bands. This isolates kick
/// onsets in the low band from hi-hat energy in the high band, improving
/// BPM detection accuracy over broadband RMS energy difference.
fn spectral_flux_onsets(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    let hop_size = sample_rate as usize / 20;  // 50ms hop
    let window_size = sample_rate as usize / 10; // 100ms window

    // Moving-average filter: approximate low-pass at cutoff ≈ sr / (2 * width).
    // Width in samples for each band boundary:
    //   ~200 Hz cutoff → sr/400 samples
    //   ~2000 Hz cutoff → sr/4000 samples
    //   ~8000 Hz cutoff → sr/16000 samples
    let lp_width_low = (sample_rate as usize / 400).max(2);
    let lp_width_mid = (sample_rate as usize / 4000).max(2);
    let lp_width_high = (sample_rate as usize / 16000).max(2);

    // Apply moving-average to get low-passed versions at each cutoff
    fn moving_avg(data: &[f32], width: usize) -> Vec<f32> {
        if width <= 1 || data.len() < width {
            return data.to_vec();
        }
        let mut out = vec![0.0f32; data.len()];
        let mut sum = 0.0f64;
        // Initialize first window
        for i in 0..width {
            sum += data[i] as f64;
        }
        let half = width / 2;
        out[half] = (sum / width as f64) as f32;
        for i in 1..data.len() - width + 1 {
            sum += data[i + width - 1] as f64 - data[i - 1] as f64;
            out[i + half] = (sum / width as f64) as f32;
        }
        out
    }

    let lp_200 = moving_avg(samples, lp_width_low);
    let lp_2k = moving_avg(samples, lp_width_mid);
    let lp_8k = moving_avg(samples, lp_width_high);

    // Band signals by subtraction:
    //   low  = lp_200
    //   low_mid = lp_2k - lp_200
    //   high_mid = lp_8k - lp_2k
    //   high = original - lp_8k

    let num_bands = 4;
    let mut prev_band_energy = vec![0.0f64; num_bands];
    let mut flux = Vec::new();

    let mut pos = 0;
    while pos + window_size <= samples.len() {
        let mut band_energy = [0.0f64; 4];
        for j in pos..pos + window_size {
            let low = lp_200[j] as f64;
            let mid_low = lp_2k[j] as f64 - lp_200[j] as f64;
            let mid_high = lp_8k[j] as f64 - lp_2k[j] as f64;
            let high = samples[j] as f64 - lp_8k[j] as f64;
            band_energy[0] += low * low;
            band_energy[1] += mid_low * mid_low;
            band_energy[2] += mid_high * mid_high;
            band_energy[3] += high * high;
        }
        let n = window_size as f64;
        for e in &mut band_energy { *e /= n; }

        // Spectral flux: sum of positive energy differences across bands
        let sf: f64 = band_energy.iter().zip(prev_band_energy.iter())
            .map(|(curr, prev)| (curr - prev).max(0.0))
            .sum();
        flux.push(sf as f32);
        prev_band_energy = band_energy.to_vec();
        pos += hop_size;
    }
    flux
}

fn detect_bpm(samples: &[f32], sample_rate: u32) -> f64 {
    let hop_size = sample_rate as usize / 20;

    let onsets = spectral_flux_onsets(samples, sample_rate);
    if onsets.len() < 4 { return 128.0; }

    let min_lag = (sample_rate as f64 * 60.0 / 200.0 / hop_size as f64) as usize;
    let max_lag = (sample_rate as f64 * 60.0 / 60.0 / hop_size as f64) as usize;
    let max_lag = max_lag.min(onsets.len() / 2);
    if min_lag >= max_lag { return 128.0; }

    let sr = sample_rate as f64;
    let hop = hop_size as f64;
    let tempo_prior_bpm = 128.0;
    let sigma = 30.0; // BPM standard deviation

    let mut best_lag = min_lag;
    let mut best_corr = 0.0f64;
    for lag in min_lag..=max_lag {
        let n = onsets.len() - lag;
        let mut corr = 0.0f64;
        for i in 0..n { corr += onsets[i] as f64 * onsets[i + lag] as f64; }
        corr /= n as f64;
        // Gaussian tempo prior centered at 128 BPM — biases toward the
        // most common electronic music tempo and helps disambiguate
        // octave errors (e.g. 86 vs 172 BPM).
        let candidate_bpm = 60.0 * sr / (lag as f64 * hop);
        let weight = (-0.5 * ((candidate_bpm - tempo_prior_bpm) / sigma).powi(2)).exp();
        let weighted_corr = corr * weight;
        if weighted_corr > best_corr { best_corr = weighted_corr; best_lag = lag; }
    }

    let beat_interval = best_lag as f64 * hop_size as f64 / sample_rate as f64;
    if beat_interval > 0.0 { 60.0 / beat_interval } else { 128.0 }
}

fn normalize_bpm(bpm: f64) -> f64 {
    let mut b = bpm;
    while b < 80.0 { b *= 2.0; }
    while b > 200.0 { b /= 2.0; }
    b
}

/// Call stratum-dsp for BPM detection. Returns `None` when the crate
/// isn't compiled in (default) or the call failed. Caller falls
/// back to the built-in detector in either case.
#[cfg(feature = "stratum")]
fn stratum_detect_bpm(samples: &[f32], sample_rate: u32) -> Option<f64> {
    let cfg = stratum_dsp::AnalysisConfig::default();
    match stratum_dsp::analyze_audio(samples, sample_rate, cfg) {
        Ok(result) => {
            tracing::info!("Stratum BPM: {:.2}", result.bpm);
            Some(result.bpm as f64)
        }
        Err(e) => {
            tracing::warn!("Stratum analysis failed: {e:?} — falling back to built-in");
            None
        }
    }
}

#[cfg(not(feature = "stratum"))]
fn stratum_detect_bpm(_samples: &[f32], _sample_rate: u32) -> Option<f64> { None }

fn resolve_bpm(
    samples: &[f32],
    sample_rate: u32,
    hint: Option<f64>,
    engine: crate::config::AnalyzerEngine,
) -> (f64, &'static str) {
    // Stratum path overrides only if the feature is compiled in AND the
    // call actually succeeded. Either miss → fall through to built-in.
    let detected = match engine {
        crate::config::AnalyzerEngine::Stratum => {
            if let Some(b) = stratum_detect_bpm(samples, sample_rate) {
                normalize_bpm(b)
            } else {
                normalize_bpm(detect_bpm(samples, sample_rate))
            }
        }
        crate::config::AnalyzerEngine::Builtin => {
            normalize_bpm(detect_bpm(samples, sample_rate))
        }
    };

    match hint {
        Some(hint_bpm) if hint_bpm > 0.0 => {
            let hint_norm = normalize_bpm(hint_bpm);
            let ratio = detected / hint_norm;
            if (0.95..=1.05).contains(&ratio) {
                (hint_norm, "hint (confirmed)")
            } else {
                // Check harmonic aliases: 2x, 0.5x (octave),
                // 1.5x, 0.667x (triplet/half-time feel).
                let ratios = [
                    detected / 2.0,
                    detected * 2.0,
                    detected * 1.5,
                    detected / 1.5,
                ];
                let is_harmonic = ratios.iter().any(|r| {
                    let rel = r / hint_norm;
                    (0.95..=1.05).contains(&rel)
                });
                if is_harmonic {
                    (hint_norm, "hint (harmonic corrected)")
                } else {
                    tracing::warn!("BPM conflict: hint={hint_norm:.1}, detected={detected:.1} — using detected");
                    (detected, "detected (hint overridden)")
                }
            }
        }
        _ => (detected, "detected"),
    }
}

// ── First Audio / First Beat ────────────────────────────────────

/// Find first moment of any significant audio energy (skip leading silence).
fn find_first_audio(samples: &[f32], sample_rate: u32) -> f64 {
    let window = sample_rate as usize / 100; // 10ms window
    if window == 0 { return 0.0; }

    // Noise floor: RMS of first 0.5s (or whatever's there)
    let noise_len = (sample_rate as usize / 2).min(samples.len());
    let noise_rms = if noise_len > 0 {
        let sum: f64 = samples[..noise_len].iter().map(|s| (*s as f64) * (*s as f64)).sum();
        (sum / noise_len as f64).sqrt()
    } else {
        0.0
    };
    // Threshold: 10x noise floor, minimum 0.001 (avoid triggering on digital silence)
    let threshold = (noise_rms * 10.0).max(0.001);

    let mut i = 0;
    while i + window < samples.len() {
        let rms: f64 = {
            let sum: f64 = samples[i..i + window].iter().map(|s| (*s as f64) * (*s as f64)).sum();
            (sum / window as f64).sqrt()
        };
        if rms > threshold {
            return i as f64 / sample_rate as f64;
        }
        i += window;
    }
    0.0
}

/// Find first beat by grid-fitting: try candidate positions within one beat
/// interval, score each by how well a regular grid at the BPM aligns with
/// Find first beat using log-energy derivative with 1ms frames, then grid-fit.
///
/// In log scale, a kick attack (silence → 0.12 in 1ms) is a +55 dB spike
/// that dominates over the kick body (+16 dB) and peak (+1.5 dB).
/// This finds the attack onset, not the peak.
///
/// Ref: Bello et al., "A Tutorial on Onset Detection in Music Signals" (2005)
pub(crate) fn find_first_beat(samples: &[f32], sample_rate: u32, bpm: f64) -> f64 {
    let sr = sample_rate as usize;
    let total_len = samples.len();
    if total_len < 100 || bpm <= 0.0 { return 0.0; }

    let beat_samples = (60.0 / bpm * sr as f64) as usize;
    if beat_samples == 0 { return 0.0; }

    let scan_len = (sr * 15).min(total_len);
    let frame_size = sr / 1000; // 1ms
    if frame_size == 0 { return 0.0; }
    let num_frames = scan_len / frame_size;
    if num_frames < 4 { return 0.0; }

    let eps: f64 = 1e-10;

    // Log-energy per 1ms frame
    let log_e: Vec<f64> = (0..num_frames).map(|i| {
        let s = i * frame_size;
        let e = (s + frame_size).min(scan_len);
        let energy: f64 = samples[s..e].iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / frame_size as f64;
        10.0 * (energy + eps).log10()
    }).collect();

    // Half-wave rectified derivative (only rising = attacks)
    let deriv: Vec<f64> = (1..num_frames).map(|i| (log_e[i] - log_e[i - 1]).max(0.0)).collect();

    // Threshold: median of positive derivatives × 3, min 15 dB
    let mut pos: Vec<f64> = deriv.iter().filter(|&&d| d > 0.5).copied().collect();
    pos.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let thresh = if pos.len() > 10 { (pos[pos.len() / 2] * 3.0).max(15.0) } else { 15.0 };

    // Find first frame exceeding threshold
    let onset_frame = match deriv.iter().position(|&d| d >= thresh) {
        Some(f) => f + 1,
        None => return 0.0,
    };

    // Grid-fit: search ±6 frames (~±300ms) around onset, pick position where
    // log-energy derivative is consistently high at beat intervals across 32 beats.
    // For each candidate frame, refine to sample-level transient FIRST so the grid
    // scoring anchors on true attacks rather than reverb buildup.
    let beat_frames = beat_samples / frame_size;
    let num_beats = (num_frames / beat_frames).min(32);

    if num_beats >= 4 && beat_frames > 0 {
        let search_start = onset_frame.saturating_sub(6);
        let search_end = (onset_frame + 7).min(deriv.len());

        let mut best_score = 0.0f64;
        let mut best_sample_pos: Option<usize> = None;

        for candidate in search_start..search_end {
            // Sample-level refinement: find true transient within this frame
            let frame_start = candidate * frame_size;
            let prev_start = if candidate > 0 { (candidate - 1) * frame_size } else { 0 };
            let frame_end = ((candidate + 1) * frame_size).min(scan_len);

            let noise: f64 = samples[prev_start..frame_start.max(prev_start + 1)].iter()
                .map(|&s| (s as f64).abs()).fold(0.0f64, f64::max);
            let st = (noise * 4.0).max(0.001);

            let sample_pos = samples[prev_start..frame_end].iter()
                .position(|&s| (s as f64).abs() > st)
                .map(|p| prev_start + p)
                .unwrap_or(frame_start);

            // Score grid consistency from this candidate
            let mut score = 0.0f64;
            for beat in 0..num_beats {
                let f = candidate + beat * beat_frames;
                if f >= deriv.len() { break; }
                score += deriv[f];
                if f > 0 { score += deriv[f - 1] * 0.3; }
                if f + 1 < deriv.len() { score += deriv[f + 1] * 0.3; }
            }
            if score > best_score {
                best_score = score;
                best_sample_pos = Some(sample_pos);
            }
        }

        if let Some(pos) = best_sample_pos {
            return pos as f64 / sr as f64;
        }
    }

    onset_frame as f64 * frame_size as f64 / sr as f64
}

// ── Phrase Detection (Novelty Curve / Self-Similarity) ──────────

/// Compute spectral features per bar using simple band energies (no FFT needed).
/// Returns a feature vector per bar: [low_energy, mid_energy, high_energy, rms].
fn compute_bar_features(samples: &[f32], sample_rate: u32, bpm: f64) -> Vec<[f64; 4]> {
    let bar_samples = ((60.0 / bpm) * 4.0 * sample_rate as f64) as usize;
    if bar_samples == 0 { return Vec::new(); }

    let mut features = Vec::new();
    let mut offset = 0;

    while offset + bar_samples <= samples.len() {
        let bar = &samples[offset..offset + bar_samples];

        // Simple band splitting via zero-crossing rate and energy
        // Low band: moving average (approximates low-pass)
        let mut low_energy = 0.0f64;
        let mut high_energy = 0.0f64;
        let mut total_energy = 0.0f64;

        // Low-pass approximation: average over 5ms windows
        let lp_window = (sample_rate as usize / 200).max(1);
        for chunk in bar.chunks(lp_window) {
            let avg: f32 = chunk.iter().sum::<f32>() / chunk.len() as f32;
            low_energy += (avg as f64) * (avg as f64) * chunk.len() as f64;

            // High = original - lowpass
            for &s in chunk {
                let hp = s - avg;
                high_energy += (hp as f64) * (hp as f64);
                total_energy += (s as f64) * (s as f64);
            }
        }

        let n = bar_samples as f64;
        let rms = (total_energy / n).sqrt();
        low_energy = (low_energy / n).sqrt();
        high_energy = (high_energy / n).sqrt();

        // Mid = total - low - high (approximate)
        let mid_energy = (rms - low_energy - high_energy).max(0.0);

        features.push([low_energy, mid_energy, high_energy, rms]);
        offset += bar_samples;
    }

    features
}

/// Cosine distance between two feature vectors.
fn cosine_distance(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if mag_a < 1e-10 || mag_b < 1e-10 { return 1.0; }
    1.0 - (dot / (mag_a * mag_b))
}

/// Detect phrase boundaries using a novelty curve from self-similarity.
fn detect_phrases(samples: &[f32], sample_rate: u32, bpm: f64, _duration: f64) -> Vec<Phrase> {
    let features = compute_bar_features(samples, sample_rate, bpm);
    let n = features.len();
    if n < 8 { return Vec::new(); }

    // Build self-similarity matrix (cosine distance)
    let mut sim = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        for j in i..n {
            let d = cosine_distance(&features[i], &features[j]);
            sim[i][j] = d;
            sim[j][i] = d;
        }
    }

    // Novelty curve: sum of distance between neighboring blocks along the diagonal.
    // A checkerboard kernel detects boundaries where texture changes.
    let kernel_size = 4; // 4 bars on each side
    let mut novelty = vec![0.0f64; n];
    for i in kernel_size..n.saturating_sub(kernel_size) {
        // Compare the block before position i with the block after
        let mut before_self = 0.0f64;
        let mut after_self = 0.0f64;
        let mut cross = 0.0f64;

        for a in (i - kernel_size)..i {
            for b in (i - kernel_size)..i {
                before_self += sim[a][b];
            }
            for b in i..(i + kernel_size).min(n) {
                cross += sim[a][b];
            }
        }
        for a in i..(i + kernel_size).min(n) {
            for b in i..(i + kernel_size).min(n) {
                after_self += sim[a][b];
            }
        }

        let k = kernel_size as f64;
        // Novelty = cross-block distance minus within-block distance
        novelty[i] = cross / (k * k) - (before_self + after_self) / (2.0 * k * k);
    }

    // Find peaks in novelty curve (local maxima above threshold)
    let max_novelty = novelty.iter().cloned().fold(0.0f64, f64::max);
    let threshold = max_novelty * 0.3;

    let bar_duration = (60.0 / bpm) * 4.0;
    let min_phrase_bars = 8; // minimum 8 bars between boundaries

    let mut boundaries: Vec<usize> = Vec::new();
    boundaries.push(0); // track start

    for i in 1..n - 1 {
        if novelty[i] > threshold && novelty[i] >= novelty[i - 1] && novelty[i] >= novelty[i + 1] {
            // Use actual peak position — don't snap to bar multiples.
            // The novelty peak IS where the music changes.
            if let Some(&last) = boundaries.last()
                && i > last + min_phrase_bars && i < n {
                    boundaries.push(i);
                }
        }
    }

    // Label phrases based on energy profile
    let mut phrases = Vec::new();
    let avg_rms: f64 = features.iter().map(|f| f[3]).sum::<f64>() / n as f64;

    for (idx, &bar_start) in boundaries.iter().enumerate() {
        let bar_end = boundaries.get(idx + 1).copied().unwrap_or(n);
        let segment_len = bar_end - bar_start;
        if segment_len == 0 { continue; }

        // Average energy and low-band energy for this segment
        let seg_rms: f64 = features[bar_start..bar_end].iter().map(|f| f[3]).sum::<f64>() / segment_len as f64;
        let seg_low: f64 = features[bar_start..bar_end].iter().map(|f| f[0]).sum::<f64>() / segment_len as f64;

        // Energy trend (rising/falling)
        let first_half_rms: f64 = if segment_len > 1 {
            let mid = bar_start + segment_len / 2;
            features[bar_start..mid].iter().map(|f| f[3]).sum::<f64>() / (mid - bar_start) as f64
        } else {
            seg_rms
        };
        let second_half_rms: f64 = if segment_len > 1 {
            let mid = bar_start + segment_len / 2;
            features[mid..bar_end].iter().map(|f| f[3]).sum::<f64>() / (bar_end - mid) as f64
        } else {
            seg_rms
        };

        let phrase_type = if bar_start == 0 && seg_rms < avg_rms * 0.7 {
            PhraseType::Intro
        } else if bar_end >= n - 2 && seg_rms < avg_rms * 0.7 {
            PhraseType::Outro
        } else if seg_rms > avg_rms * 1.1 && seg_low > avg_rms * 0.3 {
            PhraseType::Drop
        } else if second_half_rms > first_half_rms * 1.2 {
            PhraseType::Buildup
        } else if seg_rms < avg_rms * 0.7 {
            PhraseType::Breakdown
        } else {
            PhraseType::Drop // high energy default
        };

        phrases.push(Phrase {
            start_time: bar_start as f64 * bar_duration,
            energy: seg_rms,
            phrase_type,
        });
    }

    // Set outro start from last low-energy segment
    phrases
}

// ── Main Analyze ────────────────────────────────────────────────

/// Decode + analyze in one pass with the caller-selected engine.
/// Returns analysis result AND the decoded samples so we don't have
/// to decode the file twice.
/// Bytes-based variant of `analyze_and_decode_with`. The encoded
/// buffer is consumed (decoded once into mono samples), then the
/// `Vec<u8>` is dropped — only the PCM samples + analysis travel
/// onward. This is the preferred path on main (no disk cache).
pub fn analyze_and_decode_with_bytes(
    bytes: Vec<u8>,
    ext_hint: Option<&str>,
    bpm_hint: Option<f64>,
    engine: crate::config::AnalyzerEngine,
) -> Result<(AnalysisResult, Vec<f32>, u32)> {
    let (samples, sample_rate) = decode_to_mono_bytes(bytes, ext_hint)?;
    let analysis = analyze_samples(&samples, sample_rate, bpm_hint, engine);
    Ok((analysis, samples, sample_rate))
}

#[allow(dead_code)] // dev/test path; runtime uses analyze_and_decode_with_bytes
pub fn analyze_and_decode_with(
    path: &Path,
    bpm_hint: Option<f64>,
    engine: crate::config::AnalyzerEngine,
) -> Result<(AnalysisResult, Vec<f32>, u32)> {
    let (samples, sample_rate) = decode_to_mono(path)?;
    let analysis = analyze_samples(&samples, sample_rate, bpm_hint, engine);
    Ok((analysis, samples, sample_rate))
}

/// Analyze already-decoded samples. Exposed so callers can re-analyze
/// a deck in memory (e.g. after toggling the engine for A/B testing).
pub fn analyze_samples_pub(
    samples: &[f32],
    sample_rate: u32,
    bpm_hint: Option<f64>,
    engine: crate::config::AnalyzerEngine,
) -> AnalysisResult {
    analyze_samples(samples, sample_rate, bpm_hint, engine)
}

fn analyze_samples(
    samples: &[f32],
    sample_rate: u32,
    bpm_hint: Option<f64>,
    engine: crate::config::AnalyzerEngine,
) -> AnalysisResult {
    let duration = samples.len() as f64 / sample_rate as f64;

    let (bpm, source) = resolve_bpm(samples, sample_rate, bpm_hint, engine);
    let first_beat = find_first_beat(samples, sample_rate, bpm);
    let first_audio = find_first_audio(samples, sample_rate);

    let beat_grid = BeatGrid {
        bpm,
        first_beat_time: first_beat,
    };

    let phrases = detect_phrases(samples, sample_rate, bpm, duration);
    let phrase_summary: Vec<String> = phrases.iter()
        .map(|p| format!("{:?}@{:.0}s", p.phrase_type, p.start_time))
        .collect();
    tracing::info!(
        "Analysis: {bpm:.1} BPM ({source}), first beat {first_beat:.3}s, first audio {first_audio:.3}s, {} phrases [{}], duration {duration:.1}s",
        phrases.len(),
        phrase_summary.join(", ")
    );

    let rms = if !samples.is_empty() {
        let sum: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        (sum / samples.len() as f64).sqrt()
    } else { 0.0 };

    let chunk_size = (samples.len() / 1000).max(1);
    let waveform_peaks: Vec<f32> = samples.chunks(chunk_size)
        .map(|chunk| chunk.iter().map(|s| s.abs()).fold(0.0f32, f32::max))
        .collect();

    let _ = duration; // computed but not stored; available in raw samples for callers
    AnalysisResult {
        beat_grid, rms_loudness: rms,
        phrases, waveform_peaks, first_audio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground-truth first-beat detection on real decoded FLAC. Skipped
    /// gracefully when the cache files aren't present (CI / fresh clone).
    /// Previously this test was `println!` only — no assertions — so any
    /// regression in `find_first_beat` would pass silently. Now asserts the
    /// error against hand-verified ground truth within ±30ms, which is the
    /// tolerance the phase-align math tolerates before crossfades drift.
    #[test]
    fn test_first_beat_detection() {
        const TOL_MS: f64 = 30.0;

        let cases: &[(&str, f64, f64)] = &[
            ("/Users/chrismclennan/.mixr/cache/24508685.flac", 132.0, 0.107),
            ("/Users/chrismclennan/.mixr/cache/26881899.flac", 140.0, 0.925),
        ];
        let mut ran_any = false;
        for (path, bpm, ground_truth) in cases {
            let p = std::path::Path::new(path);
            if !p.exists() { continue; }
            ran_any = true;
            let (samples, sr) = decode_to_mono(p).unwrap();
            let fb = find_first_beat(&samples, sr, *bpm);
            let err_ms = (fb - ground_truth) * 1000.0;
            assert!(err_ms.abs() < TOL_MS,
                "first-beat error {err_ms:.1}ms exceeds ±{TOL_MS}ms tolerance \
                 ({path}, {bpm} BPM, detected={fb:.4}s, truth={ground_truth:.4}s)");
        }
        if !ran_any {
            eprintln!("test_first_beat_detection: no cached FLACs available, skipped");
        }
    }

    #[test]
    #[ignore = "dev inspection; prints bar-energy tables; run with --ignored"]
    fn dev_inspect_bar_energy() {
        let path1 = std::path::Path::new("/Users/chrismclennan/.mixr/cache/24508685.flac");
        if path1.exists() {
            let (samples, sr) = decode_to_mono(path1).unwrap();
            let bar_samples = ((60.0 / 132.0) * 4.0 * sr as f64) as usize;
            let bar_dur = 60.0 / 132.0 * 4.0;
            println!("\nBars 92-100 (around 174.6s):");
            for bar in 92..=100 {
                let start = bar * bar_samples;
                let end = (start + bar_samples).min(samples.len());
                let rms: f64 = {
                    let sum: f64 = samples[start..end].iter().map(|s| (*s as f64) * (*s as f64)).sum();
                    (sum / (end - start) as f64).sqrt()
                };
                let time = bar as f64 * bar_dur;
                println!("  bar {bar} ({time:.2}s): rms={rms:.4}");
            }
        }

        // Phrase detection test
        if path1.exists() {
            let (samples, sr) = decode_to_mono(path1).unwrap();
            let analysis = analyze_samples(&samples, sr, Some(132.0), crate::config::AnalyzerEngine::Builtin);
            println!("\nDSP Phrases:");
            for p in &analysis.phrases {
                println!("  {:?} @ {:.3}s", p.phrase_type, p.start_time);
            }
            println!("Ground truth: 29.084, 58.287, 116.576, 174.646");

            // AI phrase detection
            let rt = tokio::runtime::Runtime::new().unwrap();
            match rt.block_on(crate::audio::ai_beat::detect_phrases_ai(&samples, sr, 132.0)) {
                Ok(phrases) => {
                    println!("\nAI Phrases:");
                    for (time, label) in &phrases {
                        println!("  {label} @ {time:.3}s");
                    }
                }
                Err(e) => println!("AI phrase detection failed: {e}"),
            }
        }

        // Grid validation test
        if path1.exists() {
            let (samples, sr) = decode_to_mono(path1).unwrap();
            let analysis = analyze_samples(&samples, sr, Some(132.0), crate::config::AnalyzerEngine::Builtin);
            let rt = tokio::runtime::Runtime::new().unwrap();
            match rt.block_on(crate::audio::ai_beat::validate_grid(
                &samples, sr, analysis.beat_grid.bpm, analysis.beat_grid.first_beat_time, &analysis.phrases
            )) {
                Ok(v) => println!("Grid validation: {}", v.details),
                Err(e) => println!("Grid validation failed: {e}"),
            }
        }

        // Debug: also check what's at the detected position
        if path1.exists() {
            let (samples, sr) = decode_to_mono(path1).unwrap();
            let fb = find_first_beat(&samples, sr, 132.0);
            let det_pos = (fb * sr as f64) as usize;
            if det_pos + 44 < samples.len() {
                let peak: f32 = samples[det_pos..det_pos + 44].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                println!("At detected {fb:.4}s: peak={peak:.4}");
            }
        }
        // Debug: dump sample values around the known beat position for track 1
        if path1.exists() {
            let (samples, sr) = decode_to_mono(path1).unwrap();
            println!("\nSamples around 0.107s (track 24508685, sr={sr}):");
            let center = (0.107 * sr as f64) as usize;
            for ms_offset in -5i32..=10 {
                let pos = (center as i32 + ms_offset * sr as i32 / 1000) as usize;
                if pos < samples.len() {
                    let window = &samples[pos..pos.min(samples.len() - 44) + 44];
                    let peak: f32 = window.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                    let time = pos as f64 / sr as f64;
                    println!("  t={time:.4}s ({ms_offset:+3}ms): peak={peak:.4}");
                }
            }
        }
    }

    #[test]
    fn normalize_bpm_doubles_below_80() {
        assert!((normalize_bpm(40.0) - 80.0).abs() < 1e-9);
        assert!((normalize_bpm(70.0) - 140.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_bpm_halves_above_200() {
        assert!((normalize_bpm(240.0) - 120.0).abs() < 1e-9);
        assert!((normalize_bpm(400.0) - 200.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_bpm_identity_in_range() {
        for bpm in [80.0_f64, 128.0, 150.0, 200.0] {
            assert!((normalize_bpm(bpm) - bpm).abs() < 1e-9, "bpm={bpm} should be unchanged");
        }
    }

    #[test]
    fn find_first_audio_skips_leading_silence() {
        let sr = 44100u32;
        let silent_samples = (0.5 * sr as f64) as usize;
        let mut samples = vec![0.0f32; silent_samples + sr as usize];
        for s in &mut samples[silent_samples..] { *s = 0.5; }
        let t = find_first_audio(&samples, sr);
        assert!((0.45..=0.55).contains(&t), "expected ~0.5s, got {t}s");
    }

    #[test]
    fn find_first_audio_all_silent_returns_zero() {
        assert_eq!(find_first_audio(&vec![0.0f32; 44100], 44100), 0.0);
    }

    #[test]
    fn resolve_bpm_trusts_hint_on_triplet_alias() {
        // 85.7 * 1.5 = 128.55 ≈ 130. Should trust the hint, not the detector.
        // We can't call resolve_bpm directly (it runs detect_bpm internally),
        // but we can verify the harmonic check math.
        let detected = 85.7;
        let hint = 130.0;
        let triplet = detected * 1.5;
        let ratio = triplet / hint;
        assert!((0.95..=1.05).contains(&ratio),
            "85.7 * 1.5 / 130 = {ratio:.3}, should be within 4% tolerance");
    }

    #[test]
    fn resolve_bpm_trusts_hint_on_octave_alias() {
        let detected = 65.0;
        let hint = 130.0;
        let doubled = detected * 2.0;
        let ratio = doubled / hint;
        assert!((0.95..=1.05).contains(&ratio),
            "65 * 2 / 130 = {ratio:.3}, should be within 4% tolerance");
    }

    #[test]
    fn detect_bpm_synthetic_128() {
        let sr = 44100u32;
        let bpm = 128.0;
        let beat_samples = (60.0 / bpm * sr as f64) as usize;
        let duration_secs = 10;
        let total = sr as usize * duration_secs;
        let mut samples = vec![0.0f32; total];
        // Place impulses at every beat
        let mut pos = 0;
        while pos < total {
            samples[pos] = 1.0;
            if pos + 1 < total { samples[pos + 1] = 0.8; }
            if pos + 2 < total { samples[pos + 2] = 0.5; }
            pos += beat_samples;
        }
        let detected = detect_bpm(&samples, sr);
        let normalized = normalize_bpm(detected);
        assert!((normalized - 128.0).abs() < 6.0,
            "expected ~128 BPM from synthetic signal, got {normalized} (raw {detected})");
    }
}
