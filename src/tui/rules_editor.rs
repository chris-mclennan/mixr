//! Transition-rules editor overlay.
//!
//! Two modes: list view (scroll/reorder/delete rules) and edit view (tweak
//! one rule's condition + action). Weighted actions let the user set
//! percentage splits across multiple transitions for the same `when`.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::audio::transition_rules::{Action, Condition, Rule, RuleConfig};

/// All transition names in a stable order. The UI cycles through this list.
pub const TRANSITIONS: &[&str] = &[
    "BeatMatched",
    "EchoOut",
    "BassSwap",
    "FilterSweep",
    "LoopRoll",
];

/// Condition fields the UI exposes. `None` means "no condition" (matches always).
pub const CONDITION_FIELDS: &[&str] = &[
    "(any — matches always)",
    "bpm_gap_pct_gt",
    "bpm_gap_pct_lt",
    "key_dist_eq",
    "key_dist_lte",
    "key_dist_gte",
    "last_transition_eq",
    "mix_count_mod",
    "energy_delta_gt",
    "energy_delta_lt",
    "phrase_is_drop",
    "time_in_set_min_gt",
    "time_in_set_min_lt",
];

pub const ACTION_KINDS: &[&str] = &["force", "cycle", "weighted"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    When,
    Then,
    List,
}

#[derive(Debug, Clone)]
pub struct EditState {
    /// Existing rule index; `None` if inserting a new rule.
    pub idx: Option<usize>,
    pub rule: Rule,
    pub focus: Focus,
    /// Currently-selected choice within a Cycle or Weighted action.
    pub action_selected: usize,
}

// Lint expects boxing the larger variant for Vec<Mode> efficiency.
// Mode is held as a single `editor.mode` field, never in a Vec, so
// boxing would add indirection without saving memory.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Mode {
    List,
    Edit(EditState),
}

pub struct RulesEditor {
    pub config: RuleConfig,
    pub selected: usize, // 0..rules.len() = rule; rules.len() = "Default" row
    pub mode: Mode,
    pub dirty: bool,
}

