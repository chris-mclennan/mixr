//! Download + decode + analyze pipeline, pulled out of `app.rs`.
//!
//! The pipeline is a pure async flow: fetch source URL, stream the file,
//! spawn a blocking analyze/decode, optionally hit the AI-beat-detection
//! endpoint, and report results via `AppAction` messages back to the UI
//! event loop. Nothing here needs an `App` reference — the caller clones
//! the handles it owns (`api`, `downloader`, `tx`) and we take them by
//! value.
//!
//! `download_in_flight` remains on `App` — the `spawn_fire_and_forget`
//! pattern here can't manage that flag directly. Callers set it before
//! spawning and clear it in the `DownloadFailed` / `TrackDecoded`
//! handlers.

use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

use crate::audio::analyzer;
use crate::beatport::api::BeatportAPI;
use crate::beatport::stream::StreamDownloader;
use crate::beatport::models::BeatportTrack;
use crate::config::{AudioQuality, AnalyzerEngine};

use super::app::AppAction;

/// Context bundle passed into each spawned task. Cheap to clone (Arcs +
/// enum scalars) so the app can recreate it per call.
pub struct Pipeline {
    pub api: Arc<TokioMutex<BeatportAPI>>,
    pub downloader: Arc<StreamDownloader>,
    pub tx: mpsc::UnboundedSender<AppAction>,
    pub quality: AudioQuality,
    pub ai_beat: bool,
    pub ai_grid: bool,
    pub ai_phrases: bool,
    /// Which BPM/key engine analyzer::analyze_and_decode_with uses.
    pub analyzer_engine: AnalyzerEngine,
}

/// Preview-mode download: fetch → decode → emit `PreviewReady`.
/// Lighter-weight than `download_and_play`: skips pipeline timing logs,
/// skips AI beat refinement, doesn't touch `download_in_flight`.
pub fn download_for_preview(p: Pipeline, track: BeatportTrack) {
    let Pipeline { api, downloader, tx, quality, analyzer_engine: engine_clone, .. } = p;

    tokio::spawn(async move {
        let (bytes, ext) = if let Some(path) = track.local_path.clone() {
            // Local-library preview: read file directly.
            let bytes_result = tokio::task::spawn_blocking(move || std::fs::read(&path)).await;
            let bytes = match bytes_result {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => { tx.send(AppAction::Toast(format!("Local read: {e}"))).ok(); return; }
                Err(_) => return,
            };
            let ext: &'static str = match track.local_path.as_deref()
                .and_then(|p| std::path::Path::new(p).extension())
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .as_deref()
            {
                Some("flac") => "flac", Some("aac") => "aac",
                Some("m4a") => "m4a", Some("mp3") => "mp3",
                Some("wav") => "wav", Some("ogg") | Some("opus") => "ogg",
                _ => "flac",
            };
            (bytes, ext)
        } else {
            let mut api_guard = api.lock().await;
            let source = match api_guard.get_track_source(track.id, quality).await {
                Ok(s) => s,
                Err(e) => { tx.send(AppAction::Toast(format!("Source error: {e}"))).ok(); return; }
            };
            drop(api_guard);
            let dl = match downloader.download(&source, track.id).await {
                Ok(d) => d,
                Err(e) => { tx.send(AppAction::Toast(format!("Download failed: {e}"))).ok(); return; }
            };
            (dl.bytes, dl.ext)
        };

        let bpm_hint = track.bpm;
        match tokio::task::spawn_blocking(move || {
            analyzer::analyze_and_decode_with_bytes(bytes, Some(ext), bpm_hint, engine_clone)
        }).await {
            Ok(Ok((analysis, samples, sample_rate))) => {
                tx.send(AppAction::PreviewReady { samples, sample_rate, analysis }).ok();
            }
            Ok(Err(e)) => { tx.send(AppAction::Toast(format!("Analysis failed: {e}"))).ok(); }
            Err(e) => { tx.send(AppAction::Toast(format!("Task failed: {e}"))).ok(); }
        }
    });
}

