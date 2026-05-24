//! Key, mouse, and browse-enter handlers extracted from app.rs.
//! Pure code move — no behavior changes.

use crossterm::event::{KeyCode, KeyEvent};

use crate::audio::engine::QueueEntry;
use crate::beatport::catalog::{self, BrowseScreen, MenuAction};
use super::app::{App, AppAction, ClickAction, DashFocus, ViewMode};
use super::dashboard::CtrlSection;

impl App {
    fn simulate_key(&mut self, code: crossterm::event::KeyCode) {
        self.handle_key(crossterm::event::KeyEvent::new(
            code, crossterm::event::KeyModifiers::empty(),
        ));
    }

    /// Handle a terminal mouse event. macOS Terminal, iTerm2, and most
    /// other modern terminals emit xterm-style mouse codes that
    /// crossterm decodes for us. The intent here is "mouse should be
    /// useful for DJing" — scroll wheel acts like arrow keys (works
    /// in any view), and clicks on dashboard widgets dispatch to the
    /// matching control. Drag isn't supported yet — landing target is
    /// to allow the crossfader to be dragged across, but that requires
    /// hit-testing the rendered controller box and tracking the
    /// per-frame widget bounds, which is its own commit.
    pub(crate) fn handle_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind;
        match m.kind {
            // Scroll wheel = arrow keys in any view. Reuses every
            // existing list/menu nav handler for free.
            MouseEventKind::ScrollUp    => self.simulate_key(KeyCode::Up),
            MouseEventKind::ScrollDown  => self.simulate_key(KeyCode::Down),
            MouseEventKind::ScrollLeft  => self.simulate_key(KeyCode::Left),
            MouseEventKind::ScrollRight => self.simulate_key(KeyCode::Right),
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                self.handle_mouse_click(m.column, m.row, m.modifiers);
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Right) => {
                self.handle_mouse_right_click(m.column, m.row);
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                self.handle_mouse_drag(m.column, m.row);
            }
            _ => {}
        }
    }

    /// Right-click: if the clicked target has a `midi_action`, open
    /// the MIDI map flow for it. Otherwise no-op (right-click on
    /// non-bindable elements is silently ignored).
    pub(crate) fn handle_mouse_right_click(&mut self, col: u16, row: u16) {
        let action = self.click_targets.iter().rev()
            .find(|t| t.contains(col, row))
            .and_then(|t| t.midi_action.clone());
        if let Some(action) = action {
            self.toast.show(
                &format!("Move a controller to bind: {} (Esc to cancel)", action.label()),
                4.0,
            );
            // Clear the listener's last_event so we don't capture a
            // stale event from before the right-click.
            if let Some(midi) = &self.midi {
                if let Ok(mut state) = midi.lock() {
                    state.last_event = None;
                }
            }
            self.pending_midi_map = Some(super::app::PendingMidiMap {
                action,
                captured: None,
            });
        }
    }

    /// Continuous drag at (col, row). Fires for real `Drag(Left)`
    /// mouse events and for the IPC `drag` command. Only the drag-
    /// aware click targets (crossfader, vertical strips) react;
    /// other targets (buttons, hot cues, list rows) ignore drag so
    /// they don't fire on every sub-pixel move.
    pub(crate) fn handle_mouse_drag(&mut self, col: u16, row: u16) {
        let hit = self.click_targets.iter().rev()
            .find(|t| t.contains(col, row))
            .map(|t| t.action.clone());
        match hit {
            Some(ClickAction::SetCrossfaderRange { x_min, x_max }) if x_max > x_min => {
                let span = (x_max - x_min) as f32;
                let rel = (col.saturating_sub(x_min) as f32 / span).clamp(0.0, 1.0);
                let pos = rel * 2.0 - 1.0;
                self.engine.set_crossfader(pos);
            }
            Some(ClickAction::SetVerticalRange { control, y_min, y_max }) => {
                self.apply_vertical_drag(control, row, y_min, y_max, false);
            }
            _ => {}
        }
    }

    /// Map an absolute (col, row) terminal click to an action. Uses
    /// the widget bounds captured during the last render
    /// (`self.click_targets`) so we don't have to re-derive layout
    /// here. Falls through silently if the click missed everything.
    pub(crate) fn handle_mouse_click(&mut self, col: u16, row: u16, mods: crossterm::event::KeyModifiers) {
        // Walk targets in reverse so later (overlay) hits beat earlier
        // (background) ones.
        let hit = self.click_targets.iter().rev()
            .find(|t| t.contains(col, row))
            .map(|t| t.action.clone());
        if let Some(action) = hit {
            // Shift-click on a hot cue dot = SET that cue (vs jump).
            // Mirrors the keyboard convention: '1'-'4' jumps, '!@#$'
            // sets. Terminals pass modifiers on mouse events so we
            // can hijack here without a new ClickAction variant.
            let shift = mods.contains(crossterm::event::KeyModifiers::SHIFT);
            if shift
                && let ClickAction::SimulateKey(crossterm::event::KeyCode::Char(d)) = action
                    && let Some(set_char) = match d {
                        '1' => Some('!'), '2' => Some('@'), '3' => Some('#'), '4' => Some('$'),
                        _ => None,
                    } {
                        self.handle_key(crossterm::event::KeyEvent::new(
                            crossterm::event::KeyCode::Char(set_char),
                            crossterm::event::KeyModifiers::SHIFT,
                        ));
                        return;
                    }
            self.dispatch_click_action(action, col, row);
        }
    }

    fn dispatch_click_action(&mut self, action: ClickAction, click_col: u16, click_row: u16) {
        match action {
            ClickAction::SetCrossfaderRange { x_min, x_max } => {
                if x_max <= x_min { return; }
                let span = (x_max - x_min) as f32;
                let rel = (click_col.saturating_sub(x_min) as f32 / span).clamp(0.0, 1.0);
                let pos = rel * 2.0 - 1.0;
                self.engine.set_crossfader(pos);
                self.toast.show(&format!("Crossfader → {pos:+.2}"), 0.6);
            }
            ClickAction::SetVerticalRange { control, y_min, y_max } => {
                self.apply_vertical_drag(control, click_row, y_min, y_max, true);
            }
            ClickAction::CycleJumpBars => {
                let n = match self.config.jump_bars {
                    4 => 8, 8 => 16, 16 => 32, _ => 4,
                };
                self.config.jump_bars = n;
                self.config.save();
                self.engine.set_jump_bars(n);
                self.toast.show(&format!("Jump {n} bars"), 1.0);
            }
            ClickAction::LoopEngageDeck { is_a, beats } => {
                self.engine.loop_engage_deck(is_a, beats);
                self.toast.show(&format!("Deck {} loop {beats:.0} beats",
                    if is_a {"A"} else {"B"}), 1.0);
            }
            ClickAction::LoopOffDeck { is_a } => {
                self.engine.loop_disengage_deck(is_a);
                self.toast.show(&format!("Deck {} loop off",
                    if is_a {"A"} else {"B"}), 0.8);
            }
            ClickAction::SimulateKey(code) => self.simulate_key(code),
            ClickAction::FocusDashSection(section) => {
                self.dash_section = section;
                self.dash_focus = DashFocus::Controller;
            }
            ClickAction::DashBrowseSelect(idx) => {
                // Second click on the same row drills in (Enter on
                // dashboard with Browse focus = handle_browse_enter,
                // which loads/queues for track lists or pushes a
                // sub-screen for menu items).
                let was_focused = self.dash_focus == DashFocus::Browse
                    && self.dash_browse_sel == idx;
                self.dash_focus = DashFocus::Browse;
                self.dash_browse_sel = idx;
                if was_focused {
                    self.selected = idx;
                    self.handle_browse_enter();
                    self.dash_browse_sel = 0;
                }
            }
            ClickAction::WaveformZoom(is_a) => {
                if self.waveform_zoom == Some(is_a) {
                    self.waveform_zoom = None;
                } else {
                    self.waveform_zoom = Some(is_a);
                }
            }
            ClickAction::SetSelected(idx) => {
                // Click on the already-selected row = activate (Enter).
                // Mirrors common GUI behavior: first click selects,
                // second click on the same row opens. For Settings
                // this cycles the option; for Browse it drills in;
                // for Queue/History it triggers whatever Enter does.
                if self.selected == idx {
                    self.simulate_key(crossterm::event::KeyCode::Enter);
                } else {
                    self.selected = idx;
                    self.scroll_offset = self.scroll_offset.min(idx);
                }
            }
        }
    }

    /// Apply a click or drag on a vertical range strip. `y_min` is the
    /// top row (max value), `y_max` is the row *after* the last strip
    /// row (min value). `is_click` shows a toast + focuses the matching
    /// dashboard section; drag updates silently so the user isn't
    /// spammed with toasts while dragging.
    fn apply_vertical_drag(
        &mut self,
        control: crate::tui::app::RangeControl,
        row: u16,
        y_min: u16,
        y_max: u16,
        is_click: bool,
    ) {
        use crate::tui::app::RangeControl;
        use crate::tui::dashboard::CtrlSection;
        if y_max <= y_min { return; }
        let span = (y_max - y_min) as f32;
        // Top of strip = max value, bottom = min value. Clamp in case
        // the drag wandered off the target edge.
        let rel = (row.saturating_sub(y_min) as f32 / span).clamp(0.0, 1.0);
        let norm = 1.0 - rel; // 0..1, top = 1

        match control {
            RangeControl::TempoA | RangeControl::TempoB => {
                // Map norm to rate = 1.0 ± tempo_range%. norm 0.5 = unity.
                let range_pct = self.config.tempo_range as f64 / 100.0;
                let rate = 1.0 + range_pct * (norm as f64 * 2.0 - 1.0);
                let is_a = matches!(control, RangeControl::TempoA);
                self.engine.set_deck_rate(is_a, rate);
                if is_click {
                    self.dash_section = if is_a { CtrlSection::TempoA } else { CtrlSection::TempoB };
                    self.dash_focus = DashFocus::Controller;
                    self.toast.show(&format!("Tempo {}: {:.3}×", if is_a { "A" } else { "B" }, rate), 0.6);
                }
            }
            RangeControl::VolumeA | RangeControl::VolumeB => {
                let is_a = matches!(control, RangeControl::VolumeA);
                self.engine.set_channel_fader(is_a, norm);
                if is_click {
                    self.dash_section = if is_a { CtrlSection::VolumeA } else { CtrlSection::VolumeB };
                    self.dash_focus = DashFocus::Controller;
                    self.toast.show(&format!("Volume {}: {:.2}", if is_a { "A" } else { "B" }, norm), 0.6);
                }
            }
        }
    }

    /// Rate a specific history entry. Toggle semantics:
    /// - Unrated + key → rate.
    /// - Already-rated + same key → unrate (removes from DJ memory
    ///   via the saved timestamp). Acts as undo for inadvertent presses.
    /// - Already-rated + opposite key → flip (removes the old rating,
    ///   saves the new). Lets the user change their mind without
    ///   leaving stale entries in memory.
    pub(crate) fn rate_history_entry(&mut self, idx: usize, good: bool) {
        let Some(slot) = self.mix_entries.get_mut(idx) else {
            self.toast.show("No mix data for this entry", 1.5);
            return;
        };
        let Some(mix) = slot.as_mut() else {
            self.toast.show("Opening track — no mix to rate", 1.5);
            return;
        };
        let Some(ref dj) = self.claude_dj else {
            self.toast.show("Claude DJ not initialized", 1.5);
            return;
        };
        let Ok(mut dj) = dj.try_lock() else { return };

        // Three branches: undo, flip, or fresh rate.
        match mix.rated {
            Some(prev) if prev == good => {
                // Same key pressed again → undo.
                if let Some(ts) = mix.rated_at {
                    dj.unrate(ts, prev);
                }
                mix.rated = None;
                mix.rated_at = None;
                self.toast.show(
                    if good { "Removed 👍 — entry unrated" }
                    else    { "Removed 👎 — entry unrated" },
                    1.5,
                );
            }
            Some(prev) => {
                // Opposite key → flip. Yank the old, save the new.
                if let Some(ts) = mix.rated_at {
                    dj.unrate(ts, prev);
                }
                let mut entry = mix.entry.clone();
                let now = chrono::Utc::now().timestamp();
                entry.rated_at = Some(now);
                if good { dj.rate_good(entry); } else { dj.rate_bad(entry); }
                mix.rated = Some(good);
                mix.rated_at = Some(now);
                self.toast.show(
                    if good { "Mix re-rated: 👎 → 👍" }
                    else    { "Mix re-rated: 👍 → 👎" },
                    1.5,
                );
            }
            None => {
                // Fresh rating.
                let mut entry = mix.entry.clone();
                let now = chrono::Utc::now().timestamp();
                entry.rated_at = Some(now);
                if good { dj.rate_good(entry); } else { dj.rate_bad(entry); }
                mix.rated = Some(good);
                mix.rated_at = Some(now);
                self.toast.show(
                    if good { "Mix rated: 👍 saved to DJ memory" }
                    else    { "Mix rated: 👎 saved to DJ memory" },
                    1.5,
                );
            }
        }
    }

    /// Toggle favorite on a specific track. Shared by dashboard's
    /// `f` handler and the deck-picker overlay.
    pub(crate) fn toggle_favorite_track(&mut self, track: crate::beatport::models::BeatportTrack) {
        let added = self.favorites.toggle(&track);
        let name = format!("{} - {}", track.artist_name(), track.full_title());
        let msg = if added { format!("★ {name}") } else { format!("Unfavorited: {name}") };
        self.toast.show(&msg, 1.5);
    }

    /// Parse and dispatch a `:` command-prompt entry. Accepts either:
    ///   - Shorthand: `<key> <value>` or just `<key>` (e.g. `skip`,
    ///     `queue 12345`, `tx echoout`, `crossfade 32`, `vol 0.8`)
    ///   - Raw JSON: starts with `{` — passed through verbatim
    ///
    /// Writes to ~/.mixr/command in the existing IPC envelope so it
    /// flows through the standard command parser. Toasts the result.
    pub(crate) fn submit_command_prompt(&mut self, line: &str) {
        let path = dirs::home_dir().unwrap_or_default().join(".mixr/command");
        let json = if line.trim_start().starts_with('{') {
            line.to_string()
        } else {
            crate::ipc::shorthand_to_json(line)
        };
        // Validate before writing — bad JSON should toast, not silently
        // create a malformed command file.
        if serde_json::from_str::<serde_json::Value>(&json).is_err() {
            self.toast.show(&format!("Bad command: {line}"), 2.0);
            return;
        }
        if let Err(e) = std::fs::write(&path, &json) {
            self.toast.show(&format!("Command write failed: {e}"), 2.0);
            return;
        }
        self.toast.show(&format!(":{line}"), 1.0);
    }

    /// Add a track to the user's Beatport cart. Spawns the API call;
    /// reports success/error via toast. Drives revenue back to
    /// Beatport — the cart is the path to actually buying tracks.
    pub(crate) fn add_track_to_cart(&mut self, track: crate::beatport::models::BeatportTrack) {
        let Some(api) = self.api.clone() else {
            self.toast.show("Not authenticated", 2.0);
            return;
        };
        let tx = self.action_tx.clone();
        let name = format!("{} - {}", track.artist_name(), track.full_title());
        tokio::spawn(async move {
            let mut api = api.lock().await;
            match api.add_to_cart(track.id).await {
                Ok(()) => { tx.send(crate::tui::app::AppAction::Toast(format!("🛒 Added to cart: {name}"))).ok(); }
                Err(e) => { tx.send(crate::tui::app::AppAction::Toast(format!("Cart add failed: {e}"))).ok(); }
            }
        });
    }

    /// Toggle favorite on the track loaded into deck A or B. No-op if
    /// the deck is empty (the dashboard `f` path filters that case).
    pub(crate) fn toggle_favorite_deck(&mut self, is_a: bool) {
        let track = if is_a {
            self.cached_info.deck_a_track.as_deref().cloned()
        } else {
            self.cached_info.deck_b_track.as_deref().cloned()
        };
        if let Some(track) = track {
            self.toggle_favorite_track(track);
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        // Right-click → MIDI map flow. Highest priority — when a
        // mapping is pending, the next keystroke is interpreted in
        // the context of that flow. Y/Enter saves (after capture),
        // Esc cancels at any time, other keys absorbed.
        if self.pending_midi_map.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let pm = self.pending_midi_map.take().unwrap();
                    if let Some(event) = pm.captured.clone() {
                        if let Some(midi) = &self.midi {
                            if let Ok(mut state) = midi.lock() {
                                state.map.bind(event.clone(), pm.action.clone());
                                if let Err(e) = state.map.save() {
                                    self.toast.show(&format!("Save failed: {e}"), 3.0);
                                } else {
                                    self.toast.show(
                                        &format!("Bound: {} → {}", event.label(), pm.action.label()),
                                        2.0,
                                    );
                                }
                            }
                        }
                    } else {
                        // Y pressed before any event was captured — keep waiting.
                        self.pending_midi_map = Some(pm);
                        self.toast.show("Move a controller first, then press Y", 1.5);
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.pending_midi_map = None;
                    self.toast.show("Mapping cancelled", 0.6);
                }
                _ => { /* absorb */ }
            }
            return;
        }

        // Y/N confirmation takes priority over everything. A pending
        // destructive action absorbs the next keystroke: Y commits,
        // N/Esc cancels, anything else is ignored (so e.g. accidentally
        // typing on the keyboard during a confirm doesn't fire it).
        if let Some(action) = self.pending_confirm {
            match super::app::route_confirm_key(key.code) {
                super::app::ConfirmDecision::Commit => {
                    self.pending_confirm = None;
                    match action {
                        super::app::ConfirmAction::ResetAllMixerControls => {
                            self.reset_all_mixer_controls();
                        }
                        super::app::ConfirmAction::ClearQueue => {
                            self.engine.clear_queue();
                            self.toast.show("Queue cleared", 1.0);
                        }
                    }
                }
                super::app::ConfirmDecision::Cancel => {
                    self.pending_confirm = None;
                    self.toast.show("Cancelled", 0.6);
                }
                super::app::ConfirmDecision::Ignore => { /* keep waiting */ }
            }
            return;
        }

        // Command-prompt overlay takes priority when open. Capture all
        // keystrokes so the user can type a full command line without
        // any other key binding stealing characters.
        if self.command_prompt.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.command_prompt = None;
                }
                KeyCode::Enter => {
                    if let Some(cmd) = self.command_prompt.take() {
                        let trimmed = cmd.trim();
                        if !trimmed.is_empty() {
                            self.submit_command_prompt(trimmed);
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(buf) = self.command_prompt.as_mut() {
                        buf.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(buf) = self.command_prompt.as_mut() {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return;
        }

        // Resume prompt takes priority — Y applies the snapshot, N/Esc
        // declines and deletes the session file so future launches
        // don't keep re-asking.
        if self.pending_resume_prompt {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(snap) = self.pending_resume_snapshot.take() {
                        self.apply_resume(snap);
                    } else {
                        self.pending_resume_prompt = false;
                    }
                    return;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    crate::session::delete();
                    self.pending_resume_snapshot = None;
                    self.pending_resume_prompt = false;
                    self.toast.show("Session discarded", 1.5);
                    return;
                }
                _ => {}
            }
        }

        // Dashboard mode
        if matches!(self.view_mode, ViewMode::Dashboard) {
            // Favorite-deck picker: triggered when user presses `f` on
            // dashboard with both decks loaded and mini-browse not
            // focused. Capture a/b/Esc and dispatch.
            if self.dash_fav_picker {
                match key.code {
                    KeyCode::Esc => { self.dash_fav_picker = false; }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        self.dash_fav_picker = false;
                        self.toggle_favorite_deck(true);
                    }
                    KeyCode::Char('b') | KeyCode::Char('B') => {
                        self.dash_fav_picker = false;
                        self.toggle_favorite_deck(false);
                    }
                    _ => {}
                }
                return;
            }

            // DJ ask mode — capture typing
            if self.dj_asking {
                match key.code {
                    KeyCode::Esc => { self.dj_asking = false; self.dj_ask_buffer.clear(); }
                    KeyCode::Backspace => { self.dj_ask_buffer.pop(); }
                    KeyCode::Enter if !self.dj_ask_buffer.is_empty() => {
                        let prompt = self.dj_ask_buffer.clone();
                        self.dj_asking = false;
                        self.dj_ask_buffer.clear();
                        // Set DJ direction and trigger
                        if let Some(ref dj) = self.claude_dj
                            && let Ok(mut dj) = dj.try_lock() { dj.set_prompt(prompt.clone()); }
                        self.trigger_dj(&format!("User says: {prompt}"));
                        self.toast.show(&format!("Asked DJ: {prompt}"), 2.0);
                    }
                    KeyCode::Char(c) => { self.dj_ask_buffer.push(c); }
                    _ => {}
                }
                return;
            }

            match key.code {
                KeyCode::Tab => { self.dash_focus = self.dash_focus.next(); }
                KeyCode::Char('d') => { self.view_mode = ViewMode::Browse; }
                KeyCode::Esc => {
                    if self.waveform_zoom.is_some() {
                        self.waveform_zoom = None;
                    } else if self.dash_focus == DashFocus::Browse && self.screen_stack.len() > 1 {
                        // pop_screen restores selected / scroll / dash_browse_sel
                        // so the cursor lands back on the row the user
                        // drilled in from.
                        self.pop_screen();
                    } else {
                        self.view_mode = ViewMode::Browse;
                    }
                }
                KeyCode::Char('w') => {
                    self.waveform_mode = self.waveform_mode.next();
                    self.toast.show(&format!("Waveform: {}", self.waveform_mode.label()), 1.0);
                }
                // Cycle dashboard layout: Full → Panel(Queue) →
                // Panel(History) → Panel(Browse) → Panel(Log) → Full.
                // Panel keeps the controller pinned and shows just one
                // secondary section below — short enough to coexist
                // with another app on the same screen.
                KeyCode::Char('v') => {
                    use super::dashboard::{DashLayout, PanelSection};
                    let (next_layout, next_section, label) = match (self.dash_layout, self.dash_panel_section) {
                        (DashLayout::Full, _) => (DashLayout::Panel, PanelSection::Queue, "Panel: queue"),
                        (DashLayout::Panel, PanelSection::Queue)   => (DashLayout::Panel, PanelSection::History, "Panel: history"),
                        (DashLayout::Panel, PanelSection::History) => (DashLayout::Panel, PanelSection::Browse,  "Panel: browse"),
                        (DashLayout::Panel, PanelSection::Browse)  => (DashLayout::Panel, PanelSection::Log,     "Panel: log"),
                        (DashLayout::Panel, PanelSection::Log)     => (DashLayout::Full,  PanelSection::Queue,   "Full view"),
                    };
                    self.dash_layout = next_layout;
                    self.dash_panel_section = next_section;
                    self.config.dash_layout = next_layout;
                    self.config.dash_panel_section = next_section;
                    self.config.save();
                    self.toast.show(label, 1.0);
                }
                KeyCode::Char('?') => { self.dash_help = !self.dash_help; }
                // Manual panic / train-wreck bail. Forces the in-progress
                // crossfade onto EchoOut to salvage a bad mix. No-op
                // when not currently crossfading.
                KeyCode::Char('B') => {
                    if self.engine.bail_crossfade() {
                        self.toast.show("⚠ Bailed to EchoOut", 2.0);
                    } else {
                        self.toast.show("Nothing to bail (not crossfading)", 1.0);
                    }
                }
                // Load Next — only on mini-browse panel. State-aware:
                // - Idle:    starts this track
                // - Playing without incoming: loads as incoming
                // - Incoming loaded waiting: replaces it (displaced
                //   goes back to queue front+1 to preserve user choice)
                // - Crossfading: queues at front (plays after this mix)
                KeyCode::Char('L') => {
                    if self.dash_focus == DashFocus::Browse
                        && let Some(track) = self.current_screen().track_at(self.dash_browse_sel).cloned() {
                            let name = format!("{} - {}", track.artist_name(), track.full_title());
                            let outcome = self.engine.play_next(track);
                            let msg = match outcome {
                                crate::audio::engine::PlayNextOutcome::StartedFresh =>
                                    format!("Playing next: {name}"),
                                crate::audio::engine::PlayNextOutcome::LoadedAsIncoming =>
                                    format!("Loaded as incoming: {name}"),
                                crate::audio::engine::PlayNextOutcome::ReplacedIncoming =>
                                    format!("Replaced incoming with {name} (prev moved to queue)"),
                                crate::audio::engine::PlayNextOutcome::QueuedAtFront =>
                                    format!("Queued next: {name}"),
                            };
                            self.toast.show(&msg, 2.0);
                        }
                }
                // Favorite — focus-aware. On the mini-browse panel,
                // favorite the highlighted track. Otherwise act on the
                // decks: one loaded → favorite it; both loaded → open
                // a small picker overlay (a/b/Esc); none → do nothing.
                // Add to Beatport cart. Same focus model as favorite —
                // mini-browse panel acts on the highlighted track.
                // Shift+7 (&) avoids the $ collision with hot cue 4 set.
                KeyCode::Char('&') => {
                    if self.dash_focus == DashFocus::Browse
                        && let Some(track) = self.current_screen().track_at(self.dash_browse_sel) {
                            self.add_track_to_cart(track.clone());
                        }
                }
                KeyCode::Char('f') | KeyCode::Char('*') => {
                    if self.dash_focus == DashFocus::Browse {
                        if let Some(track) = self.current_screen().track_at(self.dash_browse_sel) {
                            self.toggle_favorite_track(track.clone());
                        }
                    } else {
                        let a_loaded = self.cached_info.deck_a_track.is_some();
                        let b_loaded = self.cached_info.deck_b_track.is_some();
                        match (a_loaded, b_loaded) {
                            (true, true) => { self.dash_fav_picker = true; }
                            (true, false) => self.toggle_favorite_deck(true),
                            (false, true) => self.toggle_favorite_deck(false),
                            (false, false) => self.toast.show("Nothing to favorite", 1.0),
                        }
                    }
                }
                // Rate the most-recent mix for training memory. Only
                // active on the dashboard — elsewhere `+` opens the
                // playlist picker. `=` is accepted as an unshifted
                // alternative so users don't have to hold shift.
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.handle_ipc_command(crate::ipc::IpcCommand::RateMix(true));
                }
                KeyCode::Char('-') | KeyCode::Char('_') => {
                    self.handle_ipc_command(crate::ipc::IpcCommand::RateMix(false));
                }
                KeyCode::Up => {
                    if self.dash_focus == DashFocus::Controller {
                        self.dash_section = self.dash_section.prev();
                    } else if self.dash_focus == DashFocus::Browse && self.dash_browse_sel > 0 {
                        self.dash_browse_sel -= 1;
                    } else if self.dash_focus == DashFocus::Log {
                        // Scroll back through log history. Capped at
                        // 1000 to avoid runaway when the log is huge.
                        self.log_scroll_offset = (self.log_scroll_offset + 1).min(1000);
                    }
                }
                KeyCode::Down => {
                    if self.dash_focus == DashFocus::Controller {
                        self.dash_section = self.dash_section.next();
                    } else if self.dash_focus == DashFocus::Browse {
                        let count = self.current_screen().item_count();
                        if self.dash_browse_sel + 1 < count.min(8) {
                            self.dash_browse_sel += 1;
                        }
                    } else if self.dash_focus == DashFocus::Log && self.log_scroll_offset > 0 {
                        self.log_scroll_offset -= 1;
                    }
                }
                KeyCode::Enter | KeyCode::Right => {
                    if self.dash_focus == DashFocus::Controller {
                        self.handle_deck_control(1);
                    } else if self.dash_focus == DashFocus::Browse {
                        self.selected = self.dash_browse_sel;
                        self.handle_browse_enter();
                        self.dash_browse_sel = 0;
                    }
                }
                KeyCode::Left => {
                    if self.dash_focus == DashFocus::Controller {
                        self.handle_deck_control(-1);
                    } else if self.dash_focus == DashFocus::Browse && self.screen_stack.len() > 1 {
                        self.pop_screen();
                    }
                }
                KeyCode::Char('/') => {
                    // On dashboard: ask Claude DJ if enabled, otherwise search
                    let dj_on = self.claude_dj.is_some() && self.config.claude_dj_enabled;
                    if dj_on {
                        self.dj_asking = true;
                        self.dj_ask_buffer.clear();
                        self.toast.show("Ask Claude DJ...", 1.0);
                    } else {
                        self.view_mode = ViewMode::Search;
                        self.search_query.clear();
                        self.search_results.clear();
                        self.selected = 0;
                    }
                }
                KeyCode::Char('A') => {
                    if let Some(data) = self.engine.alignment_peaks() {
                        self.toast.show("AI analyzing mix alignment...", 2.0);
                        let tx = self.action_tx.clone();
                        tokio::spawn(async move {
                            match crate::audio::ai_beat::analyze_mix_alignment(
                                &data.playing_peaks, &data.incoming_peaks,
                                data.playing_bpm, data.incoming_bpm,
                            ).await {
                                Ok(a) => {
                                    tx.send(AppAction::AlignmentResult {
                                        nudge_ms: a.nudge_ms,
                                        is_aligned: a.is_aligned,
                                        rate_correction: a.rate_correction,
                                        details: a.details,
                                    }).ok();
                                }
                                Err(e) => { tx.send(AppAction::Toast(format!("Alignment error: {e}"))).ok(); }
                            }
                        });
                    } else {
                        self.toast.show("Not crossfading", 1.0);
                    }
                }
                KeyCode::Char('b') => {
                    // If focused on browse, switch to full browser. Otherwise focus browse.
                    if self.dash_focus == DashFocus::Browse {
                        self.view_mode = ViewMode::Browse;
                        self.selected = 0;
                    } else {
                        self.dash_focus = DashFocus::Browse;
                    }
                }
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.view_mode = ViewMode::Mixer;
                }
                _ => {
                    // Fall through to general key handler for keys
                    // not dashboard-specific (y=clipboard, w=waveform,
                    // +/-=rate mix, etc.)
                }
            }
        }

        // Local filter mode
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter_text.clear();
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    self.filter_text.pop();
                    self.selected = 0;
                }
                KeyCode::Enter => {
                    // Keep filter, stop typing mode
                    self.filtering = false;
                }
                KeyCode::Char(c) => {
                    self.filter_text.push(c);
                    self.selected = 0;
                }
                KeyCode::Up if self.selected > 0 => { self.selected -= 1; }
                KeyCode::Down => {
                    let count = self.filtered_item_count();
                    if self.selected + 1 < count { self.selected += 1; }
                }
                _ => {}
            }
            return;
        }

        // Search input mode
        if matches!(self.view_mode, ViewMode::Help) {
            match key.code {
                KeyCode::Esc => {
                    // Two-step Esc: clear filter first, then close on
                    // a second press. Lets users back out of a search
                    // without losing the help screen.
                    if self.help_filter.is_empty() {
                        self.view_mode = ViewMode::Dashboard;
                    } else {
                        self.help_filter.clear();
                        self.help_scroll = 0;
                    }
                }
                KeyCode::Up => { self.help_scroll = self.help_scroll.saturating_sub(1); }
                KeyCode::Down => { self.help_scroll = self.help_scroll.saturating_add(1); }
                KeyCode::PageUp => { self.help_scroll = self.help_scroll.saturating_sub(10); }
                KeyCode::PageDown => { self.help_scroll = self.help_scroll.saturating_add(10); }
                KeyCode::Home => { self.help_scroll = 0; }
                KeyCode::End => { self.help_scroll = u16::MAX; }
                KeyCode::Backspace => {
                    self.help_filter.pop();
                    self.help_scroll = 0;
                }
                KeyCode::Char(c) => {
                    self.help_filter.push(c);
                    self.help_scroll = 0;
                }
                _ => {}
            }
            return;
        }

        // History view: intercept rating keys before falling through
        // to the global `+` (playlist picker). Lets users rate any
        // past mix in the list, not just the most-recent one.
        if matches!(self.view_mode, ViewMode::History) {
            match key.code {
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.rate_history_entry(self.selected, true);
                    return;
                }
                KeyCode::Char('-') | KeyCode::Char('_') => {
                    self.rate_history_entry(self.selected, false);
                    return;
                }
                _ => {}
            }
        }

        if matches!(self.view_mode, ViewMode::Search) {
            match key.code {
                KeyCode::Esc => {
                    self.view_mode = ViewMode::Browse;
                    self.search_query.clear();
                    self.search_results.clear();
                    self.selected = 0;
                }
                KeyCode::Backspace => { self.search_query.pop(); }
                KeyCode::Enter if !self.search_query.is_empty() => {
                    self.trigger_search();
                }
                KeyCode::Up if self.selected > 0 => { self.selected -= 1; }
                KeyCode::Down if self.selected + 1 < self.search_results.len() => {
                    self.selected += 1;
                }
                KeyCode::Char(c) => { self.search_query.push(c); }
                _ => {}
            }
            return;
        }

        // Playlist name input mode
        if matches!(self.view_mode, ViewMode::PlaylistNameInput) {
            if let Some(ref mut picker) = self.playlist_picker {
                match key.code {
                    KeyCode::Char(c) => { picker.new_name.push(c); }
                    KeyCode::Backspace => { picker.new_name.pop(); }
                    KeyCode::Enter if !picker.new_name.is_empty() => {
                        let name = picker.new_name.clone();
                        let track_id = picker.track_id;
                        self.create_and_add_to_playlist(name, track_id);
                    }
                    KeyCode::Esc => {
                        self.view_mode = ViewMode::PlaylistPicker;
                    }
                    _ => {}
                }
            }
            return;
        }

        // Playlist picker mode
        if matches!(self.view_mode, ViewMode::PlaylistPicker) {
            if let Some(ref mut picker) = self.playlist_picker {
                let count = picker.playlists.len() + 1; // +1 for "Create New"
                match key.code {
                    KeyCode::Up if picker.selected > 0 => { picker.selected -= 1; }
                    KeyCode::Down if picker.selected + 1 < count => { picker.selected += 1; }
                    KeyCode::Enter => {
                        if picker.selected == 0 {
                            // Create new playlist
                            let now = chrono::Local::now();
                            picker.new_name = format!("mixr {}", now.format("%H:%M %m/%d"));
                            self.view_mode = ViewMode::PlaylistNameInput;
                        } else {
                            let playlist_idx = picker.selected - 1;
                            if let Some(playlist) = picker.playlists.get(playlist_idx) {
                                let pid = playlist.id;
                                let track_id = picker.track_id;
                                let pname = playlist.name.clone();
                                self.add_track_to_playlist(pid, track_id, &pname);
                            }
                        }
                    }
                    KeyCode::Esc => {
                        self.playlist_picker = None;
                        self.view_mode = ViewMode::Browse;
                    }
                    _ => {}
                }
            }
            return;
        }

        // Genre/Favorites picker modes
        if matches!(self.view_mode, ViewMode::GenrePicker | ViewMode::FavoritesPicker) {
            if let Some(ref mut picker) = self.genre_picker {
                let count = picker.genres.len();
                match key.code {
                    KeyCode::Up if picker.selected > 0 => { picker.selected -= 1; }
                    KeyCode::Down if picker.selected + 1 < count => { picker.selected += 1; }
                    KeyCode::Enter => {
                        if let Some(genre) = picker.genres.get(picker.selected) {
                            if matches!(self.view_mode, ViewMode::GenrePicker) {
                                self.config.default_genre = genre.name.clone();
                                self.config.save();
                                self.toast.show(&format!("Default genre: {}", genre.name), 1.0);
                                self.genre_picker = None;
                                self.view_mode = ViewMode::Settings;
                            } else {
                                // Toggle favorite
                                let name = genre.name.clone();
                                if self.config.favorite_genres.contains(&name) {
                                    self.config.favorite_genres.retain(|g| g != &name);
                                    self.toast.show(&format!("Removed: {name}"), 0.5);
                                } else {
                                    self.config.favorite_genres.push(name.clone());
                                    self.toast.show(&format!("Added: {name}"), 0.5);
                                }
                                self.config.save();
                            }
                        }
                    }
                    KeyCode::Esc => {
                        self.genre_picker = None;
                        self.view_mode = ViewMode::Settings;
                    }
                    _ => {}
                }
                // Update scroll
                if let Some(ref _picker) = self.genre_picker {
                    // handled by render
                }
            }
            return;
        }

        // Settings mode — handle separately
        if matches!(self.view_mode, ViewMode::Settings) {
            let count = super::settings::settings_row_count(&self.config);
            match key.code {
                KeyCode::Up if self.selected > 0 => { self.selected -= 1; }
                KeyCode::Down if self.selected + 1 < count => { self.selected += 1; }
                KeyCode::Enter | KeyCode::Right => {
                    if let Some(row) = super::settings::settings_row_at(&self.config, self.selected) {
                        // Reset-all sentinel — Enter wipes config back to default.
                        if row.key == super::settings::RESET_ALL_KEY {
                            self.config = crate::config::AppConfig::default();
                            return;
                        }
                        if !row.options.is_empty() {
                            let next = (row.current_idx + 1) % row.options.len();
                            let key_str = row.key;
                            if self.apply_and_sync_setting(key_str, next) { return; }
                        }
                    }
                }
                KeyCode::Left => {
                    if let Some(row) = super::settings::settings_row_at(&self.config, self.selected)
                        && row.key != super::settings::RESET_ALL_KEY
                        && !row.options.is_empty() {
                            let prev = if row.current_idx == 0 { row.options.len() - 1 } else { row.current_idx - 1 };
                            let key_str = row.key;
                            if self.apply_and_sync_setting(key_str, prev) { return; }
                        }
                }
                // `r` — reset focused row to its `AppConfig::default()` value.
                KeyCode::Char('r') => {
                    if let Some(row) = super::settings::settings_row_at(&self.config, self.selected)
                        && row.key != super::settings::RESET_ALL_KEY
                        && row.current_idx != row.default_idx {
                            let key_str = row.key;
                            let def = row.default_idx;
                            self.apply_and_sync_setting(key_str, def);
                        }
                }
                // `R` (shift+r) — reset every row to default (matches the
                // explicit Reset sentinel at the bottom).
                KeyCode::Char('R') => {
                    self.config = crate::config::AppConfig::default();
                }
                // Esc / `,` / `d` all close Settings and return to
                // Dashboard. Going to full Browse on close was a bug
                // — user usually opens Settings from Dashboard and
                // expects to land back there. Matches the Mixer
                // overlay's pattern (Esc → Dashboard).
                KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('d') => {
                    self.view_mode = ViewMode::Dashboard;
                    self.selected = 0;
                }
                // Also honor the standard navigation hotkeys so users
                // don't get trapped in Settings.
                KeyCode::Char('b') => { self.view_mode = ViewMode::Browse; self.selected = 0; }
                KeyCode::Char('q') => { self.view_mode = ViewMode::Queue; self.selected = 0; }
                KeyCode::Char('h') => { self.view_mode = ViewMode::History; self.selected = 0; }
                _ => {}
            }
            return;
        }

        // Mixer mode — virtual mixer control overlay
        if matches!(self.view_mode, ViewMode::MidiLearn) {
            // Pull the latest captured event from the listener every
            // keystroke (and on each render via the renderer reading
            // self.midi_learn_captured). Picking it up on key events
            // means the user sees their last touch even if no render
            // happened between touch + key.
            if let Some(midi) = &self.midi
                && let Ok(state) = midi.lock()
                    && let Some((ev, _val)) = state.last_event.clone() {
                        self.midi_learn_captured = Some(ev);
                    }
            match key.code {
                KeyCode::Esc => {
                    self.view_mode = ViewMode::Dashboard;
                }
                KeyCode::Up => {
                    self.midi_learn_action_sel = self.midi_learn_action_sel
                        .saturating_sub(1);
                }
                KeyCode::Down => {
                    let max = super::midi_learn::action_count().saturating_sub(1);
                    self.midi_learn_action_sel = (self.midi_learn_action_sel + 1).min(max);
                }
                KeyCode::Enter => {
                    if let (Some(event), Some(action)) = (
                        self.midi_learn_captured.clone(),
                        super::midi_learn::action_at(self.midi_learn_action_sel),
                    ) {
                        if let Some(midi) = &self.midi
                            && let Ok(mut state) = midi.lock() {
                                let label = format!("{} → {}", event.label(), action.label());
                                state.map.bind(event, action);
                                drop(state);
                                self.toast.show(&format!("Bound {label}"), 1.5);
                                self.midi_learn_captured = None;
                            }
                    } else {
                        self.toast.show("Touch a control first", 1.0);
                    }
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    if let Some(event) = self.midi_learn_captured.clone()
                        && let Some(midi) = &self.midi
                            && let Ok(mut state) = midi.lock() {
                                state.map.unbind(&event);
                                drop(state);
                                self.toast.show(&format!("Unbound {}", event.label()), 1.0);
                            }
                }
                _ => {}
            }
            return;
        }

        if matches!(self.view_mode, ViewMode::Mixer) {
            match key.code {
                KeyCode::Esc | KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.view_mode = ViewMode::Dashboard;
                }
                KeyCode::Tab => {
                    self.mixer_deck_is_a = !self.mixer_deck_is_a;
                    self.toast.show(&format!("Deck {}", if self.mixer_deck_is_a {"A"} else {"B"}), 0.5);
                }
                KeyCode::Up => { self.mixer_row = self.mixer_row.prev(); }
                KeyCode::Down => { self.mixer_row = self.mixer_row.next(); }
                KeyCode::Left => { self.adjust_mixer_row(-1.0); }
                KeyCode::Right => { self.adjust_mixer_row(1.0); }
                KeyCode::Char('r') => { self.reset_mixer_row(); }
                KeyCode::Char('0') => {
                    self.pending_confirm = Some(super::app::ConfirmAction::ResetAllMixerControls);
                    self.toast.show("Reset ALL mixer controls? Y/N", 5.0);
                }
                _ => {}
            }
            return;
        }

        if matches!(self.view_mode, ViewMode::TransitionRules) {
            if let Some(ref mut ed) = self.rules_editor {
                use super::rules_editor::KeyResult;
                match super::rules_editor::handle_key(ed, key) {
                    KeyResult::Continue | KeyResult::ContinueCancelled => {}
                    KeyResult::CloseSaved => {
                        if ed.dirty {
                            self.engine.set_rules_config(ed.config.clone());
                            self.toast.show("Rules saved", 1.0);
                        }
                        self.rules_editor = None;
                        self.view_mode = ViewMode::Settings;
                    }
                }
            }
            return;
        }

        let item_count = match self.view_mode {
            ViewMode::Browse => self.current_screen().item_count(),
            ViewMode::Queue => self.cached_info.queue.len(),
            ViewMode::History => self.cached_info.history.len(),
            _ => 0,
        };

        match key.code {
            KeyCode::Up if self.selected > 0 => { self.selected -= 1; }
            KeyCode::Down if self.selected + 1 < item_count => { self.selected += 1; }
            KeyCode::Home => { self.selected = 0; }
            KeyCode::End => { self.selected = item_count.saturating_sub(1); }
            KeyCode::PageUp => { self.selected = self.selected.saturating_sub(10); }
            KeyCode::PageDown => { self.selected = (self.selected + 10).min(item_count.saturating_sub(1)); }

            KeyCode::Esc => {
                match self.view_mode {
                    ViewMode::Browse => {
                        if self.pop_screen() {
                            // Skip empty placeholder screens (from
                            // canonical nav paths). Each pop restores
                            // its own cursor state.
                            while self.screen_stack.len() > 1 && self.current_screen().item_count() == 0 {
                                self.pop_screen();
                            }
                        }
                    }
                    _ => {
                        self.view_mode = ViewMode::Browse;
                        self.selected = 0;
                        self.scroll_offset = 0;
                    }
                }
            }

            KeyCode::Enter => {
                if matches!(self.view_mode, ViewMode::Browse) {
                    self.handle_browse_enter();
                }
            }

            KeyCode::Right => {
                if matches!(self.view_mode, ViewMode::Browse) {
                    if matches!(self.current_screen(), BrowseScreen::TrackList { .. }) {
                        // Cycle columns: -2 (whole row) → -1 (title) → 0 (artist) → 2 (label) → 3 (genre) → 4 (date)
                        let max_col = 4;
                        if self.selected_column < max_col {
                            self.selected_column += 1;
                            // Skip remixer in compact view
                            if self.config.compact_view && self.selected_column == 1 {
                                self.selected_column = 2;
                            }
                        }
                    } else {
                        self.handle_browse_enter();
                    }
                }
            }

            // Left never navigates back — use Esc for that.
            KeyCode::Left
                if matches!(self.view_mode, ViewMode::Browse)
                    && matches!(self.current_screen(), BrowseScreen::TrackList { .. })
                    && self.selected_column > -2 =>
            {
                self.selected_column -= 1;
                if self.config.compact_view && self.selected_column == 1 {
                    self.selected_column = 0;
                }
            }

            // Space: preview track (4 bars from first beat with metronome)
            KeyCode::Char(' ') => {
                if self.engine.is_previewing() {
                    self.engine.stop_preview();
                    self.toast.show("Preview stopped", 1.0);
                } else if matches!(self.view_mode, ViewMode::Browse)
                    && let Some(track) = self.current_screen().track_at(self.selected).cloned() {
                        self.download_for_preview(track);
                    }
            }

            // Enter on track list: queue track
            // (Enter is already handled above for browse navigation)

            // Queue all (skips duplicates already queued or loaded on a deck).
            KeyCode::Char('a') => {
                if let Some(tracks) = self.current_screen().tracks() {
                    let total = tracks.len();
                    let mut added = 0;
                    #[allow(clippy::unnecessary_to_owned)]
                    for track in tracks.to_vec() {
                        if self.engine.enqueue(QueueEntry::from(track)) { added += 1; }
                    }
                    let msg = match (added, total - added) {
                        (0, _) => "All tracks already queued".to_string(),
                        (a, 0) => format!("Queued {a} tracks"),
                        (a, skipped) => format!("Queued {a}, skipped {skipped} duplicates"),
                    };
                    self.toast.show(&msg, 1.5);
                }
            }

            KeyCode::Char('[') => {
                if let Some((shift, pos)) = self.engine.nudge(-1) {
                    self.toast.show(&format!("{shift:+.0}ms  beat@{:.0}ms", pos * 1000.0), 1.0);
                } else {
                    self.toast.show("Nudge ◀", 0.5);
                }
            }
            KeyCode::Char(']') => {
                if let Some((shift, pos)) = self.engine.nudge(1) {
                    self.toast.show(&format!("{shift:+.0}ms  beat@{:.0}ms", pos * 1000.0), 1.0);
                } else {
                    self.toast.show("Nudge ▶", 0.5);
                }
            }

            // Grid shift — phase adjustment that bypasses the rate
            // controller. Targets incoming during crossfade, playing
            // otherwise. Unlike nudge, this is persistent; each tap
            // moves first_beat_time by 2ms.
            // Loop hotkeys. `i` toggles a 4-beat loop on the playing
            // deck (most common length). Shift+number gives different
            // beat lengths: I/O double/halve, J/K/L for 1/4/8 beats.
            KeyCode::Char('i') => {
                self.engine.loop_toggle_playing(4.0);
                self.toast.show("Loop 4 beats", 1.0);
            }
            KeyCode::Char('I') => {
                self.engine.loop_toggle_playing(8.0);
                self.toast.show("Loop 8 beats", 1.0);
            }
            KeyCode::Char('u') => {
                self.engine.loop_toggle_playing(1.0);
                self.toast.show("Loop 1 beat", 1.0);
            }
            KeyCode::Char('U') => {
                self.engine.loop_toggle_playing(2.0);
                self.toast.show("Loop 2 beats", 1.0);
            }
            KeyCode::Char('O') => {
                self.engine.loop_toggle_playing(16.0);
                self.toast.show("Loop 16 beats", 1.0);
            }

            KeyCode::Char(';') => {
                self.engine.shift_grid_active(-2.0);
                self.toast.show("Grid ◀ -2ms", 0.5);
            }
            KeyCode::Char('\'') => {
                self.engine.shift_grid_active(2.0);
                self.toast.show("Grid ▶ +2ms", 0.5);
            }
            // Whole-beat grid shifts on `(` and `)` — use when the 1s
            // don't land together (grid origin wrong by a beat) and
            // the 2ms fine-shift can't cover the gap. Moved off `:`
            // and `"` to free `:` for the command-mode prompt
            // (vim-style entry).
            KeyCode::Char('(') => {
                self.engine.shift_grid_active_beats(-1);
                self.toast.show("Grid ◀ -1 beat", 0.8);
            }
            KeyCode::Char(')') => {
                self.engine.shift_grid_active_beats(1);
                self.toast.show("Grid ▶ +1 beat", 0.8);
            }

            KeyCode::Char('<') => {
                let bars = self.config.jump_bars as i32;
                self.engine.jump(-bars);
                self.toast.show(&format!("Jump -{bars} bars"), 1.0);
            }
            KeyCode::Char('>') => {
                let bars = self.config.jump_bars as i32;
                self.engine.jump(bars);
                self.toast.show(&format!("Jump +{bars} bars"), 1.0);
            }
            KeyCode::Char(':') => {
                // Open the command-prompt overlay. Subsequent keys are
                // captured by the prompt (see top of handle_key).
                self.command_prompt = Some(String::new());
            }
            KeyCode::Char('p') => { self.engine.pause(); self.toast.show("Play/Pause", 1.0); }
            KeyCode::Char('n') => { self.engine.skip(); self.toast.show("Skipped", 1.0); }
            KeyCode::Char('r') => {
                // Favorites are metadata-only on main — there's no
                // local audio to sync. Tracks are re-fetched (in
                // memory) from Beatport when played.
                self.toast.show("Favorites are metadata-only on main — no sync needed", 2.5);
            }

            KeyCode::Char('t') => { self.engine.teleport(&self.config); self.toast.show("Teleport to mix point", 1.0); }
            KeyCode::Char('T') => {
                use crate::audio::engine::RewindOutcome;
                match self.engine.request_rewind() {
                    Some(RewindOutcome::InPlace) => {
                        self.toast.show("Rewinding last mix", 2.0);
                    }
                    Some(RewindOutcome::NeedLoad(track)) => {
                        let name = format!("{} - {}", track.artist_name(), track.full_title());
                        self.download_and_play(std::sync::Arc::new(track), true);
                        self.toast.show(&format!("Rewinding: {name}"), 2.0);
                    }
                    Some(RewindOutcome::Blocked) => {
                        self.toast.show("Wait for the current mix to finish before rewinding", 2.5);
                    }
                    None => {
                        self.toast.show("No mix to rewind", 1.5);
                    }
                }
            }
            KeyCode::Char('G') => {
                // Cycle analyzer engine + re-analyze the playing deck
                // in-place. Used to A/B the built-in onset detector
                // vs stratum-dsp on a bad mix: press `G`, hear the
                // re-gridded playback, press `G` again to flip back.
                use crate::config::AnalyzerEngine;
                self.config.analyzer_engine = match self.config.analyzer_engine {
                    AnalyzerEngine::Builtin => AnalyzerEngine::Stratum,
                    AnalyzerEngine::Stratum => AnalyzerEngine::Builtin,
                };
                self.config.save();
                let label = match self.config.analyzer_engine {
                    AnalyzerEngine::Builtin => "built-in",
                    AnalyzerEngine::Stratum => "stratum",
                };
                // Flag when the feature isn't compiled — the config
                // flip is honored but resolve_bpm silently falls back
                // to built-in, which would otherwise look like "G does
                // nothing on re-press."
                let fallback_note =
                    if matches!(self.config.analyzer_engine, AnalyzerEngine::Stratum)
                        && !cfg!(feature = "stratum") {
                        " (not compiled — using built-in)"
                    } else { "" };
                match self.engine.reanalyze_playing(self.config.analyzer_engine) {
                    Some(bpm) => self.toast.show(
                        &format!("Engine: {label}{fallback_note} — re-gridded @ {bpm:.1} BPM"), 3.0),
                    None => self.toast.show(
                        &format!("Engine: {label}{fallback_note} (no track loaded)"), 2.0),
                }
            }
            KeyCode::Char('m') => { self.engine.mix_now(); self.toast.show("Mix now", 1.0); }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                let unfollow = key.code == KeyCode::Char('W');
                if matches!(self.view_mode, ViewMode::Browse) {
                    match self.current_screen() {
                        BrowseScreen::ArtistList { artists, .. } => {
                            if let Some(artist) = artists.get(self.selected) {
                                let aid = artist.id;
                                let name = artist.name.clone();
                                if let Some(api) = self.api.clone() {
                                    let tx = self.action_tx.clone();
                                    tokio::spawn(async move {
                                        let mut api = api.lock().await;
                                        let result = if unfollow {
                                            api.unfollow_artist(aid).await
                                        } else {
                                            api.follow_artist(aid).await
                                        };
                                        let verb = if unfollow { "Unfollowed" } else { "Followed" };
                                        match result {
                                            Ok(()) => { tx.send(AppAction::Toast(format!("{verb}: {name}"))).ok(); }
                                            Err(e) => { tx.send(AppAction::Toast(format!("Error: {e}"))).ok(); }
                                        }
                                    });
                                }
                            }
                        }
                        BrowseScreen::LabelList { labels, .. } => {
                            if let Some(label) = labels.get(self.selected) {
                                let lid = label.id;
                                let name = label.name.clone();
                                if let Some(api) = self.api.clone() {
                                    let tx = self.action_tx.clone();
                                    tokio::spawn(async move {
                                        let mut api = api.lock().await;
                                        let result = if unfollow {
                                            api.unfollow_label(lid).await
                                        } else {
                                            api.follow_label(lid).await
                                        };
                                        let verb = if unfollow { "Unfollowed" } else { "Followed" };
                                        match result {
                                            Ok(()) => { tx.send(AppAction::Toast(format!("{verb}: {name}"))).ok(); }
                                            Err(e) => { tx.send(AppAction::Toast(format!("Error: {e}"))).ok(); }
                                        }
                                    });
                                }
                            }
                        }
                        _ => {
                            self.waveform_mode = self.waveform_mode.next();
                            self.toast.show(&format!("Waveform: {}", self.waveform_mode.label()), 1.0);
                        }
                    }
                } else {
                    // Not in browse — toggle waveform mode
                    self.waveform_mode = self.waveform_mode.next();
                    self.toast.show(&format!("Waveform: {}", self.waveform_mode.label()), 1.0);
                }
            }

            KeyCode::Char('F') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) | key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) => {
                // Ctrl+F or Shift+F: start local filter
                if matches!(self.view_mode, ViewMode::Browse) {
                    self.filtering = true;
                    self.filter_text.clear();
                    self.selected = 0;
                    self.toast.show("Filter: type to filter", 1.5);
                }
            }

            KeyCode::Char('y') => {
                // Copy screen dump to clipboard
                let path = dirs::home_dir().unwrap_or_default().join(".mixr/screen.txt");
                if let Ok(content) = std::fs::read_to_string(&path) {
                    match std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn() {
                        Ok(mut child) => {
                            if let Some(ref mut stdin) = child.stdin {
                                use std::io::Write;
                                stdin.write_all(content.as_bytes()).ok();
                            }
                            child.wait().ok();
                            self.toast.show("Screen copied to clipboard", 1.0);
                        }
                        Err(_) => self.toast.show("Failed to copy", 1.0),
                    }
                }
            }
            KeyCode::Char('e') => {
                let count = self.engine.export_history();
                if count > 0 { self.toast.show(&format!("History exported: {count} tracks"), 2.0); }
                else { self.toast.show("No history to export", 1.0); }
            }
            KeyCode::Char('x') => { self.engine.smart_shuffle(); self.toast.show("Queue shuffled (BPM+key)", 1.5); }
            KeyCode::Char('X') => {
                let n = self.engine.queue.len();
                if n == 0 {
                    self.toast.show("Queue already empty", 0.6);
                } else {
                    self.pending_confirm = Some(super::app::ConfirmAction::ClearQueue);
                    self.toast.show(&format!("Clear queue ({n} track{})? Y/N", if n == 1 { "" } else { "s" }), 5.0);
                }
            }
            KeyCode::Char('S') => {
                let on = self.engine.toggle_split_cue();
                self.toast.show(if on { "Split cue: ON (L=deck A, R=deck B)" } else { "Split cue: OFF" }, 2.0);
            }
            KeyCode::Char('M') => {
                let on = self.engine.toggle_metronome();
                self.toast.show(if on { "Metronome: ON" } else { "Metronome: OFF" }, 1.0);
            }
            KeyCode::Char('{') => {
                // Queue grab/drop — works globally, switches to queue view
                if !matches!(self.view_mode, ViewMode::Queue) {
                    self.view_mode = ViewMode::Queue;
                    self.selected = 0;
                }
                if self.queue_grab_index.is_some() {
                    self.queue_grab_index = None;
                    self.toast.show("Dropped", 0.5);
                } else {
                    self.queue_grab_index = Some(self.selected);
                    self.toast.show("Grabbed — move with ↑↓, press } to drop", 2.0);
                }
            }
            KeyCode::Char('}') => {
                if let Some(from) = self.queue_grab_index.take() {
                    let to = self.selected;
                    self.engine.move_queue_item(from, to);
                    self.toast.show("Moved", 0.5);
                }
            }
            KeyCode::Char('q') => { self.view_mode = ViewMode::Queue; self.selected = 0; self.queue_grab_index = None; }
            KeyCode::Char('h') => { self.view_mode = ViewMode::History; self.selected = 0; }
            KeyCode::Char('d') => { self.view_mode = ViewMode::Dashboard; }
            KeyCode::Char('/') | KeyCode::Char('s') => {
                self.view_mode = ViewMode::Search;
                self.search_query.clear();
                self.search_results.clear();
                self.selected = 0;
            }
            KeyCode::Char('?') => {
                self.view_mode = ViewMode::Help;
                self.help_filter.clear();
                self.help_scroll = 0;
            }
            // Hot cues on the currently-playing deck. 1..4 jump, !@#$ set.
            KeyCode::Char(c @ '1'..='4') => {
                let slot = (c as u8 - b'1') as usize;
                let is_a = self.cached_info.playing_is_a;
                self.engine.cue_jump(is_a, slot);
                self.toast.show(&format!("Cue {} jump ({})", slot + 1, Self::deck_label(is_a)), 0.5);
            }
            KeyCode::Char(c @ ('!' | '@' | '#' | '$')) => {
                let slot = match c { '!' => 0, '@' => 1, '#' => 2, '$' => 3, _ => 0 };
                let is_a = self.cached_info.playing_is_a;
                self.engine.cue_set(is_a, slot);
                self.toast.show(&format!("Cue {} set ({})", slot + 1, Self::deck_label(is_a)), 0.8);
            }
            // Virtual Mixer overlay is dashboard-only. See the dashboard
            // handler above (`if matches!(ViewMode::Dashboard)`) for the
            // real z/Z binding; out here it'd collide with per-mode
            // shortcuts and confuse users browsing.
            KeyCode::Char(',') => { self.view_mode = ViewMode::Settings; self.selected = 0; }
            KeyCode::Char('K') => {
                self.view_mode = ViewMode::MidiLearn;
                self.midi_learn_action_sel = 0;
                self.midi_learn_captured = None;
            }
            KeyCode::Char('b') => { self.view_mode = ViewMode::Browse; self.selected = 0; }
            // Dashboard's `v` is dashboard-layout cycle (handled in the
            // dashboard arm above); skip the compact-view toggle here so
            // the two don't double-fire.
            KeyCode::Char('v') if !matches!(self.view_mode, ViewMode::Dashboard) => {
                self.config.compact_view = !self.config.compact_view;
                self.config.save();
                self.toast.show(if self.config.compact_view { "Compact view" } else { "Full view" }, 1.0);
            }
            KeyCode::Char('L') => {
                // Load more (pagination) — works on track, chart, release lists
                if matches!(self.view_mode, ViewMode::Browse)
                    && let Some(ref action) = self.last_load_action.clone() {
                        self.load_more(action);
                    }
            }

            KeyCode::Char('+') => {
                // Add track to playlist
                if let Some(track) = self.current_screen().track_at(self.selected) {
                    let track_id = track.id;
                    self.open_playlist_picker(track_id);
                }
            }

            KeyCode::Char('f') | KeyCode::Char('*') => {
                // Toggle favorite on current track
                let track = match self.current_screen() {
                    BrowseScreen::TrackList { tracks, .. } => tracks.get(self.selected).cloned(),
                    _ => None,
                };
                if let Some(track) = track {
                    let added = self.favorites.toggle(&track);
                    let name = format!("{} - {}", track.artist_name(), track.full_title());
                    let msg = if added { format!("★ {name}") } else { format!("Unfavorited: {name}") };
                    self.toast.show(&msg, 1.5);
                }
            }

            KeyCode::Char('&') => {
                // Add current track to Beatport cart for later purchase.
                let track = match self.current_screen() {
                    BrowseScreen::TrackList { tracks, .. } => tracks.get(self.selected).cloned(),
                    _ => None,
                };
                if let Some(track) = track {
                    self.add_track_to_cart(track);
                }
            }

            KeyCode::Char('o') => {
                // Open in browser — column-aware
                self.open_in_browser();
            }

            KeyCode::Char('c') => {
                // Open the full Claude DJ screen (log scrollback + state).
                self.view_mode = ViewMode::ClaudeDj;
                self.scroll_offset = 0;
            }
            KeyCode::Char('C') => { self.toggle_claude_dj(); }
            _ => {}
        }

        // Update scroll
        let visible = 20usize; // approximate
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible {
            self.scroll_offset = self.selected - visible + 1;
        }
    }

    pub(crate) fn handle_browse_enter(&mut self) {
        // Map selection through filter if active
        if !self.filter_text.is_empty() {
            let indices = self.filtered_indices();
            if let Some(&real_idx) = indices.get(self.selected) {
                self.selected = real_idx;
            }
            self.filter_text.clear();
            self.filtering = false;
        }
        let screen = self.current_screen().clone();
        match &screen {
            BrowseScreen::Menu { items, .. } => {
                if let Some(item) = items.get(self.selected) {
                    let action = item.action.clone();
                    self.execute_menu_action(action);
                }
            }
            BrowseScreen::TrackList { tracks, .. } => {
                if let Some(track) = tracks.get(self.selected).cloned() {
                    match self.selected_column {
                        -2 => {
                            // Whole row: queue track
                            let name = format!("{} - {}", track.artist_name(), track.full_title());
                            let added = self.engine.enqueue(crate::audio::engine::QueueEntry::from(track));
                            let msg = if added {
                                format!("Queued: {name}")
                            } else {
                                format!("Already queued: {name}")
                            };
                            self.toast.show(&msg, 1.5);
                        }
                        -1 => {
                            // Title column: open release tracks
                            if let Some(rid) = track.release_id {
                                self.execute_menu_action(MenuAction::LoadReleaseTracks(rid));
                            }
                        }
                        0 => {
                            // Artist column
                            if track.artists.len() == 1 {
                                let a = &track.artists[0];
                                self.navigate_to_artist(a.id, &a.name);
                            } else if track.artists.len() > 1 {
                                let artists: Vec<crate::beatport::models::BeatportArtist> = track.artists.iter()
                                    .map(|a| crate::beatport::models::BeatportArtist { id: a.id, name: a.name.clone() })
                                    .collect();
                                self.push_screen(BrowseScreen::ArtistList { title: "Artists".into(), artists });
                            }
                        }
                        1 => {
                            // Remixer column
                            if track.remixers.len() == 1 {
                                let r = &track.remixers[0];
                                self.navigate_to_artist(r.id, &r.name);
                            } else if track.remixers.len() > 1 {
                                let artists: Vec<crate::beatport::models::BeatportArtist> = track.remixers.iter()
                                    .map(|r| crate::beatport::models::BeatportArtist { id: r.id, name: r.name.clone() })
                                    .collect();
                                self.push_screen(BrowseScreen::ArtistList { title: "Remixers".into(), artists });
                            }
                        }
                        2 => {
                            // Label column
                            if let (Some(lid), Some(lname)) = (track.label_id, track.label_name.as_deref()) {
                                self.navigate_to_label(lid, lname);
                            }
                        }
                        3 => {
                            // Genre column
                            if let (Some(gid), Some(gname)) = (track.genre_id, track.genre_name.as_deref()) {
                                self.navigate_to_genre(gid, gname);
                            }
                        }
                        4 => {
                            // Date column — show tracks from that year
                            if let Some(ref date) = track.release_date
                                && date.len() >= 4 {
                                    let year = &date[..4];
                                    let range = format!("{year}-01-01:{year}-12-31");
                                    self.execute_menu_action(MenuAction::LoadDecadeTracks(range, track.genre_id));
                                }
                        }
                        _ => {}
                    }
                    self.selected_column = -2;
                }
            }
            BrowseScreen::GenreList { genres, .. } => {
                if let Some(genre) = genres.get(self.selected).cloned() {
                    let screen = catalog::genre_detail_screen(genre.id, &genre.name);
                    self.push_screen(screen);
                }
            }
            BrowseScreen::ArtistList { artists, .. } => {
                if let Some(artist) = artists.get(self.selected).cloned() {
                    let screen = catalog::artist_detail_screen(artist.id, &artist.name);
                    self.push_screen(screen);
                }
            }
            BrowseScreen::LabelList { labels, .. } => {
                if let Some(label) = labels.get(self.selected).cloned() {
                    let screen = catalog::label_detail_screen(label.id, &label.name);
                    self.push_screen(screen);
                }
            }
            BrowseScreen::ChartList { charts, .. } => {
                if let Some(chart) = charts.get(self.selected).cloned() {
                    self.execute_menu_action(MenuAction::LoadChartTracks(chart.id));
                }
            }
            BrowseScreen::ReleaseList { releases, .. } => {
                if let Some(release) = releases.get(self.selected).cloned() {
                    self.execute_menu_action(MenuAction::LoadReleaseTracks(release.id));
                }
            }
        }
    }

    fn handle_deck_control(&mut self, direction: i32) {
        let jb = self.config.jump_bars as i32;
        match self.dash_section {
            CtrlSection::CueA | CtrlSection::JumpA => {
                self.engine.jump(direction * jb);
                self.toast.show(&format!("A {} {jb} bars", if direction > 0 { "▶▶" } else { "◀◀" }), 1.0);
            }
            CtrlSection::PlayA | CtrlSection::PlayB => {
                self.engine.pause();
                self.toast.show("Play/Pause", 1.0);
            }
            CtrlSection::NudgeA | CtrlSection::NudgeB => {
                self.engine.nudge(direction);
                self.toast.show(&format!("Nudge {}", if direction > 0 { "▶" } else { "◀" }), 0.5);
            }
            CtrlSection::CueB | CtrlSection::JumpB => {
                self.engine.jump(direction * jb);
                self.toast.show(&format!("B {} {jb} bars", if direction > 0 { "▶▶" } else { "◀◀" }), 1.0);
            }
            CtrlSection::TempoA => {
                let range = self.config.tempo_range as f64 / 100.0;
                let step = range / 20.0;
                let native = self.cached_info.playing_track.as_ref().and_then(|t| t.bpm).unwrap_or(128.0);
                let current = self.cached_info.playing_bpm.unwrap_or(native);
                let ratio = current / native;
                let new_ratio = (ratio + direction as f64 * step).clamp(1.0 - range, 1.0 + range);
                self.engine.set_playing_rate(new_ratio);
                self.toast.show(&format!("Tempo A: {:+.1}%", (new_ratio - 1.0) * 100.0), 0.5);
            }
            CtrlSection::TempoB => {
                let range = self.config.tempo_range as f64 / 100.0;
                let step = range / 20.0;
                let native = self.cached_info.incoming_track.as_ref().and_then(|t| t.bpm).unwrap_or(128.0);
                let current = self.cached_info.incoming_bpm.unwrap_or(native);
                let ratio = current / native;
                let new_ratio = (ratio + direction as f64 * step).clamp(1.0 - range, 1.0 + range);
                self.engine.set_incoming_rate(new_ratio);
                self.toast.show(&format!("Tempo B: {:+.1}%", (new_ratio - 1.0) * 100.0), 0.5);
            }
            CtrlSection::VolumeA => {
                let new_vol = if direction > 0 { 1.0f32 } else { 0.0 };
                self.engine.set_volume(0, new_vol);
                self.toast.show(&format!("Vol A: {:.0}%", new_vol * 100.0), 0.5);
            }
            CtrlSection::VolumeB => {
                let new_vol = if direction > 0 { 1.0f32 } else { 0.0 };
                self.engine.set_volume(1, new_vol);
                self.toast.show(&format!("Vol B: {:.0}%", new_vol * 100.0), 0.5);
            }
            CtrlSection::Crossfader => {
                let cur = self.cached_info.crossfader_pos as f64;
                let next = (cur + direction as f64 * 0.05).clamp(-1.0, 1.0);
                self.engine.set_crossfader(next as f32);
                self.toast.show(&format!("Crossfader: {:+.2}", next), 0.4);
            }
            CtrlSection::EqLowA | CtrlSection::EqLowB
            | CtrlSection::EqMidA | CtrlSection::EqMidB
            | CtrlSection::EqHighA | CtrlSection::EqHighB => {
                let is_a = matches!(self.dash_section,
                    CtrlSection::EqLowA | CtrlSection::EqMidA | CtrlSection::EqHighA);
                let (band, cur) = match self.dash_section {
                    CtrlSection::EqLowA => ("Low", self.cached_info.deck_a_eq_low_db),
                    CtrlSection::EqMidA => ("Mid", self.cached_info.deck_a_eq_mid_db),
                    CtrlSection::EqHighA => ("High", self.cached_info.deck_a_eq_high_db),
                    CtrlSection::EqLowB => ("Low", self.cached_info.deck_b_eq_low_db),
                    CtrlSection::EqMidB => ("Mid", self.cached_info.deck_b_eq_mid_db),
                    _ => ("High", self.cached_info.deck_b_eq_high_db),
                };
                let next = (cur + direction as f32).clamp(-24.0, 12.0);
                let lo = if band == "Low" { Some(next) } else { None };
                let mid = if band == "Mid" { Some(next) } else { None };
                let hi = if band == "High" { Some(next) } else { None };
                self.engine.set_eq(is_a, lo, mid, hi);
                self.toast.show(&format!("{band} {}: {next:+.0} dB", Self::deck_label(is_a)), 0.4);
            }
            CtrlSection::FilterA | CtrlSection::FilterB => {
                let is_a = matches!(self.dash_section, CtrlSection::FilterA);
                let cur = if is_a { self.cached_info.deck_a_filter_pos } else { self.cached_info.deck_b_filter_pos };
                let next = (cur + direction as f32 * 0.05).clamp(-1.0, 1.0);
                self.engine.set_filter(is_a, next);
                self.toast.show(&format!("Filter {}: {next:+.2}", Self::deck_label(is_a)), 0.4);
            }
        }
    }
}
