//! Key, mouse, and browse-enter handlers extracted from app.rs.
//! Pure code move — no behavior changes.

use crossterm::event::{KeyCode, KeyEvent};

use super::app::{App, ClickAction, DashFocus, ViewMode};
use super::dashboard::CtrlSection;
use crate::beatport::catalog::{self, BrowseScreen, MenuAction};

impl App {
    fn simulate_key(&mut self, code: crossterm::event::KeyCode) {
        self.handle_key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::empty(),
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
            MouseEventKind::ScrollUp => self.simulate_key(KeyCode::Up),
            MouseEventKind::ScrollDown => self.simulate_key(KeyCode::Down),
            MouseEventKind::ScrollLeft => self.simulate_key(KeyCode::Left),
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
        let action = self
            .click_targets
            .iter()
            .rev()
            .find(|t| t.contains(col, row))
            .and_then(|t| t.midi_action.clone());
        if let Some(action) = action {
            self.toast.show(
                &format!(
                    "Move a controller to bind: {} (Esc to cancel)",
                    action.label()
                ),
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
        let hit = self
            .click_targets
            .iter()
            .rev()
            .find(|t| t.contains(col, row))
            .map(|t| t.action.clone());
        match hit {
            Some(ClickAction::SetCrossfaderRange { x_min, x_max }) if x_max > x_min => {
                let span = (x_max - x_min) as f32;
                let rel = (col.saturating_sub(x_min) as f32 / span).clamp(0.0, 1.0);
                let pos = rel * 2.0 - 1.0;
                self.engine.set_crossfader(pos);
            }
            Some(ClickAction::SetVerticalRange {
                control,
                y_min,
                y_max,
            }) => {
                self.apply_vertical_drag(control, row, y_min, y_max, false);
            }
            _ => {}
        }
    }

    /// Map an absolute (col, row) terminal click to an action. Uses
    /// the widget bounds captured during the last render
    /// (`self.click_targets`) so we don't have to re-derive layout
    /// here. Falls through silently if the click missed everything.
    pub(crate) fn handle_mouse_click(
        &mut self,
        col: u16,
        row: u16,
        mods: crossterm::event::KeyModifiers,
    ) {
        // Walk targets in reverse so later (overlay) hits beat earlier
        // (background) ones.
        let hit = self
            .click_targets
            .iter()
            .rev()
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
                    '1' => Some('!'),
                    '2' => Some('@'),
                    '3' => Some('#'),
                    '4' => Some('$'),
                    _ => None,
                }
            {
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
                if x_max <= x_min {
                    return;
                }
                let span = (x_max - x_min) as f32;
                let rel = (click_col.saturating_sub(x_min) as f32 / span).clamp(0.0, 1.0);
                let pos = rel * 2.0 - 1.0;
                self.engine.set_crossfader(pos);
                self.toast.show(&format!("Crossfader → {pos:+.2}"), 0.6);
            }
            ClickAction::SetVerticalRange {
                control,
                y_min,
                y_max,
            } => {
                self.apply_vertical_drag(control, click_row, y_min, y_max, true);
            }
            ClickAction::CycleJumpBars => {
                let n = match self.config.jump_bars {
                    4 => 8,
                    8 => 16,
                    16 => 32,
                    _ => 4,
                };
                self.config.jump_bars = n;
                self.config.save();
                self.engine.set_jump_bars(n);
                self.toast.show(&format!("Jump {n} bars"), 1.0);
            }
            ClickAction::LoopEngageDeck { is_a, beats } => {
                self.engine.loop_engage_deck(is_a, beats);
                self.toast.show(
                    &format!(
                        "Deck {} loop {beats:.0} beats",
                        if is_a { "A" } else { "B" }
                    ),
                    1.0,
                );
            }
            ClickAction::LoopOffDeck { is_a } => {
                self.engine.loop_disengage_deck(is_a);
                self.toast.show(
                    &format!("Deck {} loop off", if is_a { "A" } else { "B" }),
                    0.8,
                );
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
                let was_focused =
                    self.dash_focus == DashFocus::Browse && self.dash_browse_sel == idx;
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
        if y_max <= y_min {
            return;
        }
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
                    self.dash_section = if is_a {
                        CtrlSection::TempoA
                    } else {
                        CtrlSection::TempoB
                    };
                    self.dash_focus = DashFocus::Controller;
                    self.toast.show(
                        &format!("Tempo {}: {:.3}×", if is_a { "A" } else { "B" }, rate),
                        0.6,
                    );
                }
            }
            RangeControl::VolumeA | RangeControl::VolumeB => {
                let is_a = matches!(control, RangeControl::VolumeA);
                self.engine.set_channel_fader(is_a, norm);
                if is_click {
                    self.dash_section = if is_a {
                        CtrlSection::VolumeA
                    } else {
                        CtrlSection::VolumeB
                    };
                    self.dash_focus = DashFocus::Controller;
                    self.toast.show(
                        &format!("Volume {}: {:.2}", if is_a { "A" } else { "B" }, norm),
                        0.6,
                    );
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
                    if good {
                        "Removed 👍 — entry unrated"
                    } else {
                        "Removed 👎 — entry unrated"
                    },
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
                if good {
                    dj.rate_good(entry);
                } else {
                    dj.rate_bad(entry);
                }
                mix.rated = Some(good);
                mix.rated_at = Some(now);
                self.toast.show(
                    if good {
                        "Mix re-rated: 👎 → 👍"
                    } else {
                        "Mix re-rated: 👍 → 👎"
                    },
                    1.5,
                );
            }
            None => {
                // Fresh rating.
                let mut entry = mix.entry.clone();
                let now = chrono::Utc::now().timestamp();
                entry.rated_at = Some(now);
                if good {
                    dj.rate_good(entry);
                } else {
                    dj.rate_bad(entry);
                }
                mix.rated = Some(good);
                mix.rated_at = Some(now);
                self.toast.show(
                    if good {
                        "Mix rated: 👍 saved to DJ memory"
                    } else {
                        "Mix rated: 👎 saved to DJ memory"
                    },
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
        let msg = if added {
            format!("★ {name}")
        } else {
            format!("Unfavorited: {name}")
        };
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
                Ok(()) => {
                    tx.send(crate::tui::app::AppAction::Toast(format!(
                        "🛒 Added to cart: {name}"
                    )))
                    .ok();
                }
                Err(e) => {
                    tx.send(crate::tui::app::AppAction::Toast(format!(
                        "Cart add failed: {e}"
                    )))
                    .ok();
                }
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
                                        &format!(
                                            "Bound: {} → {}",
                                            event.label(),
                                            pm.action.label()
                                        ),
                                        2.0,
                                    );
                                }
                            }
                        }
                    } else {
                        // Y pressed before any event was captured — keep waiting.
                        self.pending_midi_map = Some(pm);
                        self.toast
                            .show("Move a controller first, then press Y", 1.5);
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

        // Command palette overlay (Ctrl+Shift+P / F1) — greedy modal.
        // Type to filter, ↑↓ move, Enter dispatch via `command::run`,
        // Esc closes.
        if self.command_palette.is_some() {
            use crossterm::event::{KeyCode, KeyModifiers};
            let m = key.modifiers;
            match key.code {
                KeyCode::Esc => {
                    self.command_palette = None;
                }
                KeyCode::Enter => {
                    if let Some(palette) = self.command_palette.as_ref() {
                        let visible = palette.visible_indices();
                        if let Some(&idx) = visible.get(palette.selected) {
                            let id = super::command::registry().all()[idx].id;
                            self.command_palette = None;
                            super::command::run(id, self);
                        }
                    }
                }
                KeyCode::Up => {
                    if let Some(p) = self.command_palette.as_mut() {
                        p.selected = p.selected.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    if let Some(p) = self.command_palette.as_mut() {
                        let max = p.visible_indices().len().saturating_sub(1);
                        p.selected = (p.selected + 1).min(max);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(p) = self.command_palette.as_mut() {
                        if p.cursor > 0 {
                            let start = p
                                .filter
                                .char_indices()
                                .nth(p.cursor - 1)
                                .map(|(b, _)| b)
                                .unwrap_or(0);
                            let end = p
                                .filter
                                .char_indices()
                                .nth(p.cursor)
                                .map(|(b, _)| b)
                                .unwrap_or_else(|| p.filter.len());
                            p.filter.replace_range(start..end, "");
                            p.cursor -= 1;
                            // Clamp selection to the (newly larger)
                            // visible set.
                            let vlen = p.visible_indices().len();
                            if vlen == 0 {
                                p.selected = 0;
                            } else if p.selected >= vlen {
                                p.selected = vlen - 1;
                            }
                        }
                    }
                }
                KeyCode::Char(c) if !m.contains(KeyModifiers::CONTROL) => {
                    if let Some(p) = self.command_palette.as_mut() {
                        let byte = p
                            .filter
                            .char_indices()
                            .nth(p.cursor)
                            .map(|(b, _)| b)
                            .unwrap_or_else(|| p.filter.len());
                        p.filter.insert(byte, c);
                        p.cursor += 1;
                        // Reset highlight to top — the filtered set
                        // just changed.
                        p.selected = 0;
                    }
                }
                _ => {}
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

        // Command dispatch — chord → registry. Bindings migrate out of
        // the per-view-mode matches below one at a time; see
        // `docs/COMMAND_MIGRATION.md`. When a chord matches a migrated
        // `Command` whose `when` guard passes, we run it and return.
        if super::command::try_dispatch(&key, self) {
            return;
        }

        // Dashboard mode
        if matches!(self.view_mode, ViewMode::Dashboard) {
            // Favorite-deck picker: triggered when user presses `f` on
            // dashboard with both decks loaded and mini-browse not
            // focused. Capture a/b/Esc and dispatch.
            if self.dash_fav_picker {
                match key.code {
                    KeyCode::Esc => {
                        self.dash_fav_picker = false;
                    }
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
                    KeyCode::Esc => {
                        self.dj_asking = false;
                        self.dj_ask_buffer.clear();
                    }
                    KeyCode::Backspace => {
                        self.dj_ask_buffer.pop();
                    }
                    KeyCode::Enter if !self.dj_ask_buffer.is_empty() => {
                        let prompt = self.dj_ask_buffer.clone();
                        self.dj_asking = false;
                        self.dj_ask_buffer.clear();
                        // Set DJ direction and trigger
                        if let Some(ref dj) = self.claude_dj
                            && let Ok(mut dj) = dj.try_lock()
                        {
                            dj.set_prompt(prompt.clone());
                        }
                        self.trigger_dj(&format!("User says: {prompt}"));
                        self.toast.show(&format!("Asked DJ: {prompt}"), 2.0);
                    }
                    KeyCode::Char(c) => {
                        self.dj_ask_buffer.push(c);
                    }
                    _ => {}
                }
                return;
            }

            // ── Every dashboard chord is now in the registry. ──
            // The legacy `match key.code { ... }` block that lived
            // here is gone — its 20+ arms moved to `tui::command` as
            // `view.*` / `dash.*` / `engine.*` Commands with a
            // `dashboard_normal` `when` guard (or a more specific
            // variant). Chords that fall through this block are
            // intentionally global (y / w / +/- when not in
            // history-view / etc.) and handled below.
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
                KeyCode::Up if self.selected > 0 => {
                    self.selected -= 1;
                }
                KeyCode::Down => {
                    let count = self.filtered_item_count();
                    if self.selected + 1 < count {
                        self.selected += 1;
                    }
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
                KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                KeyCode::Down => {
                    self.help_scroll = self.help_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.help_scroll = self.help_scroll.saturating_add(10);
                }
                KeyCode::Home => {
                    self.help_scroll = 0;
                }
                KeyCode::End => {
                    self.help_scroll = u16::MAX;
                }
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

        // History-view `+`/`=`/`-`/`_` (rate history entry) migrated
        // to history.rate_good / history.rate_bad. The multi-value
        // keymap routes `+` based on view_mode:
        //   Dashboard  → engine.rate_mix_good
        //   History    → history.rate_good
        //   elsewhere  → playlist.add_track

        if matches!(self.view_mode, ViewMode::Search) {
            match key.code {
                KeyCode::Esc => {
                    self.view_mode = ViewMode::Browse;
                    self.search_query.clear();
                    self.search_results.clear();
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                }
                KeyCode::Enter if !self.search_query.is_empty() => {
                    self.trigger_search();
                }
                KeyCode::Up if self.selected > 0 => {
                    self.selected -= 1;
                }
                KeyCode::Down if self.selected + 1 < self.search_results.len() => {
                    self.selected += 1;
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                }
                _ => {}
            }
            return;
        }

        // Playlist name input mode
        if matches!(self.view_mode, ViewMode::PlaylistNameInput) {
            if let Some(ref mut picker) = self.playlist_picker {
                match key.code {
                    KeyCode::Char(c) => {
                        picker.new_name.push(c);
                    }
                    KeyCode::Backspace => {
                        picker.new_name.pop();
                    }
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
                    KeyCode::Up if picker.selected > 0 => {
                        picker.selected -= 1;
                    }
                    KeyCode::Down if picker.selected + 1 < count => {
                        picker.selected += 1;
                    }
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
        if matches!(
            self.view_mode,
            ViewMode::GenrePicker | ViewMode::FavoritesPicker
        ) {
            if let Some(ref mut picker) = self.genre_picker {
                let count = picker.genres.len();
                match key.code {
                    KeyCode::Up if picker.selected > 0 => {
                        picker.selected -= 1;
                    }
                    KeyCode::Down if picker.selected + 1 < count => {
                        picker.selected += 1;
                    }
                    KeyCode::Enter => {
                        if let Some(genre) = picker.genres.get(picker.selected) {
                            if matches!(self.view_mode, ViewMode::GenrePicker) {
                                self.config.default_genre = genre.name.clone();
                                self.config.save();
                                self.toast
                                    .show(&format!("Default genre: {}", genre.name), 1.0);
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
            // Text-input edit mode — when active, capture all keystrokes
            // for the in-progress value. Esc cancels, Enter saves to
            // config via apply_text_setting, any printable char appends.
            if self.settings_editing_text.is_some() {
                match key.code {
                    KeyCode::Esc => {
                        self.settings_editing_text = None;
                    }
                    KeyCode::Enter => {
                        if let Some(value) = self.settings_editing_text.take()
                            && let Some(super::settings::SettingsRowEntry::Text(row)) =
                                super::settings::settings_row_entry_at(&self.config, self.selected)
                        {
                            let key_id = row.key;
                            super::settings::apply_text_setting(&mut self.config, key_id, value);
                            // Persist to ~/.mixr/config.toml so the edit
                            // sticks across restarts.
                            self.config.save();
                            self.toast.show(&format!("Saved {}", row.label), 1.5);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(buf) = self.settings_editing_text.as_mut() {
                            buf.pop();
                        }
                    }
                    // Plain typed char only — modifier-bearing chords
                    // (Ctrl+R / Cmd+V / etc) used to push their literal
                    // chars into the text field. Hunt 2026-06-08 SEV-MED.
                    // Shift is allowed (it just gives the shifted glyph).
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                            && !key.modifiers.contains(crossterm::event::KeyModifiers::ALT)
                            && !key.modifiers.contains(crossterm::event::KeyModifiers::META) =>
                    {
                        if let Some(buf) = self.settings_editing_text.as_mut() {
                            buf.push(c);
                        }
                    }
                    _ => {}
                }
                return;
            }
            match key.code {
                KeyCode::Up if self.selected > 0 => {
                    self.selected -= 1;
                }
                KeyCode::Down if self.selected + 1 < count => {
                    self.selected += 1;
                }
                KeyCode::Home => {
                    self.selected = 0;
                }
                KeyCode::End => {
                    self.selected = count.saturating_sub(1);
                }
                KeyCode::PageUp => {
                    self.selected = self.selected.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.selected = (self.selected + 10).min(count.saturating_sub(1));
                }
                KeyCode::Enter | KeyCode::Right => {
                    // Text rows: Enter on a TextRow opens edit mode with the
                    // current value pre-populated.
                    if let Some(super::settings::SettingsRowEntry::Text(row)) =
                        super::settings::settings_row_entry_at(&self.config, self.selected)
                    {
                        self.settings_editing_text = Some(row.value.clone());
                        return;
                    }
                    if let Some(row) = super::settings::settings_row_at(&self.config, self.selected)
                    {
                        // Reset-all sentinel — Enter wipes config back
                        // to default, re-syncs the engine + saves. The
                        // sync is critical: without it the audio engine
                        // keeps the pre-reset values until next launch.
                        if row.key == super::settings::RESET_ALL_KEY {
                            self.config = crate::config::AppConfig::default();
                            self.resync_all_engine_settings();
                            self.toast.show("Settings reset to defaults", 1.0);
                            return;
                        }
                        if !row.options.is_empty() {
                            let next = (row.current_idx + 1) % row.options.len();
                            let key_str = row.key;
                            if self.apply_and_sync_setting(key_str, next) {
                                return;
                            }
                        }
                    }
                }
                KeyCode::Left => {
                    if let Some(row) = super::settings::settings_row_at(&self.config, self.selected)
                        && row.key != super::settings::RESET_ALL_KEY
                        && !row.options.is_empty()
                    {
                        let prev = if row.current_idx == 0 {
                            row.options.len() - 1
                        } else {
                            row.current_idx - 1
                        };
                        let key_str = row.key;
                        if self.apply_and_sync_setting(key_str, prev) {
                            return;
                        }
                    }
                }
                // `r` — reset focused row to its `AppConfig::default()`
                // value. Modifier-gated to NONE so `Ctrl+R` / `Alt+R` /
                // `Cmd+R` don't fire (hunt 2026-06-08 SEV-HIGH: settings
                // Char arms used to ignore modifiers).
                KeyCode::Char('r') if key.modifiers == crossterm::event::KeyModifiers::NONE => {
                    if let Some(row) = super::settings::settings_row_at(&self.config, self.selected)
                        && row.key != super::settings::RESET_ALL_KEY
                        && row.current_idx != row.default_idx
                    {
                        let key_str = row.key;
                        let def = row.default_idx;
                        self.apply_and_sync_setting(key_str, def);
                    }
                }
                // `R` reset-all chord REMOVED 2026-06-08. Was a footgun:
                // one keystroke nuked the user's entire config — output
                // device, transitions, library paths, Claude DJ — with
                // no confirmation. Bare `R` AND every modifier variant
                // (`Ctrl+R` / `Cmd+R`) all triggered it because the
                // match arm ignored modifiers. Reset-all is now reachable
                // only via the sentinel row at the bottom of the list +
                // Enter, which is the intentional path.
                // Esc / `,` / `d` all close Settings and return to
                // Dashboard. Modifier-gated to NONE.
                KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('d')
                    if key.modifiers == crossterm::event::KeyModifiers::NONE =>
                {
                    self.view_mode = ViewMode::Dashboard;
                    self.selected = 0;
                }
                // Also honor the standard navigation hotkeys so users
                // don't get trapped in Settings. Modifier-gated.
                KeyCode::Char('b') if key.modifiers == crossterm::event::KeyModifiers::NONE => {
                    self.view_mode = ViewMode::Browse;
                    self.selected = 0;
                }
                KeyCode::Char('q') if key.modifiers == crossterm::event::KeyModifiers::NONE => {
                    self.view_mode = ViewMode::Queue;
                    self.selected = 0;
                }
                KeyCode::Char('h') => {
                    self.view_mode = ViewMode::History;
                    self.selected = 0;
                }
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
                && let Some((ev, _val)) = state.last_event.clone()
            {
                self.midi_learn_captured = Some(ev);
            }
            match key.code {
                KeyCode::Esc => {
                    self.view_mode = ViewMode::Dashboard;
                }
                KeyCode::Up => {
                    self.midi_learn_action_sel = self.midi_learn_action_sel.saturating_sub(1);
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
                            && let Ok(mut state) = midi.lock()
                        {
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
                        && let Ok(mut state) = midi.lock()
                    {
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
                    self.toast.show(
                        &format!("Deck {}", if self.mixer_deck_is_a { "A" } else { "B" }),
                        0.5,
                    );
                }
                KeyCode::Up => {
                    self.mixer_row = self.mixer_row.prev();
                }
                KeyCode::Down => {
                    self.mixer_row = self.mixer_row.next();
                }
                KeyCode::Left => {
                    self.adjust_mixer_row(-1.0);
                }
                KeyCode::Right => {
                    self.adjust_mixer_row(1.0);
                }
                KeyCode::Char('r') => {
                    self.reset_mixer_row();
                }
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

        // ── The global match block is empty. ──
        // Every chord that used to live here has migrated to a
        // `Command` in `tui::command`, dispatched via `try_dispatch`
        // at the top of this function. The historical chord map is
        // preserved in `docs/COMMAND_MIGRATION.md`.

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
                            let added = self
                                .engine
                                .enqueue(crate::audio::engine::QueueEntry::from(track));
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
                                let artists: Vec<crate::beatport::models::BeatportArtist> = track
                                    .artists
                                    .iter()
                                    .map(|a| crate::beatport::models::BeatportArtist {
                                        id: a.id,
                                        name: a.name.clone(),
                                    })
                                    .collect();
                                self.push_screen(BrowseScreen::ArtistList {
                                    title: "Artists".into(),
                                    artists,
                                });
                            }
                        }
                        1 => {
                            // Remixer column
                            if track.remixers.len() == 1 {
                                let r = &track.remixers[0];
                                self.navigate_to_artist(r.id, &r.name);
                            } else if track.remixers.len() > 1 {
                                let artists: Vec<crate::beatport::models::BeatportArtist> = track
                                    .remixers
                                    .iter()
                                    .map(|r| crate::beatport::models::BeatportArtist {
                                        id: r.id,
                                        name: r.name.clone(),
                                    })
                                    .collect();
                                self.push_screen(BrowseScreen::ArtistList {
                                    title: "Remixers".into(),
                                    artists,
                                });
                            }
                        }
                        2 => {
                            // Label column
                            if let (Some(lid), Some(lname)) =
                                (track.label_id, track.label_name.as_deref())
                            {
                                self.navigate_to_label(lid, lname);
                            }
                        }
                        3 => {
                            // Genre column
                            if let (Some(gid), Some(gname)) =
                                (track.genre_id, track.genre_name.as_deref())
                            {
                                self.navigate_to_genre(gid, gname);
                            }
                        }
                        4 => {
                            // Date column — show tracks from that year
                            if let Some(ref date) = track.release_date
                                && date.len() >= 4
                            {
                                let year = &date[..4];
                                let range = format!("{year}-01-01:{year}-12-31");
                                self.execute_menu_action(MenuAction::LoadDecadeTracks(
                                    range,
                                    track.genre_id,
                                ));
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

    pub(crate) fn handle_deck_control(&mut self, direction: i32) {
        let jb = self.config.jump_bars as i32;
        match self.dash_section {
            CtrlSection::CueA | CtrlSection::JumpA => {
                self.engine.jump(direction * jb);
                self.toast.show(
                    &format!("A {} {jb} bars", if direction > 0 { "▶▶" } else { "◀◀" }),
                    1.0,
                );
            }
            CtrlSection::PlayA | CtrlSection::PlayB => {
                self.engine.pause();
                self.toast.show("Play/Pause", 1.0);
            }
            CtrlSection::NudgeA | CtrlSection::NudgeB => {
                self.engine.nudge(direction);
                self.toast.show(
                    &format!("Nudge {}", if direction > 0 { "▶" } else { "◀" }),
                    0.5,
                );
            }
            CtrlSection::CueB | CtrlSection::JumpB => {
                self.engine.jump(direction * jb);
                self.toast.show(
                    &format!("B {} {jb} bars", if direction > 0 { "▶▶" } else { "◀◀" }),
                    1.0,
                );
            }
            CtrlSection::TempoA => {
                let range = self.config.tempo_range as f64 / 100.0;
                let step = range / 20.0;
                let native = self
                    .cached_info
                    .playing_track
                    .as_ref()
                    .and_then(|t| t.bpm)
                    .unwrap_or(128.0);
                let current = self.cached_info.playing_bpm.unwrap_or(native);
                let ratio = current / native;
                let new_ratio = (ratio + direction as f64 * step).clamp(1.0 - range, 1.0 + range);
                self.engine.set_playing_rate(new_ratio);
                self.toast
                    .show(&format!("Tempo A: {:+.1}%", (new_ratio - 1.0) * 100.0), 0.5);
            }
            CtrlSection::TempoB => {
                let range = self.config.tempo_range as f64 / 100.0;
                let step = range / 20.0;
                let native = self
                    .cached_info
                    .incoming_track
                    .as_ref()
                    .and_then(|t| t.bpm)
                    .unwrap_or(128.0);
                let current = self.cached_info.incoming_bpm.unwrap_or(native);
                let ratio = current / native;
                let new_ratio = (ratio + direction as f64 * step).clamp(1.0 - range, 1.0 + range);
                self.engine.set_incoming_rate(new_ratio);
                self.toast
                    .show(&format!("Tempo B: {:+.1}%", (new_ratio - 1.0) * 100.0), 0.5);
            }
            CtrlSection::VolumeA => {
                let new_vol = if direction > 0 { 1.0f32 } else { 0.0 };
                self.engine.set_volume(0, new_vol);
                self.toast
                    .show(&format!("Vol A: {:.0}%", new_vol * 100.0), 0.5);
            }
            CtrlSection::VolumeB => {
                let new_vol = if direction > 0 { 1.0f32 } else { 0.0 };
                self.engine.set_volume(1, new_vol);
                self.toast
                    .show(&format!("Vol B: {:.0}%", new_vol * 100.0), 0.5);
            }
            CtrlSection::Crossfader => {
                let cur = self.cached_info.crossfader_pos as f64;
                let next = (cur + direction as f64 * 0.05).clamp(-1.0, 1.0);
                self.engine.set_crossfader(next as f32);
                self.toast.show(&format!("Crossfader: {:+.2}", next), 0.4);
            }
            CtrlSection::EqLowA
            | CtrlSection::EqLowB
            | CtrlSection::EqMidA
            | CtrlSection::EqMidB
            | CtrlSection::EqHighA
            | CtrlSection::EqHighB => {
                let is_a = matches!(
                    self.dash_section,
                    CtrlSection::EqLowA | CtrlSection::EqMidA | CtrlSection::EqHighA
                );
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
                self.toast.show(
                    &format!("{band} {}: {next:+.0} dB", Self::deck_label(is_a)),
                    0.4,
                );
            }
            CtrlSection::FilterA | CtrlSection::FilterB => {
                let is_a = matches!(self.dash_section, CtrlSection::FilterA);
                let cur = if is_a {
                    self.cached_info.deck_a_filter_pos
                } else {
                    self.cached_info.deck_b_filter_pos
                };
                let next = (cur + direction as f32 * 0.05).clamp(-1.0, 1.0);
                self.engine.set_filter(is_a, next);
                self.toast.show(
                    &format!("Filter {}: {next:+.2}", Self::deck_label(is_a)),
                    0.4,
                );
            }
        }
    }
}
