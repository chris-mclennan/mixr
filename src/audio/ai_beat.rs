//! AI-assisted beat detection using Claude API.
//! Three methods: DSP (log-energy), AI text (peak data), AI vision (waveform image).
//! Combined pipeline runs all available methods and takes consensus.

use anyhow::Result;
use crate::claude::api::ClaudeAPI;

const MODEL: &str = "claude-haiku-4-5-20251001";

/// Tool definition for beat onset detection.
fn onset_tool() -> serde_json::Value {
    serde_json::json!({
        "name": "report_onset",
        "description": "Report the detected onset position of the first kick drum beat",
        "input_schema": {
            "type": "object",
            "properties": {
                "onset_ms": {
                    "type": "number",
                    "description": "The onset time in milliseconds of the first kick drum attack"
                },
                "confidence": {
                    "type": "string",
                    "enum": ["high", "medium", "low"],
                    "description": "Confidence in the detection"
                }
            },
            "required": ["onset_ms"]
        }
    })
}

/// AI method 1: Send peak-per-ms text data, use tool calling.
pub async fn detect_from_peaks(samples: &[f32], sample_rate: u32, bpm: f64) -> Result<(f64, String)> {
    let api = ClaudeAPI::from_key_file()?;

    let bin_size = (sample_rate as usize / 1000).max(1);
    let num_ms = 2000usize.min(samples.len() / bin_size);

    let mut data_lines = Vec::new();
    for ms in 0..num_ms {
        let start = ms * bin_size;
        let end = (start + bin_size).min(samples.len());
        let peak: f32 = samples[start..end].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        if peak > 0.0005 || ms < 300 {
            data_lines.push(format!("{ms}: {peak:.4}"));
        }
    }

    let prompt = format!(
        r#"Analyze this peak amplitude per millisecond data from an electronic music track ({bpm} BPM).

Find the exact onset of the first kick drum beat. A kick onset is a sudden sharp amplitude increase (10x-1000x jump) within 1-2ms. Report the FIRST millisecond showing the sharp increase, NOT the peak.

Data (ms: peak_amplitude):
{}"#,
        data_lines.join("\n")
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 200,
        "tools": [onset_tool()],
        "tool_choice": {"type": "tool", "name": "report_onset"},
        "messages": [{"role": "user", "content": prompt}],
    });

    let json = api.post(&body).await?;
    parse_tool_response(&json)
}

/// AI method 2: Render waveform as a simple ASCII/text visualization + tool calling.
/// More visual than raw numbers — shows the shape of the waveform.
pub async fn detect_from_visual(samples: &[f32], sample_rate: u32, bpm: f64) -> Result<(f64, String)> {
    let api = ClaudeAPI::from_key_file()?;

    let bin_size = (sample_rate as usize / 1000).max(1);
    let num_ms = 500usize.min(samples.len() / bin_size); // first 500ms

    // Build ASCII waveform — each ms is one row, amplitude shown as bar
    let mut visual = String::new();
    for ms in 0..num_ms {
        let start = ms * bin_size;
        let end = (start + bin_size).min(samples.len());
        let peak: f32 = samples[start..end].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        let bar_len = (peak * 60.0) as usize;
        let bar: String = "█".repeat(bar_len.min(60));
        visual.push_str(&format!("{ms:>4}ms |{bar}\n"));
    }

    let prompt = format!(
        r#"This is an ASCII waveform of the first 500ms of an electronic music track ({bpm} BPM).
Each row is 1 millisecond. The bar length represents peak amplitude.

Find the exact millisecond where the first kick drum attack begins — the first row showing a sudden jump from silence/low to a full bar.

{visual}"#
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 200,
        "tools": [onset_tool()],
        "tool_choice": {"type": "tool", "name": "report_onset"},
        "messages": [{"role": "user", "content": prompt}],
    });

    let json = api.post(&body).await?;
    parse_tool_response(&json)
}

