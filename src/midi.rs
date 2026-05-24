//! MIDI controller integration.
//!
//! Generic MIDI-input layer that maps device events (CC, note-on,
//! pitch-bend) to mixr operations via a user-editable JSON map at
//! `~/.mixr/midi-map.json`. Background thread listens to all
//! connected MIDI inputs, translates events through the map, and
//! dispatches as IPC commands so any mixr action that's IPC-
//! reachable is also MIDI-bindable for free.
//!
//! ### MIDI learn
//!
//! From the TUI: `M` opens the MIDI learn screen. The next event
//! observed from any device shows up there; user picks an `Action`
//! from a list, confirms, and the binding is appended to the JSON
//! map. Subsequent events fire the bound action immediately.
//!
//! ### Why IPC dispatch (not direct engine calls)
//!
//! MIDI events arrive on a non-tokio thread (`midir` callbacks).
//! Going through the existing IPC command file means the listener
//! stays simple (just write JSON to a file) and reuses every input
//! validator, toast, and side-effect that IPC commands already have.
//! Cost: file write per event (~100µs on warm cache). Fine — even
//! a busy DJ session is <100 events/sec.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

const MAP_FILENAME: &str = "midi-map.json";

fn map_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".mixr")
        .join(MAP_FILENAME)
}

/// Identifies a single MIDI control event without its data byte.
/// CC#7 ch 1 → ControlChange { channel: 0, controller: 7 }, regardless
/// of value. Used as the lookup key in the binding map.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidiEvent {
    /// Continuous controller (knob, fader, pitch slider). `value`
    /// in 0..=127.
    ControlChange { channel: u8, controller: u8 },
    /// Note-on (button press, hot cue, jog touch). `velocity` is in
    /// the originating event but we use it just for press/release —
    /// 0 velocity is treated as note-off.
    NoteOn { channel: u8, note: u8 },
    /// Pitch-bend wheel. Value is i14 in [-8192, 8191].
    PitchBend { channel: u8 },
}

impl MidiEvent {
    /// Compact human label for display in the learn UI.
    pub fn label(&self) -> String {
        match self {
            Self::ControlChange {
                channel,
                controller,
            } => format!("CC ch{} #{}", channel + 1, controller),
            Self::NoteOn { channel, note } => format!("Note ch{} #{}", channel + 1, note),
            Self::PitchBend { channel } => format!("Bend ch{}", channel + 1),
        }
    }
}

/// What a MIDI event maps to on mixr's side. Closed enum so the
/// learn UI can show a complete list of bindable operations and
/// the dispatcher knows exactly how to translate. Adding a new
/// action: add a variant + a dispatch case in `Action::dispatch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Crossfader, // CC value → -1..1
    ChannelFader {
        is_a: bool,
    }, // CC value → 0..1
    EqLow {
        is_a: bool,
    }, // CC value → -24..+12 dB
    EqMid {
        is_a: bool,
    },
    EqHigh {
        is_a: bool,
    },
    Filter {
        is_a: bool,
    }, // CC value → -1..1 (LP..HP)
    Tempo {
        is_a: bool,
    }, // CC value → ±range%
    JumpBars(i32), // Note-on triggers ±N bars
    PlayPause,     // Note-on toggles
    Skip,          // Note-on advances queue
    MixNow,        // Note-on starts crossfade
    Cue {
        is_a: bool,
        slot: u8,
    }, // Note-on jumps to hot cue
    CueSet {
        is_a: bool,
        slot: u8,
    }, // Note-on sets hot cue
    LoopBeats {
        is_a: bool,
        beats: f64,
    }, // Note-on toggles loop
    Nudge(i32),    // Note-on triggers ±1 nudge (global — picks playing/incoming)
    NudgeDeck {
        is_a: bool,
        direction: i32,
    }, // Note-on triggers ±1 nudge on a specific deck
    GridShift(f64), // Note-on shifts grid by ms
    Transition(String), // Note-on selects transition (string = transition name)
    /// Per-deck play/pause. Independent of the global `PlayPause`,
    /// which toggles the playing-deck only. Use this for the play
    /// button on each physical deck.
    PlayPauseDeck {
        is_a: bool,
    },
    /// Per-deck bar jump. Same semantic as the global JumpBars but
    /// targets a specific deck.
    JumpBarsDeck {
        is_a: bool,
        bars: i32,
    },

    // ── UI navigation ────────────────────────────────────────────────
    // Used by browse knob, view/source buttons, back button on the
    // Mixstream's nav cluster. Lets the controller drive the TUI's
    // own list/menu navigation alongside the audio controls.
    /// Move the cursor up one row in any list (browse, queue, settings).
    /// Map to a momentary button.
    NavigateUp,
    /// Move the cursor down.
    NavigateDown,
    /// Drill into the selected item / confirm. Equivalent to Enter.
    NavigateEnter,
    /// Pop back up one screen / cancel. Equivalent to Esc.
    NavigateBack,
    /// Rotary-encoder navigation. Single binding handles both
    /// directions: CC values 1..63 (or 1) → down, 65..127 (or 127) → up.
    /// 0 / 64 = no-op. Most DJ controller browse knobs send this kind
    /// of relative pulse instead of absolute position.
    BrowseScroll,
    /// Switch to a named view (dashboard, browse, queue, history,
    /// settings). String matched against the IPC view commands.
    SwitchView(String),
    /// Re-enable auto-mix triggering after a user override.
    ResumeAuto,
    /// Load the currently-highlighted browse-list track to a specific
    /// deck. Refuses if the target deck is currently playing.
    LoadDeck {
        is_a: bool,
    },
}

