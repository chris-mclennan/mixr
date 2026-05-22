//! Full-screen Claude DJ view — enable state, recent log, key hints.
//!
//! Reached via `c` on the dashboard. `C` still toggles on/off globally.
//! Shows the full DJ log scrollback (not just the 4-entry dashboard panel)
//! so the user can audit what the AI has been doing and why.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::claude::dj::{ClaudeDJ, LogEntryType};

pub fn render_claude_screen(
    frame: &mut Frame,
    area: Rect,
    dj: Option<&Arc<TokioMutex<ClaudeDJ>>>,
    scroll_offset: usize,
) {
    let mut lines: Vec<Line> = Vec::new();

    let Some(dj_arc) = dj else {
        lines.push(Line::from(Span::styled(
            "  Claude DJ not available",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Put your Anthropic API key in ~/.mixr/claude_key and relaunch.",
            Style::default().fg(Color::Gray),
        )));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };

    let Ok(dj) = dj_arc.try_lock() else {
        lines.push(Line::from("  (busy — try again in a moment)"));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };

    // Header: enabled state
    let (dot, label, color) = if dj.is_enabled() {
        ("●", "ON", Color::Green)
    } else {
        ("○", "OFF", Color::DarkGray)
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{dot} Claude DJ — "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  [C] toggle   [/] ask   [Esc] back", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(""));

    // Log
    let log = dj.log_entries();
    if log.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no activity yet)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  Activity ({} entries)", log.len()),
            Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        // Newest first; respect scroll_offset so user can page up
        let start = scroll_offset.min(log.len().saturating_sub(1));
        for e in log.iter().rev().skip(start) {
            let (icon, color) = match e.entry_type {
                LogEntryType::Action => ("ACT", Color::Cyan),
                LogEntryType::Track => ("TRK", Color::Green),
                LogEntryType::Phase => ("PHZ", Color::Yellow),
                LogEntryType::Flow => ("FLO", Color::Magenta),
                LogEntryType::User => ("USR", Color::White),
                LogEntryType::Error => ("ERR", Color::Red),
                LogEntryType::Info => ("INF", Color::Gray),
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(e.message.clone(), Style::default().fg(Color::White)),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}