/// Parse the tool call response to extract onset_ms.
fn parse_tool_response(json: &serde_json::Value) -> Result<(f64, String)> {
    let content = json["content"].as_array()
        .ok_or_else(|| anyhow::anyhow!("No content in response"))?;

    for block in content {
        if block["type"].as_str() == Some("tool_use") && block["name"].as_str() == Some("report_onset") {
            let onset_ms = block["input"]["onset_ms"].as_f64()
                .ok_or_else(|| anyhow::anyhow!("No onset_ms in tool response"))?;
            let confidence = block["input"]["confidence"].as_str().unwrap_or("unknown").to_string();
            return Ok((onset_ms / 1000.0, confidence));
        }
    }

    Err(anyhow::anyhow!("No report_onset tool call in response: {json}"))
}

/// Combined pipeline: run DSP + available AI methods, take consensus.
pub async fn detect_combined(
    samples: &[f32],
    sample_rate: u32,
    bpm: f64,
) -> (f64, String) {
    // DSP result (always available, instant)
    let dsp_result = super::analyzer::find_first_beat(samples, sample_rate, bpm);

    // Try AI methods
    let ai_text = detect_from_peaks(samples, sample_rate, bpm).await;
    let ai_visual = detect_from_visual(samples, sample_rate, bpm).await;

    let mut results: Vec<(f64, &str)> = vec![(dsp_result, "dsp")];
    let mut details = format!("dsp={:.1}ms", dsp_result * 1000.0);

    if let Ok((val, conf)) = &ai_text {
        results.push((*val, "ai_text"));
        details.push_str(&format!(", ai_text={:.1}ms ({conf})", val * 1000.0));
    }
    if let Ok((val, conf)) = &ai_visual {
        results.push((*val, "ai_visual"));
        details.push_str(&format!(", ai_visual={:.1}ms ({conf})", val * 1000.0));
    }

    tracing::info!("Beat detection: {details}");

    // Consensus: if 2+ results agree within 5ms, use the average of those
    let tolerance = 0.005; // 5ms
    if results.len() >= 2 {
        for i in 0..results.len() {
            let mut agreeing: Vec<f64> = vec![results[i].0];
            for j in 0..results.len() {
                if i != j && (results[i].0 - results[j].0).abs() < tolerance {
                    agreeing.push(results[j].0);
                }
            }
            if agreeing.len() >= 2 {
                let avg = agreeing.iter().sum::<f64>() / agreeing.len() as f64;
                let method = format!("consensus ({}/{} agree)", agreeing.len(), results.len());
                tracing::info!("Beat consensus: {avg:.4}s — {method}");
                return (avg, method);
            }
        }
    }

    // No consensus — prefer AI text if available, then DSP
    if let Ok((val, _)) = ai_text {
        tracing::info!("Beat: no consensus, using ai_text={:.4}s", val);
        (val, "ai_text (no consensus)".into())
    } else {
        tracing::info!("Beat: using dsp={:.4}s", dsp_result);
        (dsp_result, "dsp only".into())
    }
}

// ── Grid Validation ─────────────────────────────────────────────

/// Result of grid validation at phrase boundaries.
#[derive(Debug, Clone)]
pub struct GridValidation {
    /// Offset in ms at each phrase boundary (positive = kick is late vs grid).
    pub phrase_offsets: Vec<(f64, f64)>, // (phrase_time, offset_ms)
    /// Suggested BPM correction (if drift is linear).
    pub bpm_correction: Option<f64>,
    /// Suggested first_beat correction in seconds.
    pub first_beat_correction: Option<f64>,
    pub details: String,
}

