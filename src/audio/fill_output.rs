use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Host, Stream};
use std::sync::{Arc, Mutex};

use super::engine::{AudioState, DeckId, EngineState, MonitorSource, profiler_enabled};
use crate::config::AppConfig;

/// Enumerate cpal output devices on the default host. Exposed for the
/// settings UI so the user can pick a monitor device.
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .ok()
        .map(|iter| iter.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Look up an output device by name; fall back to system default if
/// the name is empty or no device matches. Used by both `MixEngine::new`
/// (main output) and `build_monitor_stream` (DJ cue bus).
pub fn pick_output_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if !name.is_empty() {
        if let Ok(devs) = host.output_devices() {
            for d in devs {
                if d.name().map(|n| n == name).unwrap_or(false) {
                    return Some(d);
                }
            }
        }
        tracing::warn!("output_device {name:?} not found; using system default");
    }
    host.default_output_device()
}

/// Build the optional monitor stream. Returns None when the config entry is
/// empty or when the named device isn't found. Installs a shared sample ring
/// onto `audio_state.monitor_ring` so the main callback can push samples.
pub(crate) fn build_monitor_stream(
    host: &Host,
    config: &AppConfig,
    audio_state: &Arc<Mutex<AudioState>>,
) -> Option<Stream> {
    let name = config.monitor_device.trim();
    if name.is_empty() {
        return None;
    }
    let device = host
        .output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))?;
    let supported = device.default_output_config().ok()?;
    let channels = supported.channels() as usize;

    // Sized to hold ~1s of audio at the monitor device's actual sample
    // rate. 44.1k / 48k / 96k all work; hardcoding 48k under-caps 96k
    // devices so they'd start dropping after ~0.5s.
    let cap: usize = supported.sample_rate().0 as usize;
    let ring: Arc<Mutex<std::collections::VecDeque<f32>>> =
        Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(cap)));
    {
        let mut s = audio_state.lock().unwrap_or_else(|e| e.into_inner());
        s.monitor_ring = Some(Arc::clone(&ring));
        s.monitor_ring_cap = cap;
    }

    let ring_r = Arc::clone(&ring);
    let stream = device
        .build_output_stream(
            &supported.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // try_lock on the RT thread: if the main audio callback is mid-push,
                // output silence this buffer instead of blocking (which would stall
                // the monitor cpal thread while the other audio callback runs,
                // creating an RT-on-RT wait).
                match ring_r.try_lock() {
                    Ok(mut r) => {
                        // Mono source → fill every channel with the same sample.
                        for frame in data.chunks_mut(channels) {
                            let s = r.pop_front().unwrap_or(0.0);
                            for slot in frame.iter_mut() {
                                *slot = s;
                            }
                        }
                    }
                    Err(_) => data.fill(0.0),
                }
            },
            |err| tracing::error!("Monitor audio error: {err}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    tracing::info!("Monitor stream opened: {name}");
    Some(stream)
}

/// Convert crossfader position (−1..+1) to manual-mix crossfade progress (0..1).
///
/// Map the DJ's crossfader position (−1..+1) to crossfade progress
/// (0..1) given which deck is currently "playing" (audible-pre-mix).
/// In manual mode this drives the swap-decks trigger: when progress
/// reaches 1.0 the incoming becomes playing, identical to auto mode
/// hitting the end of the time-based curve.
///
/// The mapping is direction-aware because the physical crossfader
/// doesn't have a "playing side" — its convention flips every mix.
/// If A is playing, moving the crossfader toward +1 pulls the needle
/// toward B (incoming). If B is playing, the same crossfader move at
/// +1 is AWAY from the incoming (A) — progress decreases. So for the
/// B-playing case we invert.
pub(crate) fn manual_progress_from_crossfader(crossfader_pos: f64, playing_deck: DeckId) -> f64 {
    let raw = match playing_deck {
        DeckId::A => (crossfader_pos + 1.0) * 0.5,
        DeckId::B => (1.0 - crossfader_pos) * 0.5,
    };
    raw.clamp(0.0, 1.0)
}

pub(crate) fn apply_limiter(x: f32, mode: crate::config::LimiterMode) -> f32 {
    match mode {
        crate::config::LimiterMode::Off => x.clamp(-1.0, 1.0),
        crate::config::LimiterMode::SoftKnee => {
            let a = x.abs();
            if a < 0.7 {
                x
            } else {
                let s = if x.is_sign_negative() { -1.0 } else { 1.0 };
                s * (0.7 + 0.3 * ((a - 0.7) * 3.0).tanh())
            }
        }
    }
}