/// Playback-mode download: fetch → decode → optional AI-beat refinement
/// → emit `TrackDecoded` / `DownloadFailed`. Also logs a per-stage
/// pipeline timing breakdown at INFO for diagnostics.
pub fn download_and_play(p: Pipeline, track: BeatportTrack, as_incoming: bool) {
    let Pipeline { api, downloader, tx, quality, ai_beat, ai_grid, ai_phrases,
        analyzer_engine: engine_clone } = p;

    let short_name: String = {
        let artist = track.artist_name();
        let title = track.full_title();
        let full = format!("{artist} - {title}");
        if full.len() > 40 { format!("{}...", &full[..37]) } else { full }
    };

    tx.send(AppAction::Toast(format!("⬇ {short_name}"))).ok();

    tokio::spawn(async move {
        let t0 = std::time::Instant::now();

        // Local-library tracks bypass the Beatport fetch entirely:
        // read the user's file from disk, decode through the same
        // analyzer path. local_path is set by `local_library::scan_library`
        // for any track sourced from the user's configured library dir.
        let (bytes, ext, source_label) = if let Some(path) = track.local_path.clone() {
            tx.send(AppAction::Toast(format!("📂 {short_name} (LOCAL)"))).ok();
            let bytes_result = tokio::task::spawn_blocking(move || {
                std::fs::read(&path)
            }).await;
            let bytes = match bytes_result {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => { tx.send(AppAction::DownloadFailed(format!("Local file read: {e}"))).ok(); return; }
                Err(e) => { tx.send(AppAction::DownloadFailed(format!("Task panicked: {e}"))).ok(); return; }
            };
            let ext: &'static str = match track.local_path.as_deref()
                .and_then(|p| std::path::Path::new(p).extension())
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .as_deref()
            {
                Some("flac") => "flac",
                Some("aac") => "aac",
                Some("m4a") => "m4a",
                Some("mp3") => "mp3",
                Some("wav") => "wav",
                Some("ogg") | Some("opus") => "ogg",
                _ => "flac", // best-guess fallback for symphonia
            };
            (bytes, ext, "LOCAL")
        } else {
            let mut api_guard = api.lock().await;
            let source = match api_guard.get_track_source(track.id, quality).await {
                Ok(s) => s,
                Err(e) => { tx.send(AppAction::Toast(format!("Source error: {e}"))).ok(); return; }
            };
            drop(api_guard);
            let label = match &source {
                crate::beatport::api::TrackSource::Download(_) => "FLAC",
                crate::beatport::api::TrackSource::Hls(_) => "HLS",
            };
            tx.send(AppAction::Toast(format!("⬇ {short_name} ({label})"))).ok();
            let dl = match downloader.download(&source, track.id).await {
                Ok(d) => d,
                Err(e) => { tx.send(AppAction::DownloadFailed(format!("Download: {e}"))).ok(); return; }
            };
            (dl.bytes, dl.ext, label)
        };

        let t_download = t0.elapsed();
        let dl_secs = t_download.as_secs_f32();
        let size_kb = (bytes.len() / 1024) as u64;
        tx.send(AppAction::Toast(format!("Analyzing {short_name} ({size_kb}KB in {dl_secs:.1}s)"))).ok();

        let bpm_hint = track.bpm;
        let result = tokio::task::spawn_blocking(move || {
            analyzer::analyze_and_decode_with_bytes(bytes, Some(ext), bpm_hint, engine_clone)
        }).await;

        let t_total = t0.elapsed();
        let analyze_secs = (t_total - t_download).as_secs_f32();
        tracing::info!(
            "Pipeline: source+download={:.1}s ({size_kb}KB {source_label}), analyze={:.1}s, total={:.1}s — {short_name}",
            dl_secs, analyze_secs, t_total.as_secs_f32()
        );

        match result {
            Ok(Ok((mut analysis, samples, sample_rate))) => {
                let any_ai = ai_beat || ai_grid || ai_phrases;
                if any_ai {
                    tx.send(AppAction::Toast(format!("AI analysis: {short_name}"))).ok();
                }

                // 1. AI beat onset refinement
                if ai_beat {
                    let (refined_beat, reason) = crate::audio::ai_beat::detect_combined(&samples, sample_rate, analysis.beat_grid.bpm).await;
                    let old = analysis.beat_grid.first_beat_time;
                    let diff = (refined_beat - old).abs() * 1000.0;
                    if diff > 1.0 {
                        analysis.beat_grid.first_beat_time = refined_beat;
                        tracing::info!("AI refined first beat: {old:.3}s → {refined_beat:.3}s ({diff:.1}ms, {reason})");
                    } else {
                        tracing::info!("AI beat: no change ({reason}, diff={diff:.1}ms)");
                    }
                }

                // 2. AI grid validation at phrase boundaries
                if ai_grid {
                    match crate::audio::ai_beat::validate_grid(
                        &samples, sample_rate, analysis.beat_grid.bpm,
                        analysis.beat_grid.first_beat_time, &analysis.phrases,
                    ).await {
                        Ok(v) => {
                            tracing::info!("AI grid: {} offsets, {}", v.phrase_offsets.len(), v.details);
                            if let Some(fb_corr) = v.first_beat_correction
                                && fb_corr.abs() > 0.003 {
                                    let old_fb = analysis.beat_grid.first_beat_time;
                                    analysis.beat_grid.first_beat_time += fb_corr;
                                    tracing::info!(
                                        "AI grid correction: first_beat {old_fb:.3}s → {:.3}s ({:+.1}ms)",
                                        analysis.beat_grid.first_beat_time, fb_corr * 1000.0
                                    );
                                }
                            if let Some(bpm_corr) = v.bpm_correction
                                && (bpm_corr - analysis.beat_grid.bpm).abs() > 0.1 {
                                    let old_bpm = analysis.beat_grid.bpm;
                                    analysis.beat_grid.bpm = bpm_corr;
                                    tracing::info!("AI BPM correction: {old_bpm:.1} → {bpm_corr:.1}");
                                }
                        }
                        Err(e) => tracing::debug!("AI grid validation skipped: {e}"),
                    }
                }

                // 3. AI phrase detection (supplements DSP phrases)
                if ai_phrases {
                    match crate::audio::ai_beat::detect_phrases_ai(
                        &samples, sample_rate, analysis.beat_grid.bpm,
                    ).await {
                        Ok(detected_phrases) if !detected_phrases.is_empty() => {
                            let dsp_count = analysis.phrases.len();
                            // Replace DSP phrases with AI phrases if AI found more
                            if detected_phrases.len() >= dsp_count {
                                analysis.phrases = detected_phrases.iter().map(|(time, label)| {
                                    use crate::audio::analyzer::{Phrase, PhraseType};
                                    let phrase_type = match label.as_str() {
                                        "drop" => PhraseType::Drop,
                                        "buildup" => PhraseType::Buildup,
                                        "breakdown" => PhraseType::Breakdown,
                                        "intro" => PhraseType::Intro,
                                        "outro" => PhraseType::Outro,
                                        _ => PhraseType::Drop,
                                    };
                                    Phrase { start_time: *time, energy: 0.0, phrase_type }
                                }).collect();
                                tracing::info!(
                                    "AI phrases: {} (replaced DSP's {})",
                                    analysis.phrases.len(), dsp_count
                                );
                            } else {
                                tracing::info!(
                                    "AI phrases: {} (kept DSP's {} — more detailed)",
                                    detected_phrases.len(), dsp_count
                                );
                            }
                        }
                        Ok(_) => {}
                        Err(e) => tracing::debug!("AI phrase detection skipped: {e}"),
                    }
                }
                tx.send(AppAction::TrackDecoded { track, samples, sample_rate, analysis, as_incoming }).ok();
            }
            Ok(Err(e)) => { tx.send(AppAction::DownloadFailed(format!("Analysis: {e}"))).ok(); }
            Err(e) => { tx.send(AppAction::DownloadFailed(format!("Task: {e}"))).ok(); }
        }
    });
}