impl Action {
    /// Short label for display in the learn picker.
    pub fn label(&self) -> String {
        match self {
            Self::Crossfader => "Crossfader".into(),
            Self::ChannelFader { is_a } => format!("Fader {}", deck(*is_a)),
            Self::EqLow { is_a } => format!("EQ Low {}", deck(*is_a)),
            Self::EqMid { is_a } => format!("EQ Mid {}", deck(*is_a)),
            Self::EqHigh { is_a } => format!("EQ High {}", deck(*is_a)),
            Self::Filter { is_a } => format!("Filter {}", deck(*is_a)),
            Self::Tempo { is_a } => format!("Tempo {}", deck(*is_a)),
            Self::JumpBars(n) => format!("Jump {n:+} bars"),
            Self::PlayPause => "Play/Pause".into(),
            Self::Skip => "Skip".into(),
            Self::MixNow => "Mix Now".into(),
            Self::Cue { is_a, slot } => format!("Cue {} jump {}", slot + 1, deck(*is_a)),
            Self::CueSet { is_a, slot } => format!("Cue {} set {}", slot + 1, deck(*is_a)),
            Self::LoopBeats { is_a, beats } => format!("Loop {beats}-beat {}", deck(*is_a)),
            Self::Nudge(n) => format!("Nudge {n:+}"),
            Self::NudgeDeck { is_a, direction } => format!("Nudge {direction:+} {}", deck(*is_a)),
            Self::GridShift(ms) => format!("Grid shift {ms:+}ms"),
            Self::Transition(name) => format!("Transition: {name}"),
            Self::PlayPauseDeck { is_a } => format!("Play/Pause {}", deck(*is_a)),
            Self::JumpBarsDeck { is_a, bars } => format!("Jump {bars:+} bars {}", deck(*is_a)),
            Self::NavigateUp => "Nav: Up".into(),
            Self::NavigateDown => "Nav: Down".into(),
            Self::NavigateEnter => "Nav: Enter".into(),
            Self::NavigateBack => "Nav: Back".into(),
            Self::BrowseScroll => "Browse Knob (rotary)".into(),
            Self::SwitchView(v) => format!("View: {v}"),
            Self::ResumeAuto => "Auto-Mix: Resume".into(),
            Self::LoadDeck { is_a } => format!("Load → Deck {}", deck(*is_a)),
        }
    }

