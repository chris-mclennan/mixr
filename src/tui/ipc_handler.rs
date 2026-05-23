//! IPC command handler extracted from app.rs.
//! Pure code move — no behavior changes.

use super::app::{App, AppAction, DashFocus, ViewMode, TestMixState};

impl App {
    pub(crate) fn handle_ipc_command(&mut self, cmd: crate::ipc::IpcCommand) {
        match cmd {
            crate::ipc::IpcCommand::Skip => { self.engine.skip(); self.toast.show("Skipped (remote)", 1.0); }
            crate::ipc::IpcCommand::Pause => { self.engine.pause(); self.toast.show("Pause (remote)", 1.0); }
            crate::ipc::IpcCommand::Teleport => { self.engine.teleport(&self.config); self.toast.show("Teleport (remote)", 1.0); }
            crate::ipc::IpcCommand::MixNow => { self.engine.mix_now(); self.toast.show("Mix now (remote)", 1.0); }
            crate::ipc::IpcCommand::ClearQueue => { self.engine.clear_queue(); self.toast.show("Queue cleared (remote)", 1.0); }
            crate::ipc::IpcCommand::Nudge(dir) => { self.engine.nudge(dir); }
            crate::ipc::IpcCommand::ResumeAuto => {
                let was_paused = self.engine.resume_auto();
                self.toast.show(
                    if was_paused { "Auto-mix re-enabled" } else { "Auto-mix already on" },
                    1.0,
                );
            }
            crate::ipc::IpcCommand::LoadDeck { is_a } => {
                let deck_label = if is_a { "A" } else { "B" };
                // Resolve the highlighted track from the current screen.
                let track = self.current_screen().tracks()
                    .and_then(|tracks| tracks.get(self.selected).cloned());
                let Some(track) = track else {
                    self.toast.show(
                        &format!("Load → Deck {deck_label}: no track selected"),
                        1.5,
                    );
                    return;
                };
                // Refuse if the requested deck is currently playing —
                // hot-loading over a live deck would cut it mid-song.
                let info = &self.cached_info;
                let target_is_playing = (is_a && info.deck_a_is_playing)
                    || (!is_a && info.deck_b_is_playing);
                if target_is_playing {
                    self.toast.show(
                        &format!("Deck {deck_label} is playing — pause first"),
                        2.0,
                    );
                    return;
                }
                // Determine routing: incoming-slot if the playing deck
                // is the *other* one; playing-slot when both decks idle
                // and we're starting fresh.
                let any_playing = info.deck_a_is_playing || info.deck_b_is_playing;
                let as_incoming = any_playing;
                self.toast.show(
                    &format!("Load → Deck {deck_label}: {} - {}",
                        track.artist_name(), track.full_title()),
                    1.5,
                );
                self.download_and_play(std::sync::Arc::new(track), as_incoming);
            }
            crate::ipc::IpcCommand::NudgeDeck { is_a, direction } => {
                self.engine.nudge_deck(is_a, direction);
            }
            crate::ipc::IpcCommand::PlayDeck { is_a } => {
                self.engine.play_pause_deck(is_a);
                self.toast.show(
                    &format!("Play/Pause {}", if is_a { "A" } else { "B" }),
                    0.5,
                );
            }
            crate::ipc::IpcCommand::JumpDeck { is_a, bars } => {
                self.engine.jump_deck_bars(is_a, bars);
                self.toast.show(
                    &format!("Jump {bars:+} bars {}", if is_a { "A" } else { "B" }),
                    0.5,
                );
            }
            crate::ipc::IpcCommand::CueJump { is_a, slot } => {
                self.engine.cue_jump(is_a, slot as usize);
            }
            crate::ipc::IpcCommand::CueSet { is_a, slot } => {
                self.engine.cue_set(is_a, slot as usize);
                self.toast.show(
                    &format!("Cue {} set {}", slot + 1, if is_a { "A" } else { "B" }),
                    0.5,
                );
            }
            crate::ipc::IpcCommand::Metronome => { self.engine.toggle_metronome(); }
            crate::ipc::IpcCommand::SplitCue => { self.engine.toggle_split_cue(); }
            crate::ipc::IpcCommand::Search(query) => {
                self.toast.show(&format!("Searching: {query}"), 1.0);
                self.search_query = query;
                self.view_mode = ViewMode::Search;
                self.trigger_search();
            }
            crate::ipc::IpcCommand::QueueAll => {
                let screen = self.current_screen();
                tracing::info!("QueueAll: screen={}, items={}", screen.title(), screen.item_count());
                if let Some(tracks) = screen.tracks() {
                    let total = tracks.len();
                    let mut added = 0;
                    #[allow(clippy::unnecessary_to_owned)]
                    for track in tracks.to_vec() {
                        if self.engine.enqueue(crate::audio::engine::QueueEntry::from(track)) {
                            added += 1;
                        }
                    }
                    let skipped = total - added;
                    tracing::info!("Queued {added}/{total} tracks via IPC ({skipped} duplicates skipped)");
                    let msg = if skipped == 0 {
                        format!("Queued {added} (remote)")
                    } else {
                        format!("Queued {added}, skipped {skipped} dup (remote)")
                    };
                    self.toast.show(&msg, 1.5);
                } else {
                    tracing::info!("QueueAll: no tracks on current screen");
                    self.toast.show("No tracks to queue", 1.0);
                }
            }
            crate::ipc::IpcCommand::Status => {
                // Build the full screen context so forced writes match the
                // 2 s periodic write — otherwise smoke tests that poll
                // `{"status":1}` observe an empty `screen` field and can't
                // verify browse navigation.
                let dash_focus_label = match self.dash_focus {
                    DashFocus::Controller => "Controller",
                    DashFocus::Queue => "Queue",
                    DashFocus::History => "History",
                    DashFocus::Browse => "Browse",
                    DashFocus::Log => "Log",
                };
                let screen_title = self.current_screen().title().to_string();
                let item_count = self.current_screen().item_count();
                let screen_items: Vec<String> = (0..item_count.min(20))
                    .map(|i| self.current_screen().item_label(i))
                    .collect();
                crate::ipc::write_status_with_screen(
                    &self.cached_info,
                    &screen_title,
                    &screen_items,
                    self.toast.peek(),
                    Some(self.dash_section.label()),
                    Some(dash_focus_label),
                );
            }
            crate::ipc::IpcCommand::Restart => {
                tracing::info!("Restart requested via IPC");
                std::process::exit(75);
            }
            crate::ipc::IpcCommand::Browse(path) => {
                let segments: Vec<&str> = path.split('/').collect();
                self.browse_path(&segments);
                self.toast.show(&format!("Browse: {path}"), 1.0);
            }
            crate::ipc::IpcCommand::Navigate(dir) => {
                let count = self.current_screen().item_count();
                match dir.as_str() {
                    "up" => { if self.selected > 0 { self.selected -= 1; } }
                    "down" => { if self.selected + 1 < count { self.selected += 1; } }
                    "enter" => { self.handle_browse_enter(); }
                    "back" => { self.pop_screen(); }
                    _ => {}
                }
            }
            crate::ipc::IpcCommand::ViewDashboard => { self.view_mode = ViewMode::Dashboard; }
            crate::ipc::IpcCommand::ViewBrowse => { self.view_mode = ViewMode::Browse; self.selected = 0; }
            crate::ipc::IpcCommand::ViewQueue => { self.view_mode = ViewMode::Queue; self.selected = 0; }
            crate::ipc::IpcCommand::ViewHistory => { self.view_mode = ViewMode::History; self.selected = 0; }
            crate::ipc::IpcCommand::ViewHelp => { self.view_mode = ViewMode::Help; }
            crate::ipc::IpcCommand::ViewSettings => { self.view_mode = ViewMode::Settings; self.selected = 0; }
            crate::ipc::IpcCommand::WaveformMode => {
                self.waveform_mode = self.waveform_mode.next();
                self.toast.show(&format!("Waveform: {}", self.waveform_mode.label()), 1.0);
            }
            crate::ipc::IpcCommand::Shuffle => {
                self.engine.smart_shuffle();
                self.toast.show("Queue shuffled (IPC)", 1.0);
            }
            crate::ipc::IpcCommand::SetQuality(q) => {
                match q.to_lowercase().as_str() {
                    // FLAC unreachable on main's scope; coerce to 256k.
                    "flac" | "lossless" => self.config.audio_quality = crate::config::AudioQuality::High,
                    "256k" | "high" => self.config.audio_quality = crate::config::AudioQuality::High,
                    "128k" | "standard" => self.config.audio_quality = crate::config::AudioQuality::Standard,
                    _ => {}
                }
                self.config.save();
            }
            crate::ipc::IpcCommand::SetCrossfade(bars) => {
                if [4, 8, 16, 32, 64].contains(&bars) {
                    self.config.crossfade_bars = bars;
                    self.config.save();
                    // Engine mirrors this on its AudioState — push it
                    // through so the next mix actually uses the new
                    // length (start_crossfade reads from the engine,
                    // not AppConfig, to avoid a lock dance each call).
                    self.engine.set_crossfade_bars(bars);
                    self.toast.show(&format!("Crossfade: {bars} bars"), 1.0);
                } else {
                    self.toast.show(&format!("Invalid crossfade bars: {bars} (use 4/8/16/32/64)"), 2.0);
                }
            }
            crate::ipc::IpcCommand::Jump(bars) => {
                self.engine.jump(bars);
                self.toast.show(&format!("Jump {bars} bars (IPC)"), 1.0);
            }
            crate::ipc::IpcCommand::ShiftGrid(ms) => {
                self.engine.shift_grid(ms);
                self.toast.show(&format!("Grid shifted {ms:+.1}ms"), 1.0);
            }
            crate::ipc::IpcCommand::Extend(bars) => {
                self.engine.extend_playback(bars);
                self.toast.show(&format!("Extended {bars} bars"), 1.0);
            }
            crate::ipc::IpcCommand::SetRate { deck, rate } => {
                match deck {
                    Some(is_a) => {
                        self.engine.set_deck_rate(is_a, rate);
                        self.toast.show(
                            &format!("Tempo {}: {rate:.3}", if is_a { "A" } else { "B" }),
                            0.6,
                        );
                    }
                    None => {
                        self.engine.set_incoming_rate(rate);
                        self.toast.show(&format!("Rate: {rate:.3}"), 1.0);
                    }
                }
            }
            crate::ipc::IpcCommand::Volume { playing, incoming } => {
                if let Some(v) = playing { self.engine.set_volume(0, v as f32); }
                if let Some(v) = incoming { self.engine.set_volume(1, v as f32); }
                self.toast.show("Volume set (IPC)", 0.5);
            }
            crate::ipc::IpcCommand::SetMixIn(t) => {
                self.engine.set_mix_in_point(t);
                self.toast.show(&format!("Mix-in: {t:.1}s"), 1.0);
            }
            crate::ipc::IpcCommand::SetEq { is_a, low, mid, high } => {
                self.engine.set_eq(is_a, low, mid, high);
                self.toast.show(&format!("EQ {}: low={:?} mid={:?} high={:?}",
                    Self::deck_label(is_a), low, mid, high), 1.0);
            }
            crate::ipc::IpcCommand::SetDeckFilter { is_a, pos } => {
                self.engine.set_filter(is_a, pos);
                self.toast.show(&format!("Filter {}: {:+.2}", Self::deck_label(is_a), pos), 1.0);
            }
            crate::ipc::IpcCommand::SetChannelFader { is_a, level } => {
                self.engine.set_channel_fader(is_a, level);
                self.toast.show(&format!("Fader {}: {:.2}", Self::deck_label(is_a), level), 1.0);
            }
            crate::ipc::IpcCommand::SetCrossfader(pos) => {
                self.engine.set_crossfader(pos);
                self.toast.show(&format!("Crossfader: {pos:+.2}"), 1.0);
            }
            crate::ipc::IpcCommand::SetTransition(name) => {
                if self.engine.set_transition(&name) {
                    self.toast.show(&format!("Transition → {name}"), 1.0);
                } else {
                    self.toast.show(&format!("Unknown transition: {name}"), 1.0);
                }
            }
            crate::ipc::IpcCommand::LoopBeats { is_a, beats } => {
                self.engine.loop_beats(is_a, beats);
                self.toast.show(&format!("Loop {} {beats} beats", Self::deck_label(is_a)), 1.0);
            }
            crate::ipc::IpcCommand::LoopRelease { is_a } => {
                self.engine.loop_release(is_a);
                self.toast.show(&format!("Loop {} released", Self::deck_label(is_a)), 1.0);
            }
            crate::ipc::IpcCommand::InstallRubberband => {
                self.spawn_install_rubberband();
            }
            crate::ipc::IpcCommand::TestMix => {
                // Deterministic test harness.
                self.engine.clear_queue();
                self.browse_path(&["Discover", "Trending", "Global Top 10"]);
                self.test_mix_state = Some(TestMixState::WaitForList { ticks: 0 });
                self.toast.show("test_mix: navigating…", 1.0);
            }
            crate::ipc::IpcCommand::MasterGain(g) => {
                self.engine.set_master_gain(g);
                self.toast.show(&format!("Master gain: {:.2}", g), 1.0);
            }
            crate::ipc::IpcCommand::Profile(on) => {
                let next = on.unwrap_or(!crate::audio::engine::profiler_enabled());
                crate::audio::engine::set_profiler_enabled(next);
                self.toast.show(
                    if next { "Profiler: ON" } else { "Profiler: OFF" },
                    1.5,
                );
            }
            crate::ipc::IpcCommand::PitchStretch(name) => {
                use crate::audio::pitch_stretch::PitchStretchEngine as E;
                let engine = match name.to_ascii_lowercase().as_str() {
                    "rubberband" | "rb" => E::Rubberband,
                    "timestretch" | "ts" => E::Timestretch,
                    _ => E::Off,
                };
                self.config.pitch_stretch_engine = engine;
                self.config.save();
                self.engine.set_pitch_stretch_engine(engine);
                self.toast.show(&format!("Pitch stretch: {engine:?}"), 1.0);
            }
            crate::ipc::IpcCommand::Click { col, row, shift } => {
                // Synthetic mouse click — used by smoke tests since the
                // file-based IPC channel can't carry real mouse events.
                // Routes through the same handle_mouse_click path that
                // a real mouse would.
                let mods = if shift {
                    crossterm::event::KeyModifiers::SHIFT
                } else {
                    crossterm::event::KeyModifiers::empty()
                };
                self.handle_mouse_click(col, row, mods);
            }
            crate::ipc::IpcCommand::Drag { col, row } => {
                // Synthetic mouse drag — same path as a real
                // Drag(Left) event. Caller should click first to
                // "latch" the target, then emit drags to move it.
                self.handle_mouse_drag(col, row);
            }
            crate::ipc::IpcCommand::LayoutDump => {
                // Walk click_targets and serialize labeled ones so
                // smoke tests can look up tempoA, crossfader, etc.
                // by name. Also include axis + range extents for drag
                // targets so tests can compute the rect center or
                // any specific Y/X along the strip.
                use serde_json::{json, Value};
                let mut out = serde_json::Map::new();
                for t in &self.click_targets {
                    let Some(label) = t.label else { continue };
                    let mut entry = json!({
                        "x": t.x, "y": t.y, "w": t.w, "h": t.h,
                    });
                    match &t.action {
                        crate::tui::app::ClickAction::SetCrossfaderRange { x_min, x_max } => {
                            entry["axis"] = Value::String("x".into());
                            entry["x_min"] = Value::from(*x_min);
                            entry["x_max"] = Value::from(*x_max);
                        }
                        crate::tui::app::ClickAction::SetVerticalRange { y_min, y_max, .. } => {
                            entry["axis"] = Value::String("y".into());
                            entry["y_min"] = Value::from(*y_min);
                            entry["y_max"] = Value::from(*y_max);
                        }
                        _ => {}
                    }
                    out.insert(label.to_string(), entry);
                }
                let path = dirs::home_dir().unwrap_or_default().join(".mixr/layout.json");
                let text = serde_json::to_string_pretty(&Value::Object(out))
                    .unwrap_or_else(|_| "{}".into());
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::warn!("Layout dump write failed: {e}");
                }
            }
            crate::ipc::IpcCommand::Quantize { on, beats } => {
                self.config.quantize_on = on;
                self.config.quantize_beats = beats.max(0.001);
                self.config.save();
                self.engine.set_quantize(self.config.quantize_on, self.config.quantize_beats);
                let label = match beats {
                    b if b < 0.1875 => "1/8".to_string(),
                    b if b < 0.375  => "1/4".to_string(),
                    b if b < 0.75   => "1/2".to_string(),
                    b => format!("{b:.0}"),
                };
                self.toast.show(&format!("Quantize {} ({label} beat{})",
                    if on {"on"} else {"off"},
                    if beats >= 1.5 {"s"} else {""}), 1.0);
            }
            crate::ipc::IpcCommand::RateMix(good) => {
                let Some(ref entry) = self.last_mix_entry else {
                    self.toast.show("No recent mix to rate", 1.5);
                    return;
                };
                let mut e = entry.clone();
                // Stamp the rating time so the memory file shows when
                // the user actually gave feedback (distinct from when
                // the mix happened).
                e.rated_at = Some(chrono::Utc::now().timestamp());
                let Some(ref dj) = self.claude_dj else {
                    self.toast.show("Claude DJ not initialized", 1.5);
                    return;
                };
                if let Ok(mut dj) = dj.try_lock() {
                    if good { dj.rate_good(e); }
                    else    { dj.rate_bad(e); }
                    self.toast.show(
                        if good { "Mix rated: 👍 saved to DJ memory" }
                        else    { "Mix rated: 👎 saved to DJ memory" },
                        1.5,
                    );
                }
            }
            crate::ipc::IpcCommand::ClaudeDjSettings(patch) => {
                // Merge the JSON patch onto the current settings. Using
                // serde_json::to_value + merge-by-key lets callers flip
                // one knob without re-sending the whole block. Unknown
                // keys are silently ignored (serde skips them).
                let mut current = serde_json::to_value(&self.config.claude_dj)
                    .unwrap_or(serde_json::Value::Null);
                if let (Some(curr), Some(obj)) = (current.as_object_mut(), patch.as_object()) {
                    for (k, v) in obj { curr.insert(k.clone(), v.clone()); }
                }
                if let Ok(new) = serde_json::from_value::<crate::config::ClaudeDjSettings>(current.clone()) {
                    self.config.claude_dj = new.clone();
                    self.config.save();
                    if let Some(dj) = &self.claude_dj
                        && let Ok(mut dj) = dj.try_lock() { dj.apply_settings(new.clone()); }
                    // Engine-side manual-mix flag mirrors the settings
                    // `mode` field so the audio path acts on it live.
                    self.engine.apply_claude_dj_settings(&new, self.config.claude_dj_enabled);
                    let m = format!("{:?}", self.config.claude_dj.mode);
                    self.toast.show(&format!("Claude DJ: {m} mode"), 1.5);
                } else {
                    self.toast.show("Claude DJ settings: invalid patch", 2.0);
                }
            }
            crate::ipc::IpcCommand::PlaylistCreate(name) => {
                let Some(api) = self.api.clone() else {
                    self.toast.show("Playlist create: not logged in", 2.0);
                    return;
                };
                let tx = self.action_tx.clone();
                let pname = name.clone();
                tokio::spawn(async move {
                    let mut api = api.lock().await;
                    match api.create_playlist(&pname).await {
                        Ok(pid) => {
                            // Surface the id so scripts (smoke test,
                            // Claude DJ tools) can capture it from the
                            // toast. Format is stable for regex parsing.
                            tx.send(AppAction::Toast(format!("Playlist created: id={pid} name='{pname}'"))).ok();
                        }
                        Err(e) => { tx.send(AppAction::Toast(format!("Playlist create failed: {e}"))).ok(); }
                    }
                });
            }
            crate::ipc::IpcCommand::PlaylistDelete(id) => {
                let Some(api) = self.api.clone() else {
                    self.toast.show("Playlist delete: not logged in", 2.0);
                    return;
                };
                let tx = self.action_tx.clone();
                tokio::spawn(async move {
                    let mut api = api.lock().await;
                    match api.delete_playlist(id).await {
                        Ok(()) => { tx.send(AppAction::Toast(format!("Playlist deleted: id={id}"))).ok(); }
                        Err(e) => { tx.send(AppAction::Toast(format!("Playlist delete failed: {e}"))).ok(); }
                    }
                });
            }
            crate::ipc::IpcCommand::PlaylistDeleteRequest(id) => {
                self.toast.show(
                    &format!("Playlist {id}: pass {{\"id\":{id},\"confirm\":true}} to delete"),
                    5.0,
                );
            }
            crate::ipc::IpcCommand::MonitorSource(src) => {
                use crate::audio::engine::MonitorSource;
                let source = match src.to_lowercase().as_str() {
                    "incoming" => Some(MonitorSource::Incoming),
                    "playing" => {
                        // Map "playing" to the physical deck currently playing
                        let info = &self.cached_info;
                        if info.playing_is_a { Some(MonitorSource::DeckA) } else { Some(MonitorSource::DeckB) }
                    }
                    "both" => Some(MonitorSource::Both),
                    "a" => Some(MonitorSource::DeckA),
                    "b" => Some(MonitorSource::DeckB),
                    _ => None,
                };
                if let Some(s) = source {
                    self.engine.set_monitor_source(s);
                    self.toast.show(&format!("Monitor source: {src}"), 1.0);
                } else {
                    self.toast.show(&format!("Unknown monitor source: {src}"), 1.5);
                }
            }
            crate::ipc::IpcCommand::MonitorDevice(name) => {
                // Persist only — rebuilding the cpal monitor stream live is
                // nontrivial and the settings-UI path forces a restart. The
                // IPC path skips the restart so scripts can flip the config
                // without disrupting an active session; the new device
                // takes effect on next launch.
                self.config.monitor_device = name.clone();
                self.config.save();
                let label = if name.is_empty() { "disabled".to_string() } else { name };
                self.toast.show(&format!("Monitor device: {label} (effective on restart)"), 2.0);
            }
            crate::ipc::IpcCommand::LocalLibraryDir(path) => {
                // Set the local library path. Live: rebuild the root
                // browse menu so the "Local Library" entry appears or
                // disappears immediately; user can drill into it next
                // tick without restart.
                self.config.local_library_dir = path.clone();
                self.config.save();
                self.screen_stack = vec![
                    crate::beatport::catalog::root_screen_v2(
                        !path.is_empty(),
                        !self.config.rekordbox_xml.is_empty(),
                        !self.config.engine_dj_db.is_empty(),
                    )
                ];
                self.selected = 0;
                let label = if path.is_empty() { "disabled".to_string() } else { path };
                self.toast.show(&format!("Local library: {label}"), 2.0);
            }
            crate::ipc::IpcCommand::RekordboxXml(path) => {
                self.config.rekordbox_xml = path.clone();
                self.config.save();
                self.screen_stack = vec![
                    crate::beatport::catalog::root_screen_v2(
                        !self.config.local_library_dir.is_empty(),
                        !path.is_empty(),
                        !self.config.engine_dj_db.is_empty(),
                    )
                ];
                self.selected = 0;
                let label = if path.is_empty() { "disabled".to_string() } else { path };
                self.toast.show(&format!("Rekordbox XML: {label}"), 2.0);
            }
            crate::ipc::IpcCommand::EngineDjDb(path) => {
                self.config.engine_dj_db = path.clone();
                self.config.save();
                self.screen_stack = vec![
                    crate::beatport::catalog::root_screen_v3(
                        !self.config.local_library_dir.is_empty(),
                        !self.config.rekordbox_xml.is_empty(),
                        !path.is_empty(),
                        !self.config.serato_db.is_empty(),
                    )
                ];
                self.selected = 0;
                let label = if path.is_empty() { "disabled".to_string() } else { path };
                self.toast.show(&format!("Engine DJ DB: {label}"), 2.0);
            }
            crate::ipc::IpcCommand::SeratoDb(path) => {
                self.config.serato_db = path.clone();
                self.config.save();
                self.screen_stack = vec![
                    crate::beatport::catalog::root_screen_v3(
                        !self.config.local_library_dir.is_empty(),
                        !self.config.rekordbox_xml.is_empty(),
                        !self.config.engine_dj_db.is_empty(),
                        !path.is_empty(),
                    )
                ];
                self.selected = 0;
                let label = if path.is_empty() { "disabled".to_string() } else { path };
                self.toast.show(&format!("Serato DB: {label}"), 2.0);
            }
            crate::ipc::IpcCommand::DelayFeedback { is_a, value } => {
                self.engine.set_delay_feedback(is_a, value);
                self.toast.show(&format!("Delay feedback {}: {:.2}", Self::deck_label(is_a), value), 1.0);
            }
            crate::ipc::IpcCommand::DelaySamples { is_a, value } => {
                self.engine.set_delay_samples(is_a, value);
                self.toast.show(&format!("Delay samples {}: {}", Self::deck_label(is_a), value), 1.0);
            }
            crate::ipc::IpcCommand::DelaySync { is_a, beat_fraction } => {
                self.engine.set_delay_sync(is_a, beat_fraction);
                self.toast.show(&format!("Delay sync {}: {:.2} beats", Self::deck_label(is_a), beat_fraction), 1.0);
            }
            crate::ipc::IpcCommand::LoopIn { is_a } => {
                self.engine.loop_in(is_a);
                self.toast.show(&format!("Loop IN {}", Self::deck_label(is_a)), 1.0);
            }
            crate::ipc::IpcCommand::LoopOut { is_a } => {
                self.engine.loop_out(is_a);
                self.toast.show(&format!("Loop OUT {}", Self::deck_label(is_a)), 1.0);
            }
            crate::ipc::IpcCommand::StopDeck { is_a } => {
                self.engine.stop_deck(is_a);
                self.toast.show(&format!("Stop {}", Self::deck_label(is_a)), 1.0);
            }
            crate::ipc::IpcCommand::SeekDeck { is_a, time } => {
                self.engine.seek_deck(is_a, time);
                self.toast.show(&format!("Seek {} → {time:.1}s", Self::deck_label(is_a)), 1.0);
            }
            crate::ipc::IpcCommand::Cue { is_a, slot, action } => {
                let deck = Self::deck_label(is_a);
                match action {
                    crate::ipc::CueAction::Set => {
                        self.engine.cue_set(is_a, slot);
                        self.toast.show(&format!("Cue {} set ({deck})", slot + 1), 0.8);
                    }
                    crate::ipc::CueAction::Jump => {
                        self.engine.cue_jump(is_a, slot);
                        self.toast.show(&format!("Cue {} jump ({deck})", slot + 1), 0.5);
                    }
                    crate::ipc::CueAction::Clear => {
                        self.engine.cue_clear(is_a, slot);
                        self.toast.show(&format!("Cue {} clear ({deck})", slot + 1), 0.5);
                    }
                }
            }
            crate::ipc::IpcCommand::Diagnose => {
                // Write diagnostic info
                let info = self.engine.now_playing();
                let prof = self.engine.profile_stats();
                let diag = serde_json::json!({
                    "state": format!("{:?}", info.state),
                    "playing_bpm": info.playing_bpm,
                    "incoming_bpm": info.incoming_bpm,
                    "phase_ms": info.phase_offset_ms,
                    "crossfade_progress": info.crossfade_progress,
                    "queue_count": info.queue.len(),
                    "history_count": info.history.len(),
                    "audio_callback": prof,
                });
                let path = dirs::home_dir().unwrap_or_default().join(".mixr/diagnose.json");
                if let Ok(json) = serde_json::to_string_pretty(&diag) {
                    std::fs::write(path, json).ok();
                }
                self.toast.show("Diagnostic written", 1.0);
            }
            crate::ipc::IpcCommand::SimulateKey(c) => {
                self.handle_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(c),
                    crossterm::event::KeyModifiers::empty(),
                ));
            }
            crate::ipc::IpcCommand::SimulateKeyNamed(name) => {
                use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
                let code = match name.to_ascii_lowercase().as_str() {
                    "up" => KeyCode::Up,
                    "down" => KeyCode::Down,
                    "left" => KeyCode::Left,
                    "right" => KeyCode::Right,
                    "enter" => KeyCode::Enter,
                    "esc" | "escape" => KeyCode::Esc,
                    "tab" => KeyCode::Tab,
                    "backspace" => KeyCode::Backspace,
                    "pageup" | "pgup" => KeyCode::PageUp,
                    "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
                    "home" => KeyCode::Home,
                    "end" => KeyCode::End,
                    _ => return,
                };
                self.handle_key(KeyEvent::new(code, KeyModifiers::empty()));
            }
            crate::ipc::IpcCommand::ExportHistory => {
                let count = self.engine.export_history();
                self.toast.show(&format!("Exported {count} tracks (IPC)"), 1.0);
            }
            crate::ipc::IpcCommand::SmartShuffle => {
                self.engine.smart_shuffle();
                self.toast.show("Smart shuffled (IPC)", 1.0);
            }
            crate::ipc::IpcCommand::Favorite => {
                // Toggle favorite on current track
                if let Some(track) = self.current_screen().track_at(self.selected) {
                    let track = track.clone();
                    let added = self.favorites.toggle(&track);
                    self.toast.show(if added { "★ Favorited" } else { "Unfavorited" }, 1.0);
                }
            }
            crate::ipc::IpcCommand::Filter(text) => {
                self.view_mode = ViewMode::Browse;
                self.filtering = false;
                self.filter_text = text;
                self.selected = 0;
                self.toast.show("Filter applied (IPC)", 0.5);
            }
            crate::ipc::IpcCommand::GetScreen => {
                // Force immediate screen dump
                self.last_screen_dump = std::time::Instant::now() - std::time::Duration::from_secs(10);
            }
            crate::ipc::IpcCommand::QueueTrack(id) => {
                // Search queue/screen for track by ID and queue it
                let found = self.current_screen().tracks()
                    .and_then(|tracks| tracks.iter().find(|t| t.id == id).cloned());
                if let Some(track) = found {
                    let name = format!("{} - {}", track.artist_name(), track.full_title());
                    let added = self.engine.enqueue(crate::audio::engine::QueueEntry::from(track));
                    let msg = if added {
                        format!("Queued: {name} (IPC)")
                    } else {
                        format!("Already queued: {name} (IPC)")
                    };
                    self.toast.show(&msg, 1.0);
                } else {
                    self.toast.show(&format!("Track {id} not found"), 1.0);
                }
            }
        }
    }
}