/// Tool definition for grid validation.
fn grid_validation_tool() -> serde_json::Value {
    serde_json::json!({
        "name": "report_grid_validation",
        "description": "Report the beat grid accuracy at each phrase boundary",
        "input_schema": {
            "type": "object",
            "properties": {
                "phrase_offsets": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "phrase_time_s": { "type": "number", "description": "Expected phrase start time in seconds" },
                            "offset_ms": { "type": "number", "description": "Offset of nearest kick from expected position. Positive = kick is later than expected, negative = earlier." },
                            "has_kick": { "type": "boolean", "description": "Whether a clear kick drum is present near the expected position" }
                        },
                        "required": ["phrase_time_s", "offset_ms", "has_kick"]
                    }
                },
                "bpm_correction": {
                    "type": "number",
                    "description": "Suggested new BPM if drift is detected, or null if BPM is correct"
                },
                "first_beat_correction_ms": {
                    "type": "number",
                    "description": "Suggested adjustment to first beat position in ms. Positive = move later."
                },
                "summary": {
                    "type": "string",
                    "description": "Brief summary of grid accuracy"
                }
            },
            "required": ["phrase_offsets", "summary"]
        }
    })
}

/// Validate beat grid accuracy at phrase boundaries using Claude.
/// Sends 2 bars of peak data around each phrase start point.
pub async fn validate_grid(
    samples: &[f32],
    sample_rate: u32,
    bpm: f64,
    first_beat: f64,
    phrases: &[super::analyzer::Phrase],
) -> Result<GridValidation> {
    let api = ClaudeAPI::from_key_file()?;

    let sr = sample_rate as usize;
    let bin_size = sr / 1000; // 1ms bins
    let bar_duration_ms = ((60.0 / bpm) * 4.0 * 1000.0) as usize;

    // Build data for each phrase boundary
    let mut all_phrase_data = Vec::new();

    for phrase in phrases {
        let phrase_time = phrase.start_time;
        let phrase_ms = (phrase_time * 1000.0) as usize;

        let window_start_ms = phrase_ms.saturating_sub(bar_duration_ms);
        let window_end_ms = (phrase_ms + bar_duration_ms).min(samples.len() / bin_size);

        if window_end_ms <= window_start_ms { continue; }

        let mut lines = Vec::new();
        for ms in window_start_ms..window_end_ms {
            let start = ms * bin_size;
            let end = (start + bin_size).min(samples.len());
            if end > samples.len() { break; }
            let peak: f32 = samples[start..end].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            // Relative ms from expected phrase start
            let rel_ms = ms as i64 - phrase_ms as i64;
            lines.push(format!("{rel_ms:>+5}: {peak:.4}"));
        }

        all_phrase_data.push(format!(
            "--- Phrase at {phrase_time:.1}s ({:?}), expected downbeat at ms 0 ---\n{}",
            phrase.phrase_type,
            lines.join("\n")
        ));
    }

    if all_phrase_data.is_empty() {
        return Err(anyhow::anyhow!("No phrase boundaries to validate"));
    }

    let prompt = format!(
        r#"You are validating beat grid accuracy for an electronic music track.

BPM: {bpm}
First beat: {first_beat:.3}s
Beat interval: {:.1}ms
Bar interval: {:.1}ms

Below is peak amplitude data (1ms resolution) around each phrase boundary.
Each section shows ±1 bar of audio around the expected downbeat position (ms=0).

For each phrase boundary:
1. Find the nearest kick drum onset (sharp amplitude jump from low to high)
2. Report its offset from ms=0 (the expected grid position)
3. If the kick is at ms=0 ±3ms, the grid is accurate there

If offsets increase consistently across phrases, the BPM is slightly wrong.
If all offsets are similar non-zero values, the first_beat needs adjustment.

{}"#,
        60000.0 / bpm,
        60000.0 / bpm * 4.0,
        all_phrase_data.join("\n\n")
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 500,
        "tools": [grid_validation_tool()],
        "tool_choice": {"type": "tool", "name": "report_grid_validation"},
        "messages": [{"role": "user", "content": prompt}],
    });

    let json = api.post(&body).await?;

    // Parse tool response
    let content = json["content"].as_array()
        .ok_or_else(|| anyhow::anyhow!("No content"))?;

    for block in content {
        if block["type"].as_str() == Some("tool_use") && block["name"].as_str() == Some("report_grid_validation") {
            let input = &block["input"];

            let phrase_offsets: Vec<(f64, f64)> = input["phrase_offsets"].as_array()
                .map(|arr| arr.iter().filter_map(|p| {
                    Some((p["phrase_time_s"].as_f64()?, p["offset_ms"].as_f64()?))
                }).collect())
                .unwrap_or_default();

            let bpm_correction = input["bpm_correction"].as_f64();
            let first_beat_correction = input["first_beat_correction_ms"].as_f64()
                .map(|ms| ms / 1000.0);
            let summary = input["summary"].as_str().unwrap_or("").to_string();

            let details = format!(
                "offsets: [{}], bpm_corr: {:?}, fb_corr: {:?}ms — {}",
                phrase_offsets.iter().map(|(t, o)| format!("{t:.0}s:{o:+.0}ms")).collect::<Vec<_>>().join(", "),
                bpm_correction,
                first_beat_correction.map(|c| c * 1000.0),
                summary
            );
            tracing::info!("Grid validation: {details}");

            return Ok(GridValidation {
                phrase_offsets,
                bpm_correction,
                first_beat_correction,
                details,
            });
        }
    }

    Err(anyhow::anyhow!("No grid validation tool call in response"))
}