impl RulesEditor {
    pub fn new(config: RuleConfig) -> Self {
        Self {
            config,
            selected: 0,
            mode: Mode::List,
            dirty: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers — convert between UI state and rule-engine structs
// ---------------------------------------------------------------------------

fn blank_condition() -> Condition {
    Condition::default()
}
fn blank_rule() -> Rule {
    Rule {
        when: blank_condition(),
        then: Action::Force("BeatMatched".into()),
    }
}

/// Return the index (into CONDITION_FIELDS) of the *first* set field, or 0
/// ("(any)") if none. Conditions with multiple fields set are collapsed to
/// whichever we hit first — hand-edited JSON retains the rest on save.
pub fn condition_field_idx(c: &Condition) -> usize {
    if c.bpm_gap_pct_gt.is_some() {
        return 1;
    }
    if c.bpm_gap_pct_lt.is_some() {
        return 2;
    }
    if c.key_dist_eq.is_some() {
        return 3;
    }
    if c.key_dist_lte.is_some() {
        return 4;
    }
    if c.key_dist_gte.is_some() {
        return 5;
    }
    if c.last_transition_eq.is_some() {
        return 6;
    }
    if c.mix_count_mod.is_some() {
        return 7;
    }
    if c.energy_delta_gt.is_some() {
        return 8;
    }
    if c.energy_delta_lt.is_some() {
        return 9;
    }
    if c.phrase_is_drop.is_some() {
        return 10;
    }
    if c.time_in_set_min_gt.is_some() {
        return 11;
    }
    if c.time_in_set_min_lt.is_some() {
        return 12;
    }
    0
}

pub fn condition_value_str(c: &Condition, field_idx: usize) -> String {
    match field_idx {
        1 => c.bpm_gap_pct_gt.map(|v| format!("{v}")).unwrap_or_default(),
        2 => c.bpm_gap_pct_lt.map(|v| format!("{v}")).unwrap_or_default(),
        3 => c.key_dist_eq.map(|v| format!("{v}")).unwrap_or_default(),
        4 => c.key_dist_lte.map(|v| format!("{v}")).unwrap_or_default(),
        5 => c.key_dist_gte.map(|v| format!("{v}")).unwrap_or_default(),
        6 => c.last_transition_eq.clone().unwrap_or_default(),
        7 => c
            .mix_count_mod
            .map(|[m, r]| format!("{m},{r}"))
            .unwrap_or_default(),
        8 => c
            .energy_delta_gt
            .map(|v| format!("{v}"))
            .unwrap_or_default(),
        9 => c
            .energy_delta_lt
            .map(|v| format!("{v}"))
            .unwrap_or_default(),
        10 => c.phrase_is_drop.map(|v| v.to_string()).unwrap_or_default(),
        11 => c
            .time_in_set_min_gt
            .map(|v| format!("{v}"))
            .unwrap_or_default(),
        12 => c
            .time_in_set_min_lt
            .map(|v| format!("{v}"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Replace the currently-set field with one at `new_field_idx`, keeping the
/// existing value when the types align. Clears other fields.
pub fn set_condition_field(c: &mut Condition, new_field_idx: usize) {
    *c = blank_condition();
    // Seed with a sensible default value for each field type.
    match new_field_idx {
        1 => c.bpm_gap_pct_gt = Some(8.0),
        2 => c.bpm_gap_pct_lt = Some(8.0),
        3 => c.key_dist_eq = Some(0),
        4 => c.key_dist_lte = Some(1),
        5 => c.key_dist_gte = Some(2),
        6 => c.last_transition_eq = Some("BassSwap".into()),
        7 => c.mix_count_mod = Some([3, 0]),
        8 => c.energy_delta_gt = Some(0.1),
        9 => c.energy_delta_lt = Some(-0.1),
        10 => c.phrase_is_drop = Some(true),
        11 => c.time_in_set_min_gt = Some(30),
        12 => c.time_in_set_min_lt = Some(30),
        _ => {}
    }
}

/// Adjust the current condition value by `step` (+1 or -1). Best-effort:
/// numeric fields shift by a sensible unit; bool toggles; string cycles.
pub fn adjust_condition_value(c: &mut Condition, field_idx: usize, step: i32) {
    match field_idx {
        1 => {
            if let Some(ref mut v) = c.bpm_gap_pct_gt {
                *v = (*v + step as f64).max(0.0);
            }
        }
        2 => {
            if let Some(ref mut v) = c.bpm_gap_pct_lt {
                *v = (*v + step as f64).max(0.0);
            }
        }
        3 => {
            if let Some(ref mut v) = c.key_dist_eq {
                let n = *v as i32 + step;
                *v = n.clamp(0, 6) as usize;
            }
        }
        4 => {
            if let Some(ref mut v) = c.key_dist_lte {
                let n = *v as i32 + step;
                *v = n.clamp(0, 6) as usize;
            }
        }
        5 => {
            if let Some(ref mut v) = c.key_dist_gte {
                let n = *v as i32 + step;
                *v = n.clamp(0, 6) as usize;
            }
        }
        6 => {
            if let Some(ref mut s) = c.last_transition_eq {
                let i = TRANSITIONS.iter().position(|t| t == s).unwrap_or(0);
                let n = (i as i32 + step).rem_euclid(TRANSITIONS.len() as i32) as usize;
                *s = TRANSITIONS[n].to_string();
            }
        }
        7 => {
            if let Some(arr) = c.mix_count_mod.as_mut() {
                let n = (arr[1] as i32 + step).rem_euclid(10).max(0);
                arr[1] = n as u32;
            }
        }
        8 => {
            if let Some(ref mut v) = c.energy_delta_gt {
                *v = (*v + 0.05 * step as f64).clamp(-1.0, 1.0);
            }
        }
        9 => {
            if let Some(ref mut v) = c.energy_delta_lt {
                *v = (*v + 0.05 * step as f64).clamp(-1.0, 1.0);
            }
        }
        10 => {
            if let Some(ref mut v) = c.phrase_is_drop {
                *v = !*v;
                let _ = step;
            }
        }
        11 => {
            if let Some(ref mut v) = c.time_in_set_min_gt {
                let n = *v as i32 + step * 5;
                *v = n.max(0) as u32;
            }
        }
        12 => {
            if let Some(ref mut v) = c.time_in_set_min_lt {
                let n = *v as i32 + step * 5;
                *v = n.max(0) as u32;
            }
        }
        _ => {}
    }
}

pub fn action_kind_idx(a: &Action) -> usize {
    match a {
        Action::Force(_) => 0,
        Action::Cycle { .. } => 1,
        Action::Weighted { .. } => 2,
        Action::Skip { .. } => 0, // collapse skip to force for editor; JSON retains skip rules untouched
    }
}

pub fn action_choices(a: &Action) -> Vec<(String, f64)> {
    match a {
        Action::Force(s) => vec![(s.clone(), 1.0)],
        Action::Cycle { cycle } => cycle.iter().map(|s| (s.clone(), 1.0)).collect(),
        Action::Weighted { weighted } => weighted.clone(),
        Action::Skip { .. } => vec![("BeatMatched".into(), 1.0)],
    }
}

pub fn build_action(kind_idx: usize, choices: &[(String, f64)]) -> Action {
    let choices: Vec<(String, f64)> = if choices.is_empty() {
        vec![("BeatMatched".into(), 1.0)]
    } else {
        choices.to_vec()
    };
    match kind_idx {
        0 => Action::Force(choices[0].0.clone()),
        1 => Action::Cycle {
            cycle: choices.iter().map(|(s, _)| s.clone()).collect(),
        },
        _ => Action::Weighted { weighted: choices },
    }
}

pub fn describe_condition(c: &Condition) -> String {
    let idx = condition_field_idx(c);
    if idx == 0 {
        return "(any)".into();
    }
    let v = condition_value_str(c, idx);
    format!(
        "{} {}",
        CONDITION_FIELDS[idx],
        if v.is_empty() { "—" } else { v.as_str() }
    )
}

pub fn describe_action(a: &Action) -> String {
    match a {
        Action::Force(s) => s.clone(),
        Action::Cycle { cycle } => format!("cycle[{}]", cycle.join(", ")),
        Action::Weighted { weighted } => {
            let total: f64 = weighted.iter().map(|(_, w)| w.max(0.0)).sum();
            let total = if total > 0.0 { total } else { 1.0 };
            let parts: Vec<String> = weighted
                .iter()
                .map(|(name, w)| format!("{name} {:.0}%", w / total * 100.0))
                .collect();
            format!("weighted[{}]", parts.join(", "))
        }
        Action::Skip { .. } => "skip".into(),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub fn render(frame: &mut Frame, area: Rect, editor: &RulesEditor) {
    // Outer app already draws a titled block around `area`; don't add a
    // second border here — caused the double-outline look.
    match &editor.mode {
        Mode::List => render_list(frame, area, editor),
        Mode::Edit(state) => render_edit(frame, area, state),
    }
}

fn render_list(frame: &mut Frame, area: Rect, editor: &RulesEditor) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Rules evaluate top-down; first match wins.",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    for (i, rule) in editor.config.rules.iter().enumerate() {
        let is_sel = i == editor.selected;
        let marker = if is_sel { "▸ " } else { "  " };
        let text = format!(
            "{marker}{:>2}. when  {:<45}  →  {}",
            i + 1,
            describe_condition(&rule.when),
            describe_action(&rule.then),
        );
        let style = if is_sel {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    lines.push(Line::from(""));
    // Default row
    let def_sel = editor.selected == editor.config.rules.len();
    let marker = if def_sel { "▸ " } else { "  " };
    let def_line = format!(
        "{marker}Default                                                  →  {}",
        editor.config.default
    );
    let style = if def_sel {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    lines.push(Line::from(Span::styled(def_line, style)));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [↑↓] select  [Enter] edit  [i] insert  [D] del  [{/}] reorder  [Z] smart defaults  [Esc] save+close",
        Style::default().fg(Color::DarkGray),
    )));
    if editor.dirty {
        lines.push(Line::from(Span::styled(
            "  (unsaved changes — Esc to save)",
            Style::default().fg(Color::Yellow),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_edit(frame: &mut Frame, area: Rect, state: &EditState) {
    let mut lines: Vec<Line> = Vec::new();
    let title = match state.idx {
        Some(i) => format!("Edit Rule {}", i + 1),
        None => "New Rule".into(),
    };
    lines.push(Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // When panel
    let when_focused = matches!(state.focus, Focus::When);
    let field_idx = condition_field_idx(&state.rule.when);
    let field_name = CONDITION_FIELDS[field_idx];
    let field_val = condition_value_str(&state.rule.when, field_idx);
    let pad_head = |s: &str| format!("  {s}");
    let highlight = |txt: String, on: bool| {
        if on {
            Span::styled(
                txt,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(txt, Style::default().fg(Color::Cyan))
        }
    };
    lines.push(Line::from(Span::styled(
        pad_head(if when_focused { "▸ When:" } else { "  When:" }),
        Style::default()
            .fg(if when_focused {
                Color::Yellow
            } else {
                Color::Gray
            })
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::raw("      Field:   "),
        highlight(format!("[ {field_name} ]"), when_focused),
    ]));
    lines.push(Line::from(vec![
        Span::raw("      Value:   "),
        Span::styled(
            if field_val.is_empty() {
                "—".into()
            } else {
                format!("[ {field_val} ]")
            },
            Style::default().fg(if when_focused {
                Color::Yellow
            } else {
                Color::Gray
            }),
        ),
    ]));
    lines.push(Line::from(""));

    // Then panel
    let then_focused = matches!(state.focus, Focus::Then);
    let list_focused = matches!(state.focus, Focus::List);
    let kind_idx = action_kind_idx(&state.rule.then);
    let kind = ACTION_KINDS[kind_idx];
    lines.push(Line::from(Span::styled(
        pad_head(if then_focused || list_focused {
            "▸ Then:"
        } else {
            "  Then:"
        }),
        Style::default()
            .fg(if then_focused || list_focused {
                Color::Yellow
            } else {
                Color::Gray
            })
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::raw("      Kind:   "),
        highlight(format!("[ {kind} ]"), then_focused),
    ]));

    // Choices
    let choices = action_choices(&state.rule.then);
    let total: f64 = choices
        .iter()
        .map(|(_, w)| w.max(0.0))
        .sum::<f64>()
        .max(1e-9);
    if matches!(state.rule.then, Action::Force(_)) {
        // Single choice shown plainly
        lines.push(Line::from(vec![
            Span::raw("      Pick:   "),
            highlight(format!("[ {} ]", choices[0].0), list_focused),
        ]));
    } else {
        lines.push(Line::from("      Choices:"));
        for (i, (name, w)) in choices.iter().enumerate() {
            let is_sel = list_focused && i == state.action_selected;
            let marker = if is_sel { "▸" } else { " " };
            let weight_text = match state.rule.then {
                Action::Weighted { .. } => format!("  {:>3.0} %", w / total * 100.0),
                _ => String::new(),
            };
            let row = format!("        {marker} {name:<14}{weight_text}");
            let style = if is_sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(row, style)));
        }
    }

    // Per-transition volume-curve preview. Shows playing (▆) and incoming
    // (▂) volume curves over a 32-wide progress axis for the action's
    // first-choice transition. Helpful visual of what each action kind
    // means audibly.
    lines.push(Line::from(""));
    let preview_name = match &state.rule.then {
        Action::Force(s) => Some(s.clone()),
        Action::Cycle { cycle } if !cycle.is_empty() => Some(cycle[0].clone()),
        Action::Weighted { weighted } if !weighted.is_empty() => Some(weighted[0].0.clone()),
        _ => None,
    };
    if let Some(name) = preview_name
        && let Some(curves) = volume_curves(&name)
    {
        lines.push(Line::from(Span::styled(
            format!("      Preview ({}):", name),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(vec![
            Span::raw("        playing  "),
            Span::styled(curves.0, Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("        incoming "),
            Span::styled(curves.1, Style::default().fg(Color::Magenta)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Tab] switch pane   [←→] cycle field/value/kind   [↑↓] choice   [+] add   [D] remove   [Enter] save   [Esc] cancel",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render per-transition volume curves as two ASCII bars (playing, incoming)
/// across progress 0..1 in 32 steps. Height encoded with 8 block glyphs.
fn volume_curves(name: &str) -> Option<(String, String)> {
    use crate::audio::transition::TransitionType;
    let t = match name {
        "BeatMatched" => TransitionType::BeatMatched,
        "EchoOut" => TransitionType::EchoOut,
        "BassSwap" => TransitionType::BassSwap,
        "FilterSweep" => TransitionType::FilterSweep,
        "LoopRoll" => TransitionType::LoopRoll,
        _ => return None,
    };
    const N: usize = 32;
    const GLYPHS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut pv = String::with_capacity(N);
    let mut iv = String::with_capacity(N);
    for i in 0..N {
        let p = i as f64 / (N - 1) as f64;
        let v_play = t.playing_volume(p) as f64;
        let v_in = t.incoming_volume(p) as f64;
        let idx_play =
            ((v_play.clamp(0.0, 1.0) * (GLYPHS.len() - 1) as f64) as usize).min(GLYPHS.len() - 1);
        let idx_in =
            ((v_in.clamp(0.0, 1.0) * (GLYPHS.len() - 1) as f64) as usize).min(GLYPHS.len() - 1);
        pv.push(GLYPHS[idx_play]);
        iv.push(GLYPHS[idx_in]);
    }
    Some((pv, iv))
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Result of a key event. Owned up to the caller to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyResult {
    Continue,
    /// User confirmed save + close (Esc from list view with dirty, or always).
    CloseSaved,
    /// User cancelled (Esc mid-edit).
    ContinueCancelled,
}

pub fn handle_key(editor: &mut RulesEditor, key: crossterm::event::KeyEvent) -> KeyResult {
    use crossterm::event::KeyCode;
    match editor.mode.clone() {
        Mode::List => match key.code {
            KeyCode::Up => {
                if editor.selected > 0 {
                    editor.selected -= 1;
                }
                KeyResult::Continue
            }
            KeyCode::Down => {
                let max = editor.config.rules.len();
                if editor.selected < max {
                    editor.selected += 1;
                }
                KeyResult::Continue
            }
            KeyCode::Enter => {
                if editor.selected < editor.config.rules.len() {
                    let rule = editor.config.rules[editor.selected].clone();
                    editor.mode = Mode::Edit(EditState {
                        idx: Some(editor.selected),
                        rule,
                        focus: Focus::When,
                        action_selected: 0,
                    });
                } else {
                    // Editing the Default row = cycle transition.
                    let cur = TRANSITIONS
                        .iter()
                        .position(|t| t == &editor.config.default.as_str())
                        .unwrap_or(0);
                    let next = (cur + 1) % TRANSITIONS.len();
                    editor.config.default = TRANSITIONS[next].into();
                    editor.dirty = true;
                }
                KeyResult::Continue
            }
            KeyCode::Char('i') => {
                editor.mode = Mode::Edit(EditState {
                    idx: None,
                    rule: blank_rule(),
                    focus: Focus::When,
                    action_selected: 0,
                });
                KeyResult::Continue
            }
            KeyCode::Char('D') => {
                if editor.selected < editor.config.rules.len() {
                    editor.config.rules.remove(editor.selected);
                    if editor.selected > 0 && editor.selected >= editor.config.rules.len() {
                        editor.selected -= 1;
                    }
                    editor.dirty = true;
                }
                KeyResult::Continue
            }
            KeyCode::Char('{') => {
                if editor.selected > 0 && editor.selected < editor.config.rules.len() {
                    editor
                        .config
                        .rules
                        .swap(editor.selected, editor.selected - 1);
                    editor.selected -= 1;
                    editor.dirty = true;
                }
                KeyResult::Continue
            }
            KeyCode::Char('}') => {
                if editor.selected + 1 < editor.config.rules.len() {
                    editor
                        .config
                        .rules
                        .swap(editor.selected, editor.selected + 1);
                    editor.selected += 1;
                    editor.dirty = true;
                }
                KeyResult::Continue
            }
            KeyCode::Char('Z') => {
                // Reset to smart defaults (variation + context-aware
                // weighted picks). Overwrites in-memory edits; Esc
                // saves to disk as usual.
                editor.config = crate::audio::transition_rules::RuleConfig::default();
                editor.selected = 0;
                editor.dirty = true;
                KeyResult::Continue
            }
            KeyCode::Esc => KeyResult::CloseSaved,
            _ => KeyResult::Continue,
        },
        // Edit-mode key handling. `state` is taken by value, mutated in place,
        // and put back into `editor.mode` at the end — except for Esc/Enter,
        // which transition to List and return early. Previously each arm
        // restored `editor.mode = Mode::Edit(state)` individually; consolidated
        // here so the arms only carry their own logic.
        Mode::Edit(mut state) => {
            let result = match key.code {
                KeyCode::Esc => {
                    editor.mode = Mode::List;
                    return KeyResult::ContinueCancelled;
                }
                KeyCode::Enter => {
                    match state.idx {
                        Some(i) => {
                            editor.config.rules[i] = state.rule.clone();
                        }
                        None => {
                            editor.config.rules.push(state.rule.clone());
                            editor.selected = editor.config.rules.len() - 1;
                        }
                    }
                    editor.dirty = true;
                    editor.mode = Mode::List;
                    return KeyResult::Continue;
                }
                KeyCode::Tab => {
                    state.focus = match state.focus {
                        Focus::When => Focus::Then,
                        Focus::Then => Focus::List,
                        Focus::List => Focus::When,
                    };
                    KeyResult::Continue
                }
                KeyCode::Left | KeyCode::Right => {
                    let step = if matches!(key.code, KeyCode::Right) {
                        1
                    } else {
                        -1
                    };
                    match state.focus {
                        Focus::When => {
                            let cur = condition_field_idx(&state.rule.when);
                            let next = (cur as i32 + step).rem_euclid(CONDITION_FIELDS.len() as i32)
                                as usize;
                            set_condition_field(&mut state.rule.when, next);
                        }
                        Focus::Then => {
                            let cur = action_kind_idx(&state.rule.then);
                            let next =
                                (cur as i32 + step).rem_euclid(ACTION_KINDS.len() as i32) as usize;
                            let choices = action_choices(&state.rule.then);
                            state.rule.then = build_action(next, &choices);
                        }
                        Focus::List => match state.rule.then.clone() {
                            Action::Weighted { mut weighted } if !weighted.is_empty() => {
                                let i = state.action_selected.min(weighted.len() - 1);
                                weighted[i].1 = (weighted[i].1 + step as f64).max(0.0);
                                state.rule.then = Action::Weighted { weighted };
                            }
                            Action::Cycle { mut cycle } if !cycle.is_empty() => {
                                let i = state.action_selected.min(cycle.len() - 1);
                                let cur = TRANSITIONS
                                    .iter()
                                    .position(|t| t == &cycle[i].as_str())
                                    .unwrap_or(0);
                                let next = (cur as i32 + step).rem_euclid(TRANSITIONS.len() as i32)
                                    as usize;
                                cycle[i] = TRANSITIONS[next].into();
                                state.rule.then = Action::Cycle { cycle };
                            }
                            Action::Force(cur) => {
                                let i = TRANSITIONS
                                    .iter()
                                    .position(|t| t == &cur.as_str())
                                    .unwrap_or(0);
                                let next =
                                    (i as i32 + step).rem_euclid(TRANSITIONS.len() as i32) as usize;
                                state.rule.then = Action::Force(TRANSITIONS[next].into());
                            }
                            _ => {}
                        },
                    }
                    KeyResult::Continue
                }
                KeyCode::Up | KeyCode::Down => {
                    let step: i32 = if matches!(key.code, KeyCode::Up) {
                        1
                    } else {
                        -1
                    };
                    match state.focus {
                        Focus::When => {
                            let field_idx = condition_field_idx(&state.rule.when);
                            adjust_condition_value(&mut state.rule.when, field_idx, step);
                        }
                        Focus::List => {
                            let len = action_choices(&state.rule.then).len();
                            if len > 0 {
                                let next = (state.action_selected as i32 - step)
                                    .rem_euclid(len as i32)
                                    as usize;
                                state.action_selected = next;
                            }
                        }
                        _ => {}
                    }
                    KeyResult::Continue
                }
                KeyCode::Char('+') => {
                    match state.rule.then.clone() {
                        Action::Weighted { mut weighted } => {
                            weighted.push(("BeatMatched".into(), 1.0));
                            state.rule.then = Action::Weighted { weighted };
                        }
                        Action::Cycle { mut cycle } => {
                            cycle.push("BeatMatched".into());
                            state.rule.then = Action::Cycle { cycle };
                        }
                        _ => {}
                    }
                    KeyResult::Continue
                }
                KeyCode::Char('D') => {
                    match state.rule.then.clone() {
                        Action::Weighted { mut weighted } if weighted.len() > 1 => {
                            let i = state.action_selected.min(weighted.len() - 1);
                            weighted.remove(i);
                            if state.action_selected >= weighted.len() {
                                state.action_selected = weighted.len() - 1;
                            }
                            state.rule.then = Action::Weighted { weighted };
                        }
                        Action::Cycle { mut cycle } if cycle.len() > 1 => {
                            let i = state.action_selected.min(cycle.len() - 1);
                            cycle.remove(i);
                            if state.action_selected >= cycle.len() {
                                state.action_selected = cycle.len() - 1;
                            }
                            state.rule.then = Action::Cycle { cycle };
                        }
                        _ => {}
                    }
                    KeyResult::Continue
                }
                _ => KeyResult::Continue,
            };
            editor.mode = Mode::Edit(state);
            result
        }
    }
}
