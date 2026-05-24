//! MIDI learn TUI screen.
//!
//! Three-pane layout:
//!   ┌─────────────── MIDI Learn ──────────────────────────────┐
//!   │ Captured: CC ch1 #7 = 64                                │
//!   │ Pick an action below, Enter to bind, U to unbind, Esc.  │
//!   ├─────────────────────────────────────────────────────────┤
//!   │ Crossfader                                              │
//!   │ Fader A                                                 │
//!   │ Fader B                                                 │
//!   │ ...                                                     │
//!   ├─────────────────────────────────────────────────────────┤
//!   │ Current bindings:                                       │
//!   │   CC ch15 #14 → Crossfader                              │
//!   │   Note ch2 #10 → Play/Pause                             │
//!   │   ...                                                   │
//!   └─────────────────────────────────────────────────────────┘
//!
//! Captured pane shows whatever the listener saw last (live-updates
//! every render). Action picker lets the user navigate with ↑↓ and
//! Enter to bind the currently-captured event to the selected action.
//! `U` unbinds the captured event (or the highlighted binding in the
//! current-bindings pane). `Esc` returns to the previous view.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::midi::{self, Action, MidiEvent};

/// Render the MIDI learn screen. Reads the latest event from the
/// listener and displays it; the action picker shows `all_actions()`
/// with the cursor at `selected`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    captured: Option<&MidiEvent>,
    captured_value: Option<u32>,
    selected: usize,
    bindings: &[crate::midi::Binding],
) {
    let block = Block::default()
        .title(" MIDI Learn ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // captured pane (3 lines + summary)
            Constraint::Min(8),    // action picker
            Constraint::Length(8.max(bindings.len() as u16 + 2).min(15)), // current bindings
        ])
        .split(inner);

    // Cheap reverse-lookup table built once per render. Avoids O(n²)
    // when we annotate every action row with its binding status.
    let map = midi::MidiMap::load();
    let actions = midi::all_actions();
    let bound_count = actions
        .iter()
        .filter(|a| map.event_for(a).is_some())
        .count();
    let total = actions.len();

    // ── Captured pane ───────────────────────────────────────────
    let captured_text = match captured {
        Some(ev) => {
            let val = captured_value.unwrap_or(0);
            format!("Captured: {} = {}", ev.label(), val)
        }
        None => "Captured: (touch a control on your MIDI device)".to_string(),
    };
    let summary = format!(
        "Bound: {bound_count}/{total}    Unmapped: {}",
        total - bound_count
    );
    let hint = "Pick an action ↑↓, Enter binds, U unbinds, Esc closes";
    let captured_widget = Paragraph::new(vec![
        Line::from(Span::styled(
            captured_text,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            summary,
            Style::default().fg(if total == bound_count {
                Color::Green
            } else {
                Color::Yellow
            }),
        )),
        Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
    ]);
    frame.render_widget(captured_widget, chunks[0]);

    // ── Action picker ───────────────────────────────────────────
    // ✓ prefix = action has a MIDI binding; ○ = unmapped. Bound rows
    // also show the event next to the label so the user can see at a
    // glance which physical control fires each action.
    let items: Vec<ListItem> = actions
        .iter()
        .map(|a| {
            let (marker, color, suffix) = match map.event_for(a) {
                Some(ev) => ("✓", Color::Green, format!("    [{}]", ev.label())),
                None => ("○", Color::DarkGray, String::new()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(a.label()),
                Span::styled(suffix, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(selected.min(actions.len().saturating_sub(1))));
    let list = List::new(items)
        .block(Block::default().title(" Actions ").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    // ── Current bindings ────────────────────────────────────────
    let bindings_lines: Vec<Line> = if bindings.is_empty() {
        vec![Line::from(Span::styled(
            "No bindings yet. Touch a control + pick an action to start.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        bindings
            .iter()
            .map(|b| Line::from(format!("  {}  →  {}", b.event.label(), b.action.label())))
            .collect()
    };
    let bindings_widget = Paragraph::new(bindings_lines)
        .block(Block::default().title(" Bindings ").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(bindings_widget, chunks[2]);
}

/// Total count of bindable actions (for cursor wrap-around).
pub fn action_count() -> usize {
    midi::all_actions().len()
}

/// Index → Action lookup. Returns None for out-of-bounds (defensive).
pub fn action_at(index: usize) -> Option<Action> {
    midi::all_actions().into_iter().nth(index)
}
