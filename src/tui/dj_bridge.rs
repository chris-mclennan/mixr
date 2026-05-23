//! Claude DJ bridge methods extracted from app.rs.
//! Pure code move — no behavior changes.

use std::sync::Arc;

use super::app::{App, AppAction, MEMORY_RECENT_CAP};

impl App {
    pub(crate) fn execute_dj_tool(&mut self, tool: &crate::claude::api::ToolCall) -> String {
        match tool.name.as_str() {
            "browse_screen" => {
                let bc = self.breadcrumb();
                let count = self.current_screen().item_count();
                let items: Vec<String> = (0..count.min(20)).map(|i| {
                    format!("{}: {}", i, self.current_screen().item_label(i))
                }).collect();
                // Remember this screen so the next trigger's system prompt
                // can push the DJ away from re-browsing the same place.
                if let Some(dj) = &self.claude_dj
                    && let Ok(mut dj) = dj.try_lock() {
                        dj.remember_browse(bc.clone());
                    }
                format!("Screen: {bc}\nItems ({count}):\n{}", items.join("\n"))
            }
            "select_item" => {
                let idx = tool.int("index").unwrap_or(0) as usize;
                self.selected = idx;
                self.handle_browse_enter();
                format!("Selected item {idx}")
            }
            "go_back" => {
                self.pop_screen();
                format!("Went back to: {}", self.breadcrumb())
            }
            "search_tracks" => {
                let query = tool.string("query").unwrap_or("").to_string();
                // Hard guardrail: refuse searches that look like genre,
                // BPM, or key queries. Beatport's text search matches
                // track TITLES, not metadata, so "deep house 128"
                // returns tracks named that — junk for DJ selection.
                // The right path is the browse tree (Genres > [G] > ...)
                // for genre, and we already filter compatible BPM/key
                // at queue time via the queue_track compat readout.
                const GENRE_TERMS: &[&str] = &[
                    "house", "techno", "trance", "bass", "drum", "ambient",
                    "minimal", "progressive", "melodic", "afro", "electro", "breaks",
                    "dnb", "jungle", "garage", "psytrance", "hardcore", "disco",
                    "funk", "soul", "indie", "industrial", "footwork", "uk",
                ];
                let ql = query.to_ascii_lowercase();
                let has_digit = query.chars().any(|c| c.is_ascii_digit());
                let bpm_like = ql.contains("bpm") && has_digit;
                let key_like = {
                    // Camelot key labels: 1A..12A or 1B..12B as a token.
                    // Match if the entire query is a key, or a token in
                    // a multi-word query is a key.
                    ql.split_whitespace().any(|tok| {
                        let t = tok.trim();
                        let last = t.chars().last();
                        let is_ab = matches!(last, Some('a'|'b'));
                        let num_part = if is_ab { &t[..t.len()-1] } else { t };
                        if !is_ab { return false; }
                        num_part.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))
                    })
                };
                let genre_like = GENRE_TERMS.iter().any(|g| ql.contains(g));
                if bpm_like || key_like || genre_like {
                    let why = match (genre_like, bpm_like, key_like) {
                        (true, _, _) => "looks like a genre name",
                        (_, true, _) => "looks like a BPM number",
                        _ => "looks like a Camelot key",
                    };
                    return format!(
                        "REFUSED: '{query}' {why}. search_tracks matches track \
                         TITLES, not genre/BPM/key metadata — you'd get tracks \
                         with those words in their names. Use the browse tree \
                         for genre (go_back to root, then Genres > [Genre] > \
                         Charts/Top 100). BPM/key compatibility is reported \
                         inline by queue_track. Search is for ARTIST or TRACK \
                         TITLE only."
                    );
                }
                self.search_query = query.clone();
                self.trigger_search();
                format!("Searching: {query}")
            }
            "queue_track" => {
                let idx = tool.int("index").unwrap_or(0) as usize;
                if let Some(track) = self.current_screen().track_at(idx).cloned() {
                    let name = format!("{} - {}", track.artist_name(), track.full_title());
                    // BPM/key compat feedback: compare the queued track
                    // against what's currently playing so Claude sees
                    // potential issues inline and can adjust (pick a
                    // transition, or reconsider). No hard block — the
                    // track IS queued regardless.
                    let compat = {
                        let playing_bpm = self.cached_info.playing_bpm.unwrap_or(0.0);
                        let playing_key = self.cached_info.playing_track.as_ref()
                            .and_then(|t| t.key.as_deref());
                        let track_bpm = track.bpm.unwrap_or(0.0);
                        let track_key = track.key.as_deref();
                        let mut notes = Vec::new();
                        if playing_bpm > 0.0 && track_bpm > 0.0 {
                            let gap_pct = ((track_bpm - playing_bpm) / playing_bpm * 100.0).abs();
                            if gap_pct > 8.0 {
                                notes.push(format!("⚠ BPM gap {gap_pct:.0}% ({playing_bpm:.0}→{track_bpm:.0}) — consider EchoOut"));
                            } else {
                                notes.push(format!("BPM {playing_bpm:.0}→{track_bpm:.0} ({gap_pct:.1}%)"));
                            }
                        }
                        if let (Some(pk), Some(tk)) = (playing_key, track_key) {
                            let dist = crate::audio::transition::camelot_distance(pk, tk);
                            match dist {
                                0 => notes.push(format!("key {pk}={tk} (same)")),
                                1 => notes.push(format!("key {pk}→{tk} (compatible)")),
                                2..=3 => notes.push(format!("key {pk}→{tk} (dist {dist} — workable)")),
                                _ => notes.push(format!("⚠ key {pk}→{tk} (dist {dist} — risky clash)")),
                            }
                        }
                        if notes.is_empty() { String::new() } else { format!(" [{}]", notes.join(", ")) }
                    };
                    self.engine.enqueue(crate::audio::engine::QueueEntry::from(track));
                    if let Some(dj) = &self.claude_dj
                        && let Ok(mut dj) = dj.try_lock() {
                            dj.remember_queued(name.clone());
                        }
                    format!("Queued: {name}{compat}")
                } else {
                    "No track at that index".into()
                }
            }
            "queue_all" => {
                if let Some(tracks) = self.current_screen().tracks() {
                    let count = tracks.len();
                    let titles: Vec<String> = tracks.iter().take(MEMORY_RECENT_CAP).map(|t| {
                        format!("{} - {}", t.artist_name(), t.full_title())
                    }).collect();
                    #[allow(clippy::unnecessary_to_owned)]
                    for t in tracks.to_vec() {
                        self.engine.enqueue(crate::audio::engine::QueueEntry::from(t));
                    }
                    if let Some(dj) = &self.claude_dj
                        && let Ok(mut dj) = dj.try_lock() {
                            for title in titles { dj.remember_queued(title); }
                        }
                    format!("Queued {count} tracks")
                } else {
                    "No tracks on current screen".into()
                }
            }
            "mix_now" => { self.engine.mix_now(); "Crossfade triggered".into() }
            "skip_track" => { self.engine.skip(); "Skipped".into() }
            "read_phase" => {
                let info = &self.cached_info;
                let playing = info.playing_track.as_ref().map(|t| format!("{} - {} ({:.0} BPM, {})", t.artist_name(), t.full_title(), t.bpm.unwrap_or(0.0), t.key.as_deref().unwrap_or("?"))).unwrap_or("None".into());
                let incoming = info.incoming_track.as_ref().map(|t| format!("{} - {} ({:.0} BPM, {})", t.artist_name(), t.full_title(), t.bpm.unwrap_or(0.0), t.key.as_deref().unwrap_or("?"))).unwrap_or("None".into());
                let remaining = info.playing_duration - info.playing_time;
                format!("Playing: {playing}\nIncoming: {incoming}\nPhase: {:+.1}ms\nTime remaining: {:.0}s\nQueue: {} tracks\nState: {:?}", info.phase_offset_ms, remaining, info.queue.len(), info.state)
            }
            "adjust_tempo" => {
                let deck = tool.string("deck").unwrap_or("playing");
                let bpm = tool.float("bpm").unwrap_or(128.0);
                if deck == "playing" {
                    let native = self.cached_info.playing_track.as_ref().and_then(|t| t.bpm).unwrap_or(128.0);
                    self.engine.set_playing_rate(bpm / native);
                } else {
                    let native = self.cached_info.incoming_track.as_ref().and_then(|t| t.bpm).unwrap_or(128.0);
                    self.engine.set_incoming_rate(bpm / native);
                }
                format!("Set {deck} tempo to {bpm:.1} BPM")
            }
            "nudge" => {
                let dir = if tool.string("direction") == Some("forward") { 1 } else { -1 };
                self.engine.nudge(dir);
                format!("Nudged {}", if dir > 0 { "forward" } else { "backward" })
            }
            "set_crossfade_bars" => {
                let bars = tool.int("bars").unwrap_or(16) as u32;
                if (4..=64).contains(&bars) {
                    self.config.crossfade_bars = bars;
                    self.config.save();
                }
                format!("Crossfade set to {bars} bars")
            }
            "extend_playback" => {
                let bars = tool.int("bars").unwrap_or(8) as i32;
                self.engine.extend_playback(bars);
                format!("Extended by {bars} bars")
            }
            "set_eq" => {
                let is_a = tool.string("deck").map(|s| s.eq_ignore_ascii_case("a")).unwrap_or(true);
                let low = tool.float("low").map(|v| v as f32);
                let mid = tool.float("mid").map(|v| v as f32);
                let high = tool.float("high").map(|v| v as f32);
                self.engine.set_eq(is_a, low, mid, high);
                format!("EQ {} → low={:?} mid={:?} high={:?}", Self::deck_label(is_a), low, mid, high)
            }
            "set_filter" => {
                let is_a = tool.string("deck").map(|s| s.eq_ignore_ascii_case("a")).unwrap_or(true);
                let pos = tool.float("pos").unwrap_or(0.0) as f32;
                self.engine.set_filter(is_a, pos);
                format!("Filter {} → {pos:+.2}", Self::deck_label(is_a))
            }
            "set_transition" => {
                let t = tool.string("type").unwrap_or("BeatMatched");
                if self.engine.set_transition(t) {
                    format!("Transition → {t}")
                } else {
                    format!("Unknown transition: {t}")
                }
            }
            "loop_beats" => {
                let is_a = tool.string("deck").map(|s| s.eq_ignore_ascii_case("a")).unwrap_or(true);
                let beats = tool.float("beats").unwrap_or(4.0);
                self.engine.loop_beats(is_a, beats);
                format!("Loop {} for {beats} beats", Self::deck_label(is_a))
            }
            // --- Manual-mode tools ---
            "load_to_deck" => {
                let deck = tool.string("deck").unwrap_or("b");
                let idx = tool.int("index").unwrap_or(0) as usize;
                let is_a = deck.eq_ignore_ascii_case("a");
                // Scope check: if the DJ is asking for the currently-
                // playing deck, that means they want to overwrite
                // what's audible. Refuse loudly — almost always a bug
                // in the DJ's reasoning (it mixed up A vs B).
                let playing_is_a = self.cached_info.playing_is_a;
                if is_a == playing_is_a {
                    return format!(
                        "deck {} is currently playing live; load onto the other deck",
                        if is_a { "A" } else { "B" }
                    );
                }
                // Otherwise: enqueue at front so the existing
                // next-track load flow targets the correct (incoming)
                // slot. This reuses the download + analyze pipeline
                // without duplicating it.
                if let Some(track) = self.current_screen().track_at(idx).cloned() {
                    let name = format!("{} - {}", track.artist_name(), track.full_title());
                    let entry = crate::audio::engine::QueueEntry::from(track);
                    self.engine.enqueue_front(entry);
                    format!("Cued to deck {}: {name}", if is_a { "A" } else { "B" })
                } else {
                    "No track at that index".into()
                }
            }
            "set_crossfader" => {
                let pos = tool.float("pos").unwrap_or(0.0) as f32;
                self.engine.set_crossfader(pos);
                format!("Crossfader → {pos:+.2}")
            }
            "sweep_crossfader" => {
                let target = tool.float("target").unwrap_or(0.0) as f32;
                let bars = tool.int("bars").unwrap_or(8).clamp(1, 32) as u32;
                self.engine.sweep_crossfader(target, bars);
                format!("Sweeping crossfader → {target:+.2} over {bars} bars (engine paces — no further calls needed)")
            }
            "set_channel_fader" => {
                let deck = tool.string("deck").unwrap_or("a");
                let level = tool.float("level").unwrap_or(1.0) as f32;
                let is_a = deck.eq_ignore_ascii_case("a");
                self.engine.set_channel_fader(is_a, level);
                format!("Deck {} fader → {:.2}", if is_a { "A" } else { "B" }, level)
            }
            "jump_beats" => {
                let deck = tool.string("deck").unwrap_or("a");
                let beats = tool.int("beats").unwrap_or(0) as i32;
                let is_a = deck.eq_ignore_ascii_case("a");
                self.engine.jump_deck_beats(is_a, beats);
                format!("Deck {} jump {beats:+} beats", if is_a { "A" } else { "B" })
            }
            "seek_deck" => {
                let deck = tool.string("deck").unwrap_or("a");
                let is_a = deck.eq_ignore_ascii_case("a");
                // `target` is either a number of seconds or a label.
                if let Some(secs) = tool.float("target") {
                    self.engine.seek_deck_time(is_a, secs);
                    format!("Deck {} seek → {secs:.2}s", if is_a { "A" } else { "B" })
                } else if let Some(label) = tool.string("target") {
                    match self.engine.seek_deck_named(is_a, label) {
                        Some(t) => format!("Deck {} seek to {label} ({t:.2}s)", if is_a { "A" } else { "B" }),
                        None => format!("Deck {} seek to '{label}' failed — unknown label or no analysis yet",
                            if is_a { "A" } else { "B" }),
                    }
                } else {
                    "seek_deck: target must be a number (seconds) or a label".into()
                }
            }
            "read_alignment" => {
                let a = self.engine.read_alignment();
                let one_mismatch = a.beat_in_bar_a != a.beat_in_bar_b;
                let phrase_mismatch = a.bar_in_phrase_a != a.bar_in_phrase_b;
                let mut notes = Vec::new();
                if a.beat_phase_ms.abs() > 5.0 {
                    notes.push(format!("phase {:+.1}ms — adjust tempo or nudge", a.beat_phase_ms));
                }
                if one_mismatch {
                    let delta = (a.beat_in_bar_b as i32 - a.beat_in_bar_a as i32).rem_euclid(4);
                    notes.push(format!("1s are OFF by {} beat(s) — jump_beats to fix", delta));
                }
                if phrase_mismatch {
                    let delta = (a.bar_in_phrase_b as i32 - a.bar_in_phrase_a as i32).rem_euclid(16);
                    notes.push(format!("phrase is off by {delta} bar(s) — drops won't land together"));
                }
                if notes.is_empty() { notes.push("aligned — phase, 1s, and phrase all line up".into()); }
                format!(
                    "beat_phase_ms={:+.1}\nbeat_phase_fraction={:.3}\nbeat_in_bar: A={} B={}\nbar_in_phrase: A={} B={}\n{}",
                    a.beat_phase_ms,
                    a.beat_phase_fraction,
                    a.beat_in_bar_a, a.beat_in_bar_b,
                    a.bar_in_phrase_a, a.bar_in_phrase_b,
                    notes.join(" | ")
                )
            }
            "preview_deck" => {
                let deck = tool.string("deck").unwrap_or("b");
                let is_a = deck.eq_ignore_ascii_case("a");
                if self.config.monitor_device.is_empty() {
                    "preview_deck needs a monitor_device configured — set one via \
                         settings or {\"monitor_device\":\"name\"} IPC".to_string()
                } else if self.engine.preview_deck(is_a) {
                    format!("Preview: deck {} on monitor bus", if is_a { "A" } else { "B" })
                } else {
                    format!("Deck {} has no track loaded — nothing to preview",
                        if is_a { "A" } else { "B" })
                }
            }
            "stop_preview" => {
                let deck = tool.string("deck").unwrap_or("b");
                let is_a = deck.eq_ignore_ascii_case("a");
                self.engine.stop_deck_preview(is_a);
                format!("Preview stopped: deck {}", if is_a { "A" } else { "B" })
            }
            "play_deck" => {
                let deck = tool.string("deck").unwrap_or("b");
                let is_a = deck.eq_ignore_ascii_case("a");
                if self.engine.play_deck(is_a) {
                    format!("Deck {} → live on main output", if is_a { "A" } else { "B" })
                } else {
                    format!("Deck {} has no track loaded — load_to_deck first",
                        if is_a { "A" } else { "B" })
                }
            }
            "loop_release" => {
                let is_a = tool.string("deck").map(|s| s.eq_ignore_ascii_case("a")).unwrap_or(true);
                self.engine.loop_release(is_a);
                format!("Loop {} released", Self::deck_label(is_a))
            }
            "cue" => {
                let is_a = tool.string("deck").map(|s| s.eq_ignore_ascii_case("a")).unwrap_or(true);
                let slot = tool.int("slot").unwrap_or(1) as usize;
                let slot = slot.saturating_sub(1).min(3);
                let action = tool.string("action").unwrap_or("jump");
                match action {
                    "set" => { self.engine.cue_set(is_a, slot); format!("Cue {} set ({})", slot+1, Self::deck_label(is_a)) }
                    "clear" => { self.engine.cue_clear(is_a, slot); format!("Cue {} cleared ({})", slot+1, Self::deck_label(is_a)) }
                    _ => { self.engine.cue_jump(is_a, slot); format!("Cue {} jump ({})", slot+1, Self::deck_label(is_a)) }
                }
            }
            _ => format!("Unknown tool: {}", tool.name)
        }
    }

    pub(crate) fn toggle_claude_dj(&mut self) {
        if let Some(ref dj_arc) = self.claude_dj {
            let now_on = !self.config.claude_dj_enabled;
            self.config.claude_dj_enabled = now_on;
            self.toast.show(if now_on { "Claude DJ: ON" } else { "Claude DJ: OFF" }, 1.0);
            let dj_arc = Arc::clone(dj_arc);
            tokio::spawn(async move {
                let mut dj = dj_arc.lock().await;
                if now_on { dj.enable(); } else { dj.disable(); }
            });
            self.engine.apply_claude_dj_settings(
                &self.config.claude_dj,
                self.config.claude_dj_enabled,
            );
            self.config.save();
            if now_on {
                self.trigger_dj("You just took over as DJ. Check the queue — if it's low, browse and find a good track to queue.");
            }
        } else {
            self.toast.show("No Claude API key (~/.mixr/claude_key)", 2.0);
        }
    }

    /// Trigger a Claude DJ call asynchronously.
    pub(crate) fn trigger_dj(&mut self, reason: &str) {
        let Some(ref dj) = self.claude_dj else { return; };
        let dj = Arc::clone(dj);
        // Engine-state decides mode: active crossfade → Performance
        // (slim prompt + tools, token-optimized for the phase-watch
        // loop). Otherwise Prep (full curation context).
        let mode = if self.cached_info.state == crate::audio::engine::EngineState::Crossfading {
            crate::claude::dj::CallMode::Performance
        } else {
            crate::claude::dj::CallMode::Prep
        };
        let context = match mode {
            crate::claude::dj::CallMode::Performance => self.build_performance_context(),
            crate::claude::dj::CallMode::Prep => self.build_dj_context(),
        };
        let reason = reason.to_string();
        let tx = self.action_tx.clone();

        tokio::spawn(async move {
            let mut dj = dj.lock().await;
            dj.set_call_mode(mode);
            match dj.trigger(&context, &reason).await {
                Ok(tools) if !tools.is_empty() => { tx.send(AppAction::DjToolCalls(tools)).ok(); }
                Ok(_) => {} // No tools = Claude is done
                Err(e) => { tx.send(AppAction::Toast(format!("DJ error: {e}"))).ok(); }
            }
        });
    }

    /// Continue DJ conversation after tool results.
    pub(crate) fn continue_dj(&mut self, results: Vec<(String, String)>) {
        let Some(ref dj) = self.claude_dj else { return; };
        let dj = Arc::clone(dj);
        let tx = self.action_tx.clone();

        tokio::spawn(async move {
            let mut dj = dj.lock().await;
            match dj.continue_with_results(results).await {
                Ok(tools) if !tools.is_empty() => { tx.send(AppAction::DjToolCalls(tools)).ok(); }
                Ok(_) => {} // Done
                Err(e) => { tx.send(AppAction::Toast(format!("DJ error: {e}"))).ok(); }
            }
        });
    }

    pub(crate) fn build_performance_context(&self) -> String {
        let info = &self.cached_info;
        let deck = |t: &Option<std::sync::Arc<crate::beatport::models::BeatportTrack>>,
                    bpm: &Option<f64>, pos: f64| -> String {
            match t {
                Some(t) => format!("{} ({:.0} BPM, {}, {:.0}s in)",
                    t.full_title(),
                    bpm.unwrap_or(0.0),
                    t.key.as_deref().unwrap_or("?"),
                    pos,
                ),
                None => "empty".into(),
            }
        };
        let deck_a = deck(&info.deck_a_track, &info.deck_a_bpm, info.deck_a_time);
        let deck_b = deck(&info.deck_b_track, &info.deck_b_bpm, info.deck_b_time);
        let align = self.engine.read_alignment();
        format!(
            "deckA: {deck_a}\n\
             deckB: {deck_b}\n\
             playing_is_a: {}\n\
             crossfader_pos: {:+.2} (progress {:.0}%)\n\
             phase_ms: {:+.1} (fraction: {:.3})\n\
             beat_in_bar: A={} B={} (match={})\n\
             bar_in_phrase: A={} B={} (delta={})\n\
             mixer: deckA EQ L{:+.0}/M{:+.0}/H{:+.0} filter{:+.2} | \
                    deckB EQ L{:+.0}/M{:+.0}/H{:+.0} filter{:+.2}",
            info.playing_is_a,
            info.crossfader_pos, info.crossfade_progress * 100.0,
            align.beat_phase_ms, align.beat_phase_fraction,
            align.beat_in_bar_a, align.beat_in_bar_b,
            align.beat_in_bar_a == align.beat_in_bar_b,
            align.bar_in_phrase_a, align.bar_in_phrase_b,
            (align.bar_in_phrase_b as i32 - align.bar_in_phrase_a as i32).rem_euclid(16),
            info.deck_a_eq_low_db, info.deck_a_eq_mid_db, info.deck_a_eq_high_db, info.deck_a_filter_pos,
            info.deck_b_eq_low_db, info.deck_b_eq_mid_db, info.deck_b_eq_high_db, info.deck_b_filter_pos,
        )
    }

    pub(crate) fn build_dj_context(&self) -> String {
        let info = &self.cached_info;
        let playing = info.playing_track.as_ref()
            .map(|t| format!("{} - {} ({:.0} BPM, {})", t.artist_name(), t.full_title(), t.bpm.unwrap_or(0.0), t.key.as_deref().unwrap_or("?")))
            .unwrap_or("None".into());
        let incoming = info.incoming_track.as_ref()
            .map(|t| format!("{} - {} ({:.0} BPM, {})", t.artist_name(), t.full_title(), t.bpm.unwrap_or(0.0), t.key.as_deref().unwrap_or("?")))
            .unwrap_or("None".into());
        let remaining = info.playing_duration - info.playing_time;
        let queue_tracks: Vec<String> = info.queue.iter().take(5).map(|e| {
            format!("{} - {} ({:.0} BPM)", e.track.artist_name(), e.track.full_title(), e.track.bpm.unwrap_or(0.0))
        }).collect();

        // Recent mix scores — gives the model feedback on how well prior mixes
        // landed so it can lean into what's working or fix what isn't.
        let recent_scores: Vec<String> = info.history.iter().rev().take(5)
            .filter_map(|h| h.mix_score.map(|s| format!("{}={s}", h.track.full_title())))
            .collect();

        // Current phrase context for the playing deck — useful for "is now a
        // good moment to mix?" decisions.
        let phrase_now = if let Some(p) = info.playing_analysis.as_ref()
            .and_then(|a| a.phrases.iter().rev().find(|ph| ph.start_time <= info.playing_time))
        {
            format!("{:?} (started {:.0}s ago, energy {:.2})",
                p.phrase_type, info.playing_time - p.start_time, p.energy)
        } else { "unknown".into() };

        // Mixer state — Claude can read EQ/filter/fader to decide whether to
        // adjust them mid-mix.
        let mixer_state = format!(
            "Transition next: {}  Crossfader: {:+.2}\n\
             Deck A (left): EQ L{:+.0}/M{:+.0}/H{:+.0}  Filter {:+.2}  Fader {:.2}{}\n\
             Deck B (right): EQ L{:+.0}/M{:+.0}/H{:+.0}  Filter {:+.2}  Fader {:.2}{}",
            info.transition_type_name, info.crossfader_pos,
            info.deck_a_eq_low_db, info.deck_a_eq_mid_db, info.deck_a_eq_high_db,
            info.deck_a_filter_pos, info.channel_fader_a,
            if info.deck_a_loop_active { " LOOP" } else { "" },
            info.deck_b_eq_low_db, info.deck_b_eq_mid_db, info.deck_b_eq_high_db,
            info.deck_b_filter_pos, info.channel_fader_b,
            if info.deck_b_loop_active { " LOOP" } else { "" },
        );

        // Session arc hint: rough phase labels for energy shaping.
        let arc = match info.session_time_min {
            0..=20 => "warmup",
            21..=45 => "building",
            46..=90 => "peak",
            _ => "wind-down",
        };

        format!(
            "STATE: {:?}\n\
             Session: {} min ({arc})\n\
             Playing: {playing}\n\
             Incoming: {incoming}\n\
             Time remaining: {:.0}s\n\
             Phase: {:+.1}ms\n\
             Current phrase: {phrase_now}\n\
             Recent mix scores (newest first): {}\n\
             Queue ({} tracks): {}\n\
             Browse: {}\n\
             Mixer:\n{mixer_state}",
            info.state, info.session_time_min, remaining, info.phase_offset_ms,
            if recent_scores.is_empty() { "(none)".into() } else { recent_scores.join(", ") },
            info.queue.len(),
            if queue_tracks.is_empty() { "empty".into() } else { queue_tracks.join(", ") },
            self.breadcrumb()
        )
    }
}