// ── AI Phrase Detection ─────────────────────────────────────────

fn phrase_detection_tool() -> serde_json::Value {
    serde_json::json!({
        "name": "report_phrases",
        "description": "Report detected phrase boundaries in the track",
        "input_schema": {
            "type": "object",
            "properties": {
                "phrases": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "time_s": { "type": "number", "description": "Phrase start time in seconds" },
                            "label": { "type": "string", "enum": ["intro", "buildup", "drop", "breakdown", "outro"], "description": "Phrase type" }
                        },
                        "required": ["time_s", "label"]
                    }
                }
            },
            "required": ["phrases"]
        }
    })
}

/// AI phrase detection using beat-level energy for the full track.
/// Beat-level resolution catches transitions that happen within a bar.
pub async fn detect_phrases_ai(
    samples: &[f32],
    sample_rate: u32,
    bpm: f64,
) -> Result<Vec<(f64, String)>> {
    let api = ClaudeAPI::from_key_file()?;

    let sr = sample_rate as usize;
    let beat_samples = ((60.0 / bpm) * sr as f64) as usize;
    if beat_samples == 0 { return Err(anyhow::anyhow!("Invalid BPM")); }
    let beat_duration = 60.0 / bpm;
    let num_beats = samples.len() / beat_samples;
    let duration = samples.len() as f64 / sr as f64;

    // RMS per beat for the full track
    let mut beat_data = Vec::new();
    for beat in 0..num_beats {
        let start = beat * beat_samples;
        let end = (start + beat_samples).min(samples.len());
        let rms: f64 = {
            let sum: f64 = samples[start..end].iter().map(|s| (*s as f64) * (*s as f64)).sum();
            (sum / (end - start) as f64).sqrt()
        };
        let time = beat as f64 * beat_duration;
        let bar = beat / 4;
        let beat_in_bar = beat % 4 + 1;
        beat_data.push(format!("{beat:>4} (bar{bar:>3}.{beat_in_bar}) {time:>7.3}s: {rms:.4}"));
    }

    let prompt = format!(
        r#"Find phrase boundaries in this electronic music track.

{bpm} BPM, {num_beats} beats, {duration:.0}s.

Below is RMS energy per beat. Find where the energy profile changes suddenly.
In electronic music, phrase boundaries happen where:
- RMS jumps from <0.05 to >0.15 = **drop** (kick enters)
- RMS drops from >0.15 to <0.05 = **breakdown** (kick exits)
- RMS rises gradually over 8+ beats = **buildup**
- Energy permanently drops = **outro**

Report the EXACT time from the seconds column for each transition.
Typically 4-8 major transitions per track at 16 or 32 bar intervals.

Data (beat#, bar.beat, time_seconds, RMS):
{}"#,
        beat_data.join("\n")
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 500,
        "tools": [phrase_detection_tool()],
        "tool_choice": {"type": "tool", "name": "report_phrases"},
        "messages": [{"role": "user", "content": prompt}],
    });

    let json = api.post(&body).await?;

    let content = json["content"].as_array()
        .ok_or_else(|| anyhow::anyhow!("No content in response: {}", json))?;

    for block in content {
        if block["type"].as_str() == Some("tool_use") && block["name"].as_str() == Some("report_phrases") {
            let phrases: Vec<(f64, String)> = block["input"]["phrases"].as_array()
                .map(|arr| arr.iter().filter_map(|p| {
                    Some((p["time_s"].as_f64()?, p["label"].as_str()?.to_string()))
                }).collect())
                .unwrap_or_default();

            // Refine each phrase time to sub-beat precision using log-energy onset
            let refined: Vec<(f64, String)> = phrases.into_iter().map(|(t, label)| {
                let refined_t = refine_phrase_onset(samples, sample_rate, t);
                (refined_t, label)
            }).collect();

            let summary: Vec<String> = refined.iter()
                .map(|(t, l)| format!("{l}@{t:.3}s"))
                .collect();
            tracing::info!("AI phrases (refined): [{}]", summary.join(", "));

            return Ok(refined);
        }
    }

    Err(anyhow::anyhow!("No phrase tool call in response"))
}