pub(crate) fn metronome_click(
    grid: super::beat_grid::BeatGrid,
    time: f64,
    _sample_rate: f64,
) -> f32 {
    let beat_phase = grid.phase(time); // 0.0 = on the beat
    let bar_phase = grid.bar_phase(time); // 0.0 = beat 1 of bar

    // Click duration: ~15ms (short, punchy)
    let click_duration = 0.015;
    let click_phase = beat_phase * grid.beat_interval();

    if click_phase > click_duration {
        return 0.0;
    }

    // Envelope: fast decay
    let envelope = 1.0 - (click_phase / click_duration);
    let envelope = envelope * envelope; // quadratic decay

    // Accent beat 1 (bar_phase near 0): higher pitch, louder
    let is_downbeat = !(0.05..=0.95).contains(&bar_phase);
    let freq = if is_downbeat { 1500.0 } else { 1000.0 };
    let amp = if is_downbeat { 0.8 } else { 0.5 };

    let sine = (2.0 * std::f64::consts::PI * freq * click_phase).sin();
    (sine * envelope * amp) as f32
}

pub(crate) fn fill_output(state: &mut AudioState, data: &mut [f32], channels: usize) {
    let prof = profiler_enabled();
    let cb_start = if prof {
        Some(std::time::Instant::now())
    } else {
        None
    };
    if channels == 0 {
        data.fill(0.0);
        return;
    }
    let frames = data.len() / channels;

    // Scratch buffers are pre-allocated to 65536 frames at construction.
    // If cpal ever requests more, zero-fill and bail — never allocate on the RT thread.
    if state.scratch_a.len() < frames {
        tracing::error!(
            "Audio buffer {frames} exceeds scratch capacity {}",
            state.scratch_a.len()
        );
        data.fill(0.0);
        return;
    }

    // Preview mode: preview deck overrides main output
    if let Some(ref mut preview) = state.preview {
        state.scratch_preview[..frames].fill(0.0);
        let buf = &mut state.scratch_preview[..frames];
        preview.fill_buffer(buf);

        if preview.current_time() >= state.preview_stop_time || !preview.playing {
            // Move to deferred_drop — tick() will drop it outside the RT thread.
            // Guard: if tick() hasn't picked up a prior deferred_drop yet (e.g.
            // rapid back-to-back previews), don't overwrite it — that would
            // call DeckPlayer::drop (freeing samples Vec, scratch bufs, etc.)
            // on the audio thread. Bail out and leave this preview alive one
            // more cycle; tick() will catch up within 16 ms.
            if state.deferred_drop.is_none() {
                state.deferred_drop = state.preview.take();
            }
            data.fill(0.0);
            return;
        }

        let sr = preview.output_sample_rate as f64;
        let grid = preview.beat_grid;

        for frame in 0..frames {
            let click = if let Some(g) = grid {
                let time = preview.current_time() - (frames - frame) as f64 / sr;
                metronome_click(g, time, sr)
            } else {
                0.0
            };

            let sample = buf[frame] * 0.7 + click; // duck music slightly for click
            for ch in 0..channels {
                data[frame * channels + ch] = sample;
            }
        }
        return;
    }

    // Capture metronome time BEFORE fill_buffer advances playback position
    let met_pre_time = if state.metronome {
        let playing = match state.playing_deck {
            DeckId::A => &state.deck_a,
            DeckId::B => &state.deck_b,
        };
        playing.beat_grid.map(|grid| {
            (
                grid,
                playing.current_time(),
                playing.output_sample_rate as f64,
            )
        })
    } else {
        None
    };

    state.scratch_a[..frames].fill(0.0);
    state.scratch_b[..frames].fill(0.0);
    state.scratch_echo_a[..frames].fill(0.0);
    state.scratch_echo_b[..frames].fill(0.0);

    let t_decks_start = if prof {
        Some(std::time::Instant::now())
    } else {
        None
    };
    state.deck_a.fill_buffer(&mut state.scratch_a[..frames]);
    state.deck_b.fill_buffer(&mut state.scratch_b[..frames]);
    let t_decks: u32 = t_decks_start
        .map(|t| t.elapsed().as_micros().min(u32::MAX as u128) as u32)
        .unwrap_or(0);

    let progress = state.crossfade_progress;
    let t_echo_start = if prof {
        Some(std::time::Instant::now())
    } else {
        None
    };
    state
        .deck_a
        .read_echo(&mut state.scratch_echo_a[..frames], frames, progress);
    state
        .deck_b
        .read_echo(&mut state.scratch_echo_b[..frames], frames, progress);
    let t_echo: u32 = t_echo_start
        .map(|t| t.elapsed().as_micros().min(u32::MAX as u128) as u32)
        .unwrap_or(0);
    let t_mix_start = if prof {
        Some(std::time::Instant::now())
    } else {
        None
    };

    // Copy scratch data to local vecs for the per-frame loop
    // (can't borrow multiple fields of state simultaneously)
    let playing_buf: &[f32] = match state.playing_deck {
        DeckId::A => &state.scratch_a[..frames],
        DeckId::B => &state.scratch_b[..frames],
    };
    let incoming_buf: &[f32] = match state.playing_deck {
        DeckId::A => &state.scratch_b[..frames],
        DeckId::B => &state.scratch_a[..frames],
    };
    let playing_echo: &[f32] = match state.playing_deck {
        DeckId::A => &state.scratch_echo_a[..frames],
        DeckId::B => &state.scratch_echo_b[..frames],
    };

    // Push the selected deck's samples to the monitor ring. Trim
    // *before* pushing so VecDeque never grows past its pre-allocated
    // capacity (no realloc on the RT thread). Cap = ~1s of the
    // monitor device's sample rate.
    //
    // monitor_source routes which deck the DJ hears in headphones:
    //   Incoming  = role-based (auto-DJ default)
    //   DeckA/B   = physical-deck pinned (manual preview)
    if let Some(ref ring) = state.monitor_ring {
        let cap = state.monitor_ring_cap;
        if cap > 0
            && let Ok(mut r) = ring.try_lock()
        {
            match state.monitor_source {
                MonitorSource::Both => {
                    // Sum both decks for headphone monitoring.
                    let need = frames;
                    let after = r.len() + need;
                    if after > cap {
                        let drop_n = after - cap;
                        for _ in 0..drop_n.min(r.len()) {
                            r.pop_front();
                        }
                    }
                    for i in 0..frames {
                        r.push_back(state.scratch_a[i] + state.scratch_b[i]);
                    }
                }
                _ => {
                    let src_buf: &[f32] = match state.monitor_source {
                        MonitorSource::Incoming => incoming_buf,
                        MonitorSource::DeckA => &state.scratch_a[..frames],
                        MonitorSource::DeckB => &state.scratch_b[..frames],
                        MonitorSource::Both => unreachable!(),
                    };
                    let need = src_buf.len();
                    let after = r.len() + need;
                    if after > cap {
                        let drop_n = after - cap;
                        for _ in 0..drop_n.min(r.len()) {
                            r.pop_front();
                        }
                    }
                    r.extend(src_buf.iter().copied());
                }
            }
        }
    }

    let met_info = met_pre_time;

    // Hoist loop-invariant state out of the per-frame loop.
    // Split cue activates slightly *before* the mix begins so the
    // stereo image is already wide when the incoming deck drops in.
    // Predicate: either already crossfading, OR playing deck is
    // within LEAD seconds of the cached trigger time. Outside that
    // window both ears get mono; per-sample slew prevents pops.
    let split_enabled = state.split_cue;
    const SPLIT_LEAD_SECS: f64 = 3.0; // head-start before mix
    const SPLIT_TRAIL_SECS: f64 = 3.0; // linger after mix completes
    let within_lead = state.state == EngineState::Playing
        && state
            .cached_trigger_time
            .map(|trigger| {
                let pt = match state.playing_deck {
                    DeckId::A => state.deck_a.current_time(),
                    DeckId::B => state.deck_b.current_time(),
                };
                let remaining = trigger - pt;
                remaining > 0.0 && remaining <= SPLIT_LEAD_SECS
            })
            .unwrap_or(false);
    let within_trail = state
        .last_crossfade_end
        .map(|t| t.elapsed().as_secs_f64() < SPLIT_TRAIL_SECS)
        .unwrap_or(false);
    let is_mixing = state.state == EngineState::Crossfading;
    let split_target: f32 = if split_enabled && (is_mixing || within_lead || within_trail) {
        1.0
    } else {
        0.0
    };
    // ~1s time constant — gradual, like the stereo image slowly
    // opening / collapsing. With 3s lead, ramp reaches ~95% by the
    // time the mix begins; trail-out fades back to mono over ~3s
    // post-swap at the same easing. Alpha is precomputed in
    // AudioState (output sample rate never changes).
    let split_alpha: f32 = state.split_alpha;
    let master_gain = state.master_gain;
    let limiter_mode = state.limiter_mode;
    let (pv, iv) = if state.state == EngineState::Crossfading {
        if state.manual_mix {
            (1.0_f32, 1.0_f32)
        } else {
            let p = state.crossfade_progress;
            (
                state.transition_type.playing_volume(p),
                state.transition_type.incoming_volume(p),
            )
        }
    } else {
        (1.0, 0.0)
    };

    // Mixer-wide faders: crossfader gain (equal-power) × per-channel faders.
    //
    // Constant-power crossfader curve: map xf ∈ [-1, +1] to angle
    // θ ∈ [0, π/2], then xf_a = cos(θ), xf_b = sin(θ). At every
    // position xf_a² + xf_b² == 1, so the perceived loudness stays
    // flat through the sweep. Center (xf=0) → θ=π/4 → both decks at
    // ≈0.707 (the −3 dB point), not 1.0 each.
    //
    // Old linear law (xf_a = 1 - max(0, xf)) gave both decks gain
    // 1.0 at center, which sums to a +3 dB loudness bump on identical
    // content — audible swell mid-crossfade. Fixes the manual-mix
    // path where fader gain is the only crossfade compensation.
    let xf = state.crossfader_pos.clamp(-1.0, 1.0);
    let theta = (xf + 1.0) * std::f32::consts::FRAC_PI_4;
    let xf_a = theta.cos() * state.channel_fader_a;
    let xf_b = theta.sin() * state.channel_fader_b;
    let (playing_fader, incoming_fader) = match state.playing_deck {
        DeckId::A => (xf_a, xf_b),
        DeckId::B => (xf_b, xf_a),
    };

    for frame in 0..frames {
        let playing_sample = playing_buf[frame] * pv * playing_fader;
        let incoming_sample = incoming_buf[frame] * iv * incoming_fader;
        // Echo is post-fader: continues after playing_volume goes to 0
        let echo_sample = playing_echo[frame];

        let click = if let Some((grid, start_time, sr)) = met_info {
            let time = start_time + frame as f64 / sr;
            metronome_click(grid, time, sr)
        } else {
            0.0
        };

        // Advance the split-cue ramp toward its target so the stereo
        // image transitions smoothly on mix start / end.
        state.split_ramp += (split_target - state.split_ramp) * split_alpha;
        let r = state.split_ramp;

        let mono = playing_sample + echo_sample + incoming_sample + click;

        if split_enabled && channels >= 2 && r > 0.0005 {
            // Deck A → left ear, Deck B → right ear (physical-deck
            // pinned, not role-based, so orientation stays stable
            // across swaps).
            let (a_out, b_out, a_echo, b_echo) = match state.playing_deck {
                DeckId::A => (playing_sample, incoming_sample, echo_sample, 0.0),
                DeckId::B => (incoming_sample, playing_sample, 0.0, echo_sample),
            };
            let l_split = a_out + a_echo + click;
            let r_split = b_out + b_echo + click;
            for ch in 0..channels {
                let split_sample = if ch % 2 == 0 { l_split } else { r_split };
                let raw = mono * (1.0 - r) + split_sample * r;
                data[frame * channels + ch] = apply_limiter(raw * master_gain, limiter_mode);
            }
        } else {
            let mixed = apply_limiter(mono * master_gain, limiter_mode);
            for ch in 0..channels {
                data[frame * channels + ch] = mixed;
            }
        }
    }

    let t_mix: u32 = t_mix_start
        .map(|t| t.elapsed().as_micros().min(u32::MAX as u128) as u32)
        .unwrap_or(0);

    // Record the per-section breakdown for this callback (only when the
    // profiler is enabled — otherwise we skipped every timing syscall above
    // and have nothing to report).
    if let Some(cb_start) = cb_start {
        let total_us = cb_start.elapsed().as_micros().min(u32::MAX as u128) as u32;
        let sr = state.deck_a.output_sample_rate as u64;
        let budget_us = (frames as u64 * 1_000_000).checked_div(sr).unwrap_or(0) as u32;
        state.profiler.push(super::profiler::CallbackSample {
            total_us,
            decks_us: t_decks,
            echo_us: t_echo,
            mix_us: t_mix,
            budget_us,
        });
    }
}