    /// Convert a raw MIDI event payload into the IPC command JSON
    /// that mixr's command parser already understands. Returns None
    /// if the event doesn't apply (e.g., note-off where note-on is
    /// required, or out-of-range value).
    pub fn to_ipc_command(&self, value: u32) -> Option<String> {
        // Helpers: CC value (0..=127) into normalized ranges.
        let cc01 = value as f64 / 127.0; // 0..1
        let cc_signed = (value as f64 / 127.0) * 2.0 - 1.0; // -1..1
        let cc_db = (cc01 - 0.5) * 36.0; // -18..+18 dB nominal range
        // Note-on: presence triggers, value is the velocity (>0 = press).
        let pressed = value > 0;

        Some(match self {
            Self::Crossfader => format!("{{\"crossfader\":{cc_signed}}}"),
            Self::ChannelFader { is_a } => format!(
                "{{\"fader\":{{\"{}\":{cc01}}}}}",
                if *is_a { "a" } else { "b" }
            ),
            Self::EqLow { is_a } => format!(
                "{{\"eq\":{{\"deck\":\"{}\",\"low\":{cc_db}}}}}",
                if *is_a { "a" } else { "b" }
            ),
            Self::EqMid { is_a } => format!(
                "{{\"eq\":{{\"deck\":\"{}\",\"mid\":{cc_db}}}}}",
                if *is_a { "a" } else { "b" }
            ),
            Self::EqHigh { is_a } => format!(
                "{{\"eq\":{{\"deck\":\"{}\",\"high\":{cc_db}}}}}",
                if *is_a { "a" } else { "b" }
            ),
            Self::Filter { is_a } => format!(
                "{{\"deck_filter\":{{\"deck\":\"{}\",\"pos\":{cc_signed}}}}}",
                if *is_a { "a" } else { "b" }
            ),
            Self::Tempo { is_a } => {
                // ±8% of base tempo. Slider center = 1.0 rate.
                let rate = 1.0 + cc_signed * 0.08;
                format!(
                    "{{\"setrate\":{{\"deck\":\"{}\",\"rate\":{rate}}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
            Self::JumpBars(n) => {
                if !pressed {
                    return None;
                }
                format!("{{\"jump\":{n}}}")
            }
            Self::PlayPause => {
                if !pressed {
                    return None;
                }
                "{\"pause\":1}".into()
            }
            Self::Skip => {
                if !pressed {
                    return None;
                }
                "{\"skip\":1}".into()
            }
            Self::MixNow => {
                if !pressed {
                    return None;
                }
                "{\"mixnow\":1}".into()
            }
            Self::Cue { is_a, slot } => {
                if !pressed {
                    return None;
                }
                format!(
                    "{{\"cue\":{{\"deck\":\"{}\",\"slot\":{slot}}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
            Self::CueSet { is_a, slot } => {
                if !pressed {
                    return None;
                }
                format!(
                    "{{\"cue_set\":{{\"deck\":\"{}\",\"slot\":{slot}}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
            Self::LoopBeats { is_a: _is_a, beats } => {
                if !pressed {
                    return None;
                }
                format!("{{\"loop\":{{\"beats\":{beats}}}}}")
            }
            Self::Nudge(n) => {
                if !pressed {
                    return None;
                }
                format!("{{\"nudge\":{n}}}")
            }
            Self::NudgeDeck { is_a, direction } => {
                if !pressed {
                    return None;
                }
                format!(
                    "{{\"nudge\":{{\"deck\":\"{}\",\"direction\":{direction}}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
            Self::GridShift(ms) => {
                if !pressed {
                    return None;
                }
                format!("{{\"shiftgrid\":{ms}}}")
            }
            Self::Transition(name) => {
                if !pressed {
                    return None;
                }
                format!("{{\"transition\":\"{name}\"}}")
            }
            Self::PlayPauseDeck { is_a } => {
                if !pressed {
                    return None;
                }
                format!(
                    "{{\"play_deck\":{{\"deck\":\"{}\",\"toggle\":true}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
            Self::JumpBarsDeck { is_a, bars } => {
                if !pressed {
                    return None;
                }
                format!(
                    "{{\"jump\":{{\"deck\":\"{}\",\"bars\":{bars}}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
            Self::NavigateUp => {
                if !pressed {
                    return None;
                }
                "{\"navigate\":\"up\"}".into()
            }
            Self::NavigateDown => {
                if !pressed {
                    return None;
                }
                "{\"navigate\":\"down\"}".into()
            }
            Self::NavigateEnter => {
                if !pressed {
                    return None;
                }
                "{\"navigate\":\"enter\"}".into()
            }
            Self::NavigateBack => {
                if !pressed {
                    return None;
                }
                "{\"navigate\":\"back\"}".into()
            }
            Self::BrowseScroll => {
                // Rotary encoder. Most DJ browse knobs send "relative 1":
                //   value = 1   → one step clockwise (= down in lists)
                //   value = 127 → one step counter-clockwise (= up)
                //   value = 0 / 64 → idle / no-op
                // Some send small absolute deltas (2, 3) for fast spins;
                // those still fall in the >0..=63 range and get treated
                // as "down". Translate to navigate up/down.
                match value {
                    0 | 64 => return None,
                    65..=127 => "{\"navigate\":\"up\"}".into(),
                    _ => "{\"navigate\":\"down\"}".into(),
                }
            }
            Self::SwitchView(v) => {
                if !pressed {
                    return None;
                }
                // IPC accepts {"view_browse":1}, {"view_queue":1}, etc.
                // Map our view label to the right command name.
                let cmd = match v.as_str() {
                    "dashboard" => "dashboard",
                    "browse" => "view_browse",
                    "queue" => "view_queue",
                    "history" => "view_history",
                    "settings" => "view_settings",
                    other => other,
                };
                format!("{{\"{cmd}\":1}}")
            }
            Self::ResumeAuto => {
                if !pressed {
                    return None;
                }
                "{\"resume_auto\":1}".into()
            }
            Self::LoadDeck { is_a } => {
                if !pressed {
                    return None;
                }
                format!(
                    "{{\"load_deck\":{{\"deck\":\"{}\"}}}}",
                    if *is_a { "a" } else { "b" }
                )
            }
        })
    }
}

fn deck(is_a: bool) -> &'static str {
    if is_a { "A" } else { "B" }
}

/// User-editable map from MIDI event → action. Loaded from disk at
/// startup; writes go through `save()` after each change in MIDI-
/// learn mode.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MidiMap {
    /// Bindings keyed by `MidiEvent::label()` so the JSON file is
    /// human-readable. The runtime `HashMap<MidiEvent, Action>` is
    /// rebuilt on load — we don't want hash-map JSON serialization
    /// losing key structure.
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub event: MidiEvent,
    pub action: Action,
}

impl MidiMap {
    pub fn load() -> Self {
        std::fs::read_to_string(map_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(map_path(), json)
    }

    /// Find the action bound to a MIDI event, if any.
    pub fn lookup(&self, event: &MidiEvent) -> Option<&Action> {
        self.bindings
            .iter()
            .find(|b| &b.event == event)
            .map(|b| &b.action)
    }

    /// Reverse lookup: find the first MIDI event mapped to a given
    /// action. Used by the learn UI to show "this action is bound
    /// to CC ch=X #Y" alongside each row.
    pub fn event_for(&self, action: &Action) -> Option<&MidiEvent> {
        self.bindings
            .iter()
            .find(|b| &b.action == action)
            .map(|b| &b.event)
    }

    /// Replace any existing binding for `event` with the new action,
    /// or append if none exists. Saves to disk.
    pub fn bind(&mut self, event: MidiEvent, action: Action) {
        if let Some(slot) = self.bindings.iter_mut().find(|b| b.event == event) {
            slot.action = action;
        } else {
            self.bindings.push(Binding { event, action });
        }
        let _ = self.save();
    }

    /// Remove a binding by event. No-op if not present.
    pub fn unbind(&mut self, event: &MidiEvent) {
        self.bindings.retain(|b| &b.event != event);
        let _ = self.save();
    }
}

/// Shared state between the MIDI listener thread and the TUI. The
/// listener pushes the most-recent unmapped event here so the learn
/// UI can show "you just touched: CC ch1 #7". Bounded to the latest
/// event (we don't need history).
#[derive(Debug, Default)]
pub struct ListenerState {
    /// Current loaded mapping. The listener consults this on every
    /// event; the TUI updates it via `MidiMap::bind`.
    pub map: MidiMap,
    /// Last event seen, regardless of binding status. Used by the
    /// learn UI to show "current input." Cleared when consumed.
    pub last_event: Option<(MidiEvent, u32)>,
    /// Listener-active flag. While true, the dispatcher writes IPC
    /// commands; while false, events are observed but not dispatched
    /// (used in MIDI learn mode so binding events don't accidentally
    /// fire pre-existing actions).
    pub dispatch_active: bool,
}

/// Spawn the MIDI listener thread. Returns the shared state handle
/// the TUI uses to read recent events / update bindings. The thread
/// runs for the lifetime of the process; failures (no MIDI ports
/// available, midir init failed) log a warning and the function
/// returns a default state — mixr keeps running, MIDI just isn't
/// wired up.
pub fn spawn_listener() -> Arc<Mutex<ListenerState>> {
    let state = Arc::new(Mutex::new(ListenerState {
        map: MidiMap::load(),
        last_event: None,
        dispatch_active: true,
    }));
    let state_for_thread = state.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_listener(state_for_thread) {
            tracing::warn!("MIDI listener exited: {e}");
        }
    });
    state
}

fn run_listener(state: Arc<Mutex<ListenerState>>) -> anyhow::Result<()> {
    let input = midir::MidiInput::new("mixr-midi-in")?;
    let ports = input.ports();
    if ports.is_empty() {
        tracing::info!("MIDI: no input ports — listener idle");
        return Ok(());
    }

    // Open every connected port; users with multiple controllers
    // (Mixstream + nano-knobs + foot pedal) bind across all of them
    // through the same map.
    let mut connections = Vec::new();
    for (i, port) in ports.iter().enumerate() {
        let name = input.port_name(port).unwrap_or_else(|_| format!("port{i}"));
        tracing::info!("MIDI: opening {name}");
        let owned_input = midir::MidiInput::new(&format!("mixr-midi-{i}"))?;
        let state_clone = state.clone();
        let name_for_log = name.clone();
        let conn = owned_input
            .connect(
                port,
                "mixr",
                move |_ts, msg, _| {
                    handle_midi(msg, &state_clone, &name_for_log);
                },
                (),
            )
            .map_err(|e| anyhow::anyhow!("MIDI connect failed: {e}"))?;
        connections.push(conn);
    }

    // Park the thread; midir keeps the connections alive only while
    // their handles are in scope.
    loop {
        std::thread::park();
    }
}

fn handle_midi(msg: &[u8], state: &Arc<Mutex<ListenerState>>, source: &str) {
    let Some((event, value)) = parse(msg) else {
        return;
    };
    tracing::debug!("MIDI [{source}]: {} = {value}", event.label());

    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.last_event = Some((event.clone(), value));

    // Don't dispatch while in learn mode — user is binding, not driving.
    if !s.dispatch_active {
        return;
    }

    if let Some(action) = s.map.lookup(&event).cloned()
        && let Some(json) = action.to_ipc_command(value)
    {
        // Drop the lock before file I/O.
        drop(s);
        let path = dirs::home_dir()
            .unwrap_or_default()
            .join(".mixr")
            .join("command");
        if let Err(e) = std::fs::write(&path, &json) {
            tracing::warn!("MIDI: failed to write command file: {e}");
        }
    }
}

/// Decode a raw MIDI message into our (event, value) tuple. Status-
/// byte top nibble selects the message family; we handle CC, note-on,
/// note-off (treated as note-on velocity=0), and pitch bend.
fn parse(msg: &[u8]) -> Option<(MidiEvent, u32)> {
    if msg.len() < 2 {
        return None;
    }
    let status = msg[0];
    let channel = status & 0x0F;
    let kind = status & 0xF0;
    match kind {
        0xB0 if msg.len() >= 3 => Some((
            MidiEvent::ControlChange {
                channel,
                controller: msg[1],
            },
            msg[2] as u32,
        )),
        0x90 if msg.len() >= 3 => Some((
            MidiEvent::NoteOn {
                channel,
                note: msg[1],
            },
            msg[2] as u32, // velocity (0 = note-off in many controllers)
        )),
        0x80 if msg.len() >= 3 => Some((
            MidiEvent::NoteOn {
                channel,
                note: msg[1],
            },
            0, // explicit note-off → velocity 0
        )),
        0xE0 if msg.len() >= 3 => {
            let lsb = msg[1] as u32;
            let msb = msg[2] as u32;
            Some((MidiEvent::PitchBend { channel }, (msb << 7) | lsb))
        }
        _ => None,
    }
}

/// All actions for the MIDI learn picker. Order is suggestive — the
/// most-commonly-bound controls first so users find them fast.
pub fn all_actions() -> Vec<Action> {
    // Order: per-deck mixer first (most-used), then transport per-deck,
    // then load + cues, loops, transitions, UI nav. Globals (Play/Pause,
    // Mix Now, etc.) at the bottom — they're useful for "panic" or
    // "auto mix" buttons but per-deck is the default for hardware.
    let mut out = vec![
        // ── Audible mixer ──────────────────────────────────────────
        Action::Crossfader,
        Action::ChannelFader { is_a: true },
        Action::ChannelFader { is_a: false },
        Action::EqLow { is_a: true },
        Action::EqMid { is_a: true },
        Action::EqHigh { is_a: true },
        Action::EqLow { is_a: false },
        Action::EqMid { is_a: false },
        Action::EqHigh { is_a: false },
        Action::Filter { is_a: true },
        Action::Filter { is_a: false },
        Action::Tempo { is_a: true },
        Action::Tempo { is_a: false },
        // ── Per-deck transport ─────────────────────────────────────
        Action::PlayPauseDeck { is_a: true },
        Action::PlayPauseDeck { is_a: false },
        Action::JumpBarsDeck {
            is_a: true,
            bars: -8,
        },
        Action::JumpBarsDeck {
            is_a: true,
            bars: 8,
        },
        Action::JumpBarsDeck {
            is_a: false,
            bars: -8,
        },
        Action::JumpBarsDeck {
            is_a: false,
            bars: 8,
        },
        Action::NudgeDeck {
            is_a: true,
            direction: -1,
        },
        Action::NudgeDeck {
            is_a: true,
            direction: 1,
        },
        Action::NudgeDeck {
            is_a: false,
            direction: -1,
        },
        Action::NudgeDeck {
            is_a: false,
            direction: 1,
        },
        // ── Load (browse → deck) ───────────────────────────────────
        Action::LoadDeck { is_a: true },
        Action::LoadDeck { is_a: false },
    ];
    for slot in 0..4u8 {
        out.push(Action::Cue { is_a: true, slot });
        out.push(Action::Cue { is_a: false, slot });
    }
    for slot in 0..4u8 {
        out.push(Action::CueSet { is_a: true, slot });
        out.push(Action::CueSet { is_a: false, slot });
    }
    for &beats in &[1.0, 2.0, 4.0, 8.0, 16.0] {
        out.push(Action::LoopBeats { is_a: true, beats });
        out.push(Action::LoopBeats { is_a: false, beats });
    }
    for name in [
        "beatmatched",
        "echoout",
        "bassswap",
        "filtersweep",
        "looproll",
    ] {
        out.push(Action::Transition(name.into()));
    }
    // ── UI navigation ──────────────────────────────────────────────
    out.push(Action::BrowseScroll);
    out.push(Action::NavigateUp);
    out.push(Action::NavigateDown);
    out.push(Action::NavigateEnter);
    out.push(Action::NavigateBack);
    for view in ["dashboard", "browse", "queue", "history", "settings"] {
        out.push(Action::SwitchView(view.into()));
    }
    // ── Globals (less common — "panic" / "auto mix" / "trigger") ───
    // Per-deck variants above cover most controllers. These are kept
    // for special buttons: a single PLAY for "stop everything," an
    // AUTO MIX trigger, or hands-off mode toggles.
    out.push(Action::PlayPause);
    out.push(Action::MixNow);
    out.push(Action::ResumeAuto);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cc_message() {
        let msg = [0xB0, 0x07, 0x40]; // CC ch1 #7 value=64
        let (event, value) = parse(&msg).unwrap();
        assert_eq!(
            event,
            MidiEvent::ControlChange {
                channel: 0,
                controller: 7
            }
        );
        assert_eq!(value, 64);
    }

    #[test]
    fn parse_note_on_with_velocity() {
        let msg = [0x91, 0x3C, 0x70]; // Note ch2 #60 velocity=112
        let (event, value) = parse(&msg).unwrap();
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 1,
                note: 60
            }
        );
        assert_eq!(value, 112);
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        let msg = [0x90, 0x3C, 0x00]; // Note-on with velocity 0 = release
        let (_event, value) = parse(&msg).unwrap();
        assert_eq!(value, 0);
    }

    #[test]
    fn parse_pitch_bend_full_range() {
        let msg = [0xE0, 0x00, 0x40]; // pitch bend center (0x2000 = 8192)
        let (event, value) = parse(&msg).unwrap();
        assert_eq!(event, MidiEvent::PitchBend { channel: 0 });
        assert_eq!(value, 8192);
    }

    #[test]
    fn malformed_messages_return_none() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0xB0]).is_none());
        assert!(parse(&[0xB0, 0x07]).is_none()); // CC needs 3 bytes
    }

    #[test]
    fn map_round_trips_via_json() {
        let mut map = MidiMap::default();
        map.bindings.push(Binding {
            event: MidiEvent::ControlChange {
                channel: 0,
                controller: 7,
            },
            action: Action::Crossfader,
        });
        let json = serde_json::to_string(&map).unwrap();
        let back: MidiMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bindings.len(), 1);
        assert_eq!(back.bindings[0].action, Action::Crossfader);
    }

    #[test]
    fn lookup_finds_binding_after_bind() {
        let mut map = MidiMap::default();
        let event = MidiEvent::NoteOn {
            channel: 0,
            note: 60,
        };
        assert!(map.lookup(&event).is_none());
        // Bind without saving (avoid touching disk in unit tests).
        map.bindings.push(Binding {
            event: event.clone(),
            action: Action::PlayPause,
        });
        assert_eq!(map.lookup(&event), Some(&Action::PlayPause));
    }

    #[test]
    fn rebind_replaces_existing_action() {
        let mut map = MidiMap::default();
        let event = MidiEvent::NoteOn {
            channel: 0,
            note: 60,
        };
        map.bindings.push(Binding {
            event: event.clone(),
            action: Action::PlayPause,
        });
        // Manual rebind to avoid disk I/O.
        if let Some(slot) = map.bindings.iter_mut().find(|b| b.event == event) {
            slot.action = Action::Skip;
        }
        assert_eq!(map.lookup(&event), Some(&Action::Skip));
        assert_eq!(map.bindings.len(), 1, "rebind must replace, not append");
    }

    #[test]
    fn cc_to_crossfader_command() {
        let cmd = Action::Crossfader.to_ipc_command(64).unwrap();
        // 64/127 ≈ 0.504, then *2-1 ≈ 0.008 → near center
        assert!(cmd.contains("crossfader"));
    }

    #[test]
    fn note_on_press_dispatches() {
        // Velocity > 0 = press → should produce a command
        assert!(Action::PlayPause.to_ipc_command(64).is_some());
    }

    #[test]
    fn note_off_does_not_dispatch_press_actions() {
        // Velocity 0 = release → press-only actions return None
        assert!(Action::PlayPause.to_ipc_command(0).is_none());
        assert!(Action::Skip.to_ipc_command(0).is_none());
    }

    #[test]
    fn all_actions_includes_basics() {
        let actions = all_actions();
        assert!(actions.contains(&Action::Crossfader));
        assert!(actions.contains(&Action::PlayPause));
        assert!(actions.contains(&Action::ChannelFader { is_a: true }));
    }

    #[test]
    fn numark_mixstream_preset_parses() {
        // Locks the JSON schema for shipped presets — if MidiMap or
        // MidiEvent serialization changes shape, this catches it
        // before users complain that their preset stopped loading.
        let json = include_str!("../presets/numark-mixstream-pro.midi-map.json");
        let map: MidiMap = serde_json::from_str(json)
            .expect("Mixstream preset must parse against the current schema");
        assert!(!map.bindings.is_empty(), "preset must contain bindings");
        // Sanity-check the crossfader binding: CC ch15 #14 → Crossfader.
        let cf = map
            .bindings
            .iter()
            .find(|b| {
                matches!(
                    b.event,
                    MidiEvent::ControlChange {
                        channel: 15,
                        controller: 14
                    }
                )
            })
            .expect("crossfader binding present");
        assert_eq!(cf.action, Action::Crossfader);
    }
}