/// Refine a phrase onset time to ms precision using log-energy derivative.
/// Searches ±1 second around the given time for the sharpest energy transition.
fn refine_phrase_onset(samples: &[f32], sample_rate: u32, rough_time: f64) -> f64 {
    let sr = sample_rate as usize;
    let bin_size = sr / 1000; // 1ms
    let center_ms = (rough_time * 1000.0) as usize;
    let window = 1000; // ±1 second
    let start_ms = center_ms.saturating_sub(window);
    let end_ms = (center_ms + window).min(samples.len() / bin_size);

    if end_ms <= start_ms + 2 { return rough_time; }

    let eps: f64 = 1e-10;

    // Log-energy per ms
    let num_bins = end_ms - start_ms;
    let log_e: Vec<f64> = (0..num_bins).map(|i| {
        let ms = start_ms + i;
        let s = ms * bin_size;
        let e = ((ms + 1) * bin_size).min(samples.len());
        if e > samples.len() { return -100.0; }
        let energy: f64 = samples[s..e].iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / bin_size as f64;
        10.0 * (energy + eps).log10()
    }).collect();

    // Find the biggest positive jump (energy onset)
    let mut best_jump = 0.0f64;
    let mut best_ms = center_ms;

    for i in 1..num_bins {
        let jump = log_e[i] - log_e[i - 1];
        if jump > best_jump {
            best_jump = jump;
            best_ms = start_ms + i;
        }
    }

    // Only use refined position if we found a significant jump (>10 dB)
    if best_jump > 10.0 {
        best_ms as f64 / 1000.0
    } else {
        rough_time
    }
}

// ── Live Mix Alignment ──────────────────────────────────────────

/// Result of mix alignment analysis.
#[derive(Debug, Clone)]
pub struct MixAlignment {
    /// How many ms to nudge the incoming deck. Positive = delay, negative = advance.
    pub nudge_ms: f64,
    /// Rate correction for incoming deck (1.0 = no change, >1.0 = speed up).
    pub rate_correction: Option<f64>,
    /// Whether the mix is tight enough (< 5ms offset).
    pub is_aligned: bool,
    pub details: String,
}

fn mix_alignment_tool() -> serde_json::Value {
    serde_json::json!({
        "name": "report_mix_alignment",
        "description": "Report the phase alignment between two decks during a crossfade",
        "input_schema": {
            "type": "object",
            "properties": {
                "nudge_ms": {
                    "type": "number",
                    "description": "How many ms to shift the incoming deck. Positive = incoming is early (delay it), negative = incoming is late (advance it)."
                },
                "rate_correction": {
                    "type": "number",
                    "description": "Rate multiplier for incoming deck if beats are drifting. 1.0 = no change. E.g. 1.001 = speed up 0.1%. Null if beats are stable."
                },
                "is_aligned": {
                    "type": "boolean",
                    "description": "True if beats are within 5ms — no correction needed"
                },
                "summary": {
                    "type": "string",
                    "description": "Brief description of the alignment"
                }
            },
            "required": ["nudge_ms", "is_aligned", "summary"]
        }
    })
}

/// Analyze mix alignment during crossfade.
/// Takes pre-computed per-ms peak data from both decks and asks Claude
/// if the beats are aligned, drifting, or need correction.
pub async fn analyze_mix_alignment(
    playing_peaks: &[f32],
    incoming_peaks: &[f32],
    playing_bpm: f64,
    incoming_bpm: f64,
) -> Result<MixAlignment> {
    let api = ClaudeAPI::from_key_file()?;

    let bars = 4;
    let bar_ms = ((60.0 / playing_bpm) * 4.0 * 1000.0) as usize;
    let window_ms = (bar_ms * bars).min(playing_peaks.len()).min(incoming_peaks.len());

    let mut playing_data: Vec<String> = Vec::new();
    for ms in 0..window_ms {
        let p = playing_peaks.get(ms).copied().unwrap_or(0.0);
        let i = incoming_peaks.get(ms).copied().unwrap_or(0.0);
        playing_data.push(format!("{ms}: P={p:.3} I={i:.3}"));
    }

    let prompt = format!(
        r#"You are analyzing phase alignment between two DJ decks during a crossfade.

Playing deck: {playing_bpm} BPM
Incoming deck: {incoming_bpm} BPM (rate-matched to {playing_bpm})

Below is simultaneous peak amplitude data (P=playing, I=incoming) per millisecond for {bars} bars.
Beat interval: {:.1}ms

Compare the kick drum patterns: both decks should have sharp amplitude spikes at the same millisecond positions. If the incoming deck's kicks are consistently N ms before or after the playing deck's kicks, report that as the nudge amount.

If the offset grows over the 4 bars, the incoming rate needs correction (BPM mismatch).

Data (ms from capture start: Playing_peak Incoming_peak):
{}"#,
        60000.0 / playing_bpm,
        playing_data.join("\n")
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 300,
        "tools": [mix_alignment_tool()],
        "tool_choice": {"type": "tool", "name": "report_mix_alignment"},
        "messages": [{"role": "user", "content": prompt}],
    });

    let json = api.post(&body).await?;

    let content = json["content"].as_array()
        .ok_or_else(|| anyhow::anyhow!("No content"))?;

    for block in content {
        if block["type"].as_str() == Some("tool_use") && block["name"].as_str() == Some("report_mix_alignment") {
            let input = &block["input"];
            let nudge_ms = input["nudge_ms"].as_f64().unwrap_or(0.0);
            let rate_correction = input["rate_correction"].as_f64();
            let is_aligned = input["is_aligned"].as_bool().unwrap_or(false);
            let summary = input["summary"].as_str().unwrap_or("").to_string();

            let details = format!(
                "nudge={nudge_ms:+.1}ms, rate={}, aligned={is_aligned} — {summary}",
                rate_correction.map(|r| format!("{r:.4}")).unwrap_or("none".into())
            );
            tracing::info!("Mix alignment: {details}");

            return Ok(MixAlignment {
                nudge_ms,
                rate_correction,
                is_aligned,
                details,
            });
        }
    }

    Err(anyhow::anyhow!("No mix alignment tool call in response"))
}
