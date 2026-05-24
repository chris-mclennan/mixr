//! File-based IPC: command input and status output.
//! Commands: write JSON to ~/.mixr/command (polled every tick)
//! Status: ~/.mixr/status.json (written every 2s)

use std::path::PathBuf;
use std::time::Instant;

fn mixr_dir() -> PathBuf {
    let dir = dirs::home_dir().unwrap_or_default().join(".mixr");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn command_path() -> PathBuf {
    mixr_dir().join("command")
}
fn status_path() -> PathBuf {
    mixr_dir().join("status.json")
}
fn screen_path() -> PathBuf {
    mixr_dir().join("screen.txt")
}
fn events_path() -> PathBuf {
    mixr_dir().join("events.jsonl")
}

/// Convert a shorthand command line ("queue 12345", "vol 0.8",
/// "tx echoout", or just "skip") into the JSON envelope mixr's IPC
/// parser expects. Used by both the CLI (`mixr --command`) and the
/// in-app `:` prompt so the two entry points have identical
/// semantics.
///
/// Value-type inference is best-effort:
///   - empty value → `1` (matches the legacy `--command skip` form)
///   - parses as i64 → integer
///   - parses as f64 → float
///   - "true"/"false" → bool
///   - anything else → string
///
/// Anything starting with `{` is assumed to be raw JSON and the
/// caller should pass it through unchanged — this helper is only
/// for the shorthand path.
pub fn shorthand_to_json(line: &str) -> String {
    let trimmed = line.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let key = parts.next().unwrap_or("").trim();
    let val = parts.next().unwrap_or("").trim();
    if key.is_empty() {
        return "{}".into();
    }
    if val.is_empty() {
        return format!("{{\"{key}\":1}}");
    }
    if let Ok(n) = val.parse::<i64>() {
        return format!("{{\"{key}\":{n}}}");
    }
    if let Ok(f) = val.parse::<f64>() {
        return format!("{{\"{key}\":{f}}}");
    }
    if val == "true" || val == "false" {
        return format!("{{\"{key}\":{val}}}");
    }
    format!("{{\"{key}\":{}}}", serde_json::Value::String(val.into()))
}

/// Append one structured event line to ~/.mixr/events.jsonl.
///
/// Generic structured event log for any external observer (custom
/// scrobblers, analytics tools, archival helpers, monitoring scripts).
/// Each line is one JSON object with `ts` (Unix seconds, float) plus
/// arbitrary event-specific fields. Stays a strictly append-only file
/// — `tail -f` works, no rotation, no parsing of historical state.
///
/// Best-effort writer: failures are silently swallowed. Disk errors
/// here must not affect the main TUI/audio loop.
pub fn write_event(event: &serde_json::Value) {
    use std::io::Write;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let mut line = serde_json::json!({ "ts": ts });
    if let (Some(obj_l), Some(obj_r)) = (line.as_object_mut(), event.as_object()) {
        for (k, v) in obj_r {
            obj_l.insert(k.clone(), v.clone());
        }
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(events_path())
    else {
        return;
    };
    let _ = writeln!(f, "{line}");
}

/// Write a text dump of the current screen content.
pub fn write_screen_dump(_title: &str, breadcrumb: &str, items: &[String], selected: usize) {
    let mut lines = Vec::new();
    lines.push(breadcrumb.to_string());
    lines.push(format!("┌{}┐", "─".repeat(78)));
    for (i, item) in items.iter().enumerate() {
        let marker = if i == selected { "▸" } else { " " };
        lines.push(format!("│{marker} {item}"));
    }
    lines.push(format!("└{}┘", "─".repeat(78)));
    std::fs::write(screen_path(), lines.join("\n")).ok();
}

/// Write arbitrary lines to screen dump (for dashboard, settings, etc.)
pub fn write_screen_lines(title: &str, lines: &[String]) {
    let mut out = vec![title.to_string()];
    out.extend(lines.iter().cloned());
    std::fs::write(screen_path(), out.join("\n")).ok();
}

/// Write quick status text (compact, no formatting — for fast reads).
pub fn write_quick_status(info: &crate::audio::engine::NowPlayingInfo, view: &str) {
    let playing = info
        .playing_track
        .as_ref()
        .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
        .unwrap_or("—".into());
    let incoming = info
        .incoming_track
        .as_ref()
        .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
        .unwrap_or("—".into());
    // Whether the playing-role deck is actually producing audio. A
    // track can be cued (`playing` above carries its name) without the
    // deck running — external readers (mnml's now-playing chip) key
    // off this so they don't mistake "loaded" for "playing".
    let playing_active = if info.playing_is_a {
        info.deck_a_is_playing
    } else {
        info.deck_b_is_playing
    };
    let lines = format!(
        "view={view}\nstate={:?}\nplaying={playing}\nplaying_active={playing_active}\nplaying_bpm={}\nplaying_time={:.0}/{:.0}\nincoming={incoming}\nincoming_bpm={}\nphase={:+.1}ms\ncrossfade={:.0}%\nqueue={}\nhistory={}",
        info.state,
        info.playing_bpm
            .map(|b| format!("{:.1}", b))
            .unwrap_or("—".into()),
        info.playing_time,
        info.playing_duration,
        info.incoming_bpm
            .map(|b| format!("{:.1}", b))
            .unwrap_or("—".into()),
        info.phase_offset_ms,
        info.crossfade_progress * 100.0,
        info.queue.len(),
        info.history.len(),
    );
    std::fs::write(mixr_dir().join("quick.txt"), lines).ok();
}

/// Read and delete the command file. Returns parsed JSON if present.
/// Drain queued IPC commands. The command file is a newline-delimited
/// list of JSON objects — writers append with `>>` and a trailing
/// newline; the smoke harness (and any external tool) can also `>`-
/// overwrite a single object as before.
///
/// Race-free: we atomically rename the command file out of the way
/// before reading it. A writer that fires between rename and read
/// creates a *new* `command` file, which the next tick will pick up
/// — no lost messages.
pub fn read_command() -> Vec<serde_json::Value> {
    let path = command_path();
    let tmp = path.with_extension("processing");
    if std::fs::rename(&path, &tmp).is_err() {
        return Vec::new();
    }
    let data = std::fs::read_to_string(&tmp).unwrap_or_default();
    std::fs::remove_file(&tmp).ok();
    if data.trim().is_empty() {
        return Vec::new();
    }
    data.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Write status.json with current state.
pub fn write_status_with_screen(
    info: &crate::audio::engine::NowPlayingInfo,
    screen_title: &str,
    screen_items: &[String],
    toast: Option<&str>,
    dash_section: Option<&str>,
    dash_focus: Option<&str>,
) {
    write_status_inner(
        info,
        Some(screen_title),
        Some(screen_items),
        toast,
        dash_section,
        dash_focus,
    );
}

fn write_status_inner(
    info: &crate::audio::engine::NowPlayingInfo,
    screen_title: Option<&str>,
    screen_items: Option<&[String]>,
    toast: Option<&str>,
    dash_section: Option<&str>,
    dash_focus: Option<&str>,
) {
    let track = info
        .playing_track
        .as_ref()
        .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
        .unwrap_or_default();

    let bpm = info.playing_bpm.unwrap_or(0.0);
    let time_remaining = info.playing_duration - info.playing_time;
    let state = format!("{:?}", info.state);

    let queue: Vec<serde_json::Value> = info
        .queue
        .iter()
        .map(|e| {
            serde_json::json!({
                "track": format!("{} - {}", e.track.artist_name(), e.track.full_title()),
                "bpm": e.track.bpm,
            })
        })
        .collect();

    // Per-deck mixer/tone-stack state so smoke tests and external
    // scripts can observe EQ / filter / cue state without scraping
    // screen.txt. Kept under a `decks` object so the wire format
    // extends cleanly.
    let deck_a = serde_json::json!({
        "isPlaying": info.deck_a_is_playing,
        "bpm": info.deck_a_bpm,
        "eqLowDb": info.deck_a_eq_low_db,
        "eqMidDb": info.deck_a_eq_mid_db,
        "eqHighDb": info.deck_a_eq_high_db,
        "filterPos": info.deck_a_filter_pos,
        "loopActive": info.deck_a_loop_active,
        "cues": info.deck_a_cues,
    });
    let deck_b = serde_json::json!({
        "isPlaying": info.deck_b_is_playing,
        "bpm": info.deck_b_bpm,
        "eqLowDb": info.deck_b_eq_low_db,
        "eqMidDb": info.deck_b_eq_mid_db,
        "eqHighDb": info.deck_b_eq_high_db,
        "filterPos": info.deck_b_filter_pos,
        "loopActive": info.deck_b_loop_active,
        "cues": info.deck_b_cues,
    });

    let status = serde_json::json!({
        "track": track,
        "bpm": bpm,
        "time": format!("{:.0}", info.playing_time),
        "duration": format!("{:.0}", info.playing_duration),
        "timeRemaining": format!("{:.0}", time_remaining),
        "state": state,
        "queueCount": info.queue.len(),
        "historyCount": info.history.len(),
        "crossfadeProgress": info.crossfade_progress,
        "phaseOffsetMs": info.phase_offset_ms,
        "queue": queue,
        "screen": screen_title.unwrap_or(""),
        "screenItems": screen_items.unwrap_or(&[]),
        "toast": toast.unwrap_or(""),
        "deckA": deck_a,
        "deckB": deck_b,
        "crossfaderPos": info.crossfader_pos,
        "channelFaderA": info.channel_fader_a,
        "channelFaderB": info.channel_fader_b,
        "transitionType": info.transition_type_name,
        "dashSection": dash_section.unwrap_or(""),
        "dashFocus": dash_focus.unwrap_or(""),
    });

    if let Ok(json) = serde_json::to_string_pretty(&status) {
        std::fs::write(status_path(), json).ok();
    }
}

/// Process a command JSON object. Returns actions to apply.
pub enum IpcCommand {
    Skip,
    Pause,
    Teleport,
    MixNow,
    ClearQueue,
    Nudge(i32),
    /// Re-enable auto-mix triggering after a user override. No-op if
    /// auto wasn't paused. Toast confirms.
    ResumeAuto,
    /// Load the currently-highlighted browse-list track to a specific
    /// deck. App-side handler resolves the selection + deck routing.
    LoadDeck {
        is_a: bool,
    },
    /// Per-deck nudge — like `Nudge` but always targets a specific deck
    /// regardless of mix state. Used by per-deck pitch-bend buttons on
    /// hardware controllers.
    NudgeDeck {
        is_a: bool,
        direction: i32,
    },
    /// Per-deck play/pause toggle. Independent of `Pause`, which only
    /// affects the playing deck.
    PlayDeck {
        is_a: bool,
    },
    /// Per-deck bar jump. `Jump(bars)` targets the playing deck;
    /// this targets a specific deck.
    JumpDeck {
        is_a: bool,
        bars: i32,
    },
    /// Per-deck hot-cue jump. Slot 0..3.
    CueJump {
        is_a: bool,
        slot: u8,
    },
    /// Per-deck hot-cue set at current playhead. Slot 0..3.
    CueSet {
        is_a: bool,
        slot: u8,
    },
    Metronome,
    SplitCue,
    Search(String),
    Browse(String),
    Navigate(String),
    QueueAll,
    Shuffle,
    SetQuality(String),
    SetCrossfade(u32),
    Status,
    Restart,
    ViewDashboard,
    ViewBrowse,
    ViewQueue,
    ViewHistory,
    ViewHelp,
    ViewSettings,
    WaveformMode,
    Jump(i32),
    ShiftGrid(f64),
    Extend(i32),
    /// Tempo rate setter. `deck = None` → operate on incoming (legacy
    /// Claude DJ flow); `deck = Some(is_a)` → set the named deck's
    /// rate directly (MIDI tempo faders, smoke tests, manual tweaks).
    SetRate {
        deck: Option<bool>,
        rate: f64,
    },
    Volume {
        playing: Option<f64>,
        incoming: Option<f64>,
    },
    SetMixIn(f64),
    Diagnose,
    /// Simulate a keyboard event. Single-char strings produce a `Char(c)`
    /// event; named strings ("up", "down", "left", "right", "enter",
    /// "esc", "tab", "backspace", "pageup", "pagedown") produce the
    /// corresponding `KeyCode` variant. Lets smoke tests drive
    /// non-printable keys (arrow nav in settings, Esc-to-close overlays,
    /// etc.) that single-char SimulateKey couldn't reach.
    SimulateKeyNamed(String),
    SimulateKey(char),
    ExportHistory,
    SmartShuffle,
    Favorite,
    Filter(String),
    GetScreen,
    QueueTrack(i64),
    /// Set EQ bands on a specific physical deck. None = unchanged.
    SetEq {
        is_a: bool,
        low: Option<f32>,
        mid: Option<f32>,
        high: Option<f32>,
    },
    /// Filter sweep on a specific physical deck (pos in [-1, +1]).
    SetDeckFilter {
        is_a: bool,
        pos: f32,
    },
    /// Channel fader per physical deck (0..1).
    SetChannelFader {
        is_a: bool,
        level: f32,
    },
    /// Mixer-wide crossfader (-1 = full A, 0 = center, +1 = full B).
    SetCrossfader(f32),
    /// Override transition type for the next crossfade.
    SetTransition(String),
    /// Beat-aligned loop on a specific deck.
    LoopBeats {
        is_a: bool,
        beats: f64,
    },
    /// Release any loop on a specific deck.
    LoopRelease {
        is_a: bool,
    },
    /// Set, jump, or clear a hot cue slot (0..=3) on a specific deck.
    Cue {
        is_a: bool,
        slot: usize,
        action: CueAction,
    },
    /// Switch the pitch-stretch engine globally. Accepts "off" or "rubberband".
    PitchStretch(String),
    /// Set the headphone-cue monitor device by name. Empty string disables.
    /// Unknown names are silently accepted (saved to config) but the engine
    /// logs and skips stream creation — matches the settings-UI picker.
    MonitorDevice(String),
    /// Set the local audio library directory. Empty string = disable.
    /// Triggers a re-scan + refresh of the root browse menu.
    LocalLibraryDir(String),
    /// Set the rekordbox.xml export path. Empty string = disable.
    /// Triggers root menu rebuild so the "Rekordbox" entry appears.
    RekordboxXml(String),
    /// Set the Engine DJ database (`m.db`) path. Empty = disable.
    /// Triggers root menu rebuild.
    EngineDjDb(String),
    /// Set the Serato `database V2` path. Empty = disable.
    SeratoDb(String),
    /// Turn the audio-callback profiler on or off at runtime. Off by default
    /// so the RT thread doesn't pay for timing syscalls. Accepts numeric
    /// (0/1), bool, or "on"/"off"/"toggle".
    Profile(Option<bool>), // None = toggle, Some(true/false) = set
    /// Master output gain (0.0..1.5). Clipping ceiling stays at ±1.0.
    MasterGain(f32),
    /// Deterministic test harness: navigate to Global Top 10, queue all,
    /// wait until both decks are loaded with different tracks, then teleport
    /// so a crossfade fires immediately. Zero manual navigation required.
    TestMix,
    /// Run `brew install rubberband`, then `cargo build --release --features
    /// rubberband`, then restart into the new binary. macOS only.
    InstallRubberband,
    /// Create a new empty Beatport playlist by name. Surfaces the new
    /// playlist id via a toast ("Playlist created: id=N name='…'") so
    /// scripts can capture it for follow-up API calls.
    PlaylistCreate(String),
    /// Delete a Beatport playlist by id. Idempotent — 404 from the
    /// remote is treated as success so cleanup can be repeated safely.
    /// Only emitted by parser when the caller passed
    /// `{"playlist_delete":{"id":N,"confirm":true}}` — bare-number
    /// shape gets rejected via `PlaylistDeleteRequest` first.
    PlaylistDelete(i64),
    /// Delete-playlist call without explicit confirmation. Surfaces a
    /// toast prompting the caller to repeat with `confirm:true`. No
    /// network call. Stops accidental scripts from wiping playlists.
    PlaylistDeleteRequest(i64),
    /// Update one or more Claude DJ behavior knobs. Any subset of keys
    /// under `claudedj` may be present; unknown keys are ignored. Maps
    /// to `ClaudeDjSettings` in config.rs.
    ClaudeDjSettings(serde_json::Value),
    /// Rate the most-recent crossfade. `true` = good, `false` = bad.
    /// Appends an entry to ~/.mixr/dj_memory.json so Claude carries
    /// the lesson into future sessions. No-op if no mix has happened
    /// yet this session.
    RateMix(bool),
    /// Synthesize a left-click at terminal (col, row). Used by smoke
    /// tests to verify click hit-targets are wired correctly without
    /// a real mouse. Modifiers carry SHIFT-state for hot-cue set.
    Click {
        col: u16,
        row: u16,
        shift: bool,
    },
    /// Synthesize a mouse-drag event at terminal (col, row). Hits the
    /// same code path a real `Drag(Left)` event takes — used by
    /// smoke tests to exercise continuous-drag targets (crossfader,
    /// tempo/volume/EQ/filter strips). Caller typically emits one
    /// Click to "start" the drag, then one or more Drag events to
    /// simulate the cursor moving under a held button.
    Drag {
        col: u16,
        row: u16,
    },
    /// Dump the current frame's labeled click targets to
    /// `~/.mixr/layout.json`. Smoke tests use this to look up
    /// control rects (tempoA, crossfader, play_a, etc.) without
    /// hardcoding coords that would drift with terminal size.
    LayoutDump,
    /// Toggle CDJ-style quantize on/off + beat resolution. Used by
    /// smoke tests to disable quantize so loop / jump clicks fire
    /// immediately instead of waiting for the next boundary.
    /// `beats` accepts 0.125 / 0.25 / 0.5 / 1 / 2 / 4 / 8.
    Quantize {
        on: bool,
        beats: f64,
    },
    /// Set delay feedback on a specific deck (0.0..1.0).
    DelayFeedback {
        is_a: bool,
        value: f32,
    },
    /// Set delay time in samples on a specific deck.
    DelaySamples {
        is_a: bool,
        value: usize,
    },
    /// Set delay time synced to BPM (beat_fraction, e.g. 0.75 = dotted eighth).
    DelaySync {
        is_a: bool,
        beat_fraction: f64,
    },
    /// Set loop in-point at current position on a specific deck.
    LoopIn {
        is_a: bool,
    },
    /// Set loop out-point at current position on a specific deck.
    LoopOut {
        is_a: bool,
    },
    /// Stop a specific deck.
    StopDeck {
        is_a: bool,
    },
    /// Seek a specific deck to a time in seconds.
    SeekDeck {
        is_a: bool,
        time: f64,
    },
    /// Switch the monitor headphone-cue source at runtime.
    /// "incoming", "playing" (maps to role-based deck), "both", "a", "b".
    MonitorSource(String),
}

#[derive(Debug, Clone, Copy)]
pub enum CueAction {
    Set,
    Jump,
    Clear,
}

pub fn parse_command(json: &serde_json::Value) -> Vec<IpcCommand> {
    let mut cmds = Vec::new();
    if let Some(obj) = json.as_object() {
        for (key, val) in obj {
            let str_val = val.as_str().unwrap_or("").to_string();
            match key.as_str() {
                "skip" => cmds.push(IpcCommand::Skip),
                "pause" => cmds.push(IpcCommand::Pause),
                "teleport" => cmds.push(IpcCommand::Teleport),
                "mixnow" => cmds.push(IpcCommand::MixNow),
                "clear" => cmds.push(IpcCommand::ClearQueue),
                "shuffle" => cmds.push(IpcCommand::Shuffle),
                "nudge" => {
                    // Two shapes:
                    //   {"nudge": 1}                              → global (legacy)
                    //   {"nudge": {"deck":"a","direction": 1}}   → per-deck
                    if let Some(dir) = val.as_i64() {
                        cmds.push(IpcCommand::Nudge(if dir >= 0 { 1 } else { -1 }));
                    } else if let Some(obj) = val.as_object() {
                        let dir = obj.get("direction").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left"))
                            .unwrap_or(true);
                        cmds.push(IpcCommand::NudgeDeck {
                            is_a,
                            direction: if dir >= 0 { 1 } else { -1 },
                        });
                    }
                }
                "play_deck" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left"))
                            .unwrap_or(true);
                        cmds.push(IpcCommand::PlayDeck { is_a });
                    }
                }
                "cue" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left"))
                            .unwrap_or(true);
                        let slot = obj.get("slot").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                        cmds.push(IpcCommand::CueJump {
                            is_a,
                            slot: slot.min(3),
                        });
                    }
                }
                "cue_set" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left"))
                            .unwrap_or(true);
                        let slot = obj.get("slot").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                        cmds.push(IpcCommand::CueSet {
                            is_a,
                            slot: slot.min(3),
                        });
                    }
                }
                "metronome" => cmds.push(IpcCommand::Metronome),
                "resume_auto" | "auto" => cmds.push(IpcCommand::ResumeAuto),
                "load_deck" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left"))
                            .unwrap_or(true);
                        cmds.push(IpcCommand::LoadDeck { is_a });
                    }
                }
                "splitcue" => cmds.push(IpcCommand::SplitCue),
                "search" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::Search(str_val));
                    }
                }
                "browse" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::Browse(str_val));
                    }
                }
                "queueall" => cmds.push(IpcCommand::QueueAll),
                "navigate" => {
                    if str_val == "queueall" {
                        cmds.push(IpcCommand::QueueAll);
                    } else {
                        cmds.push(IpcCommand::Navigate(str_val));
                    }
                }
                "quality" => cmds.push(IpcCommand::SetQuality(str_val)),
                "crossfade" => {
                    if let Some(bars) = val.as_u64() {
                        cmds.push(IpcCommand::SetCrossfade(bars as u32));
                    }
                }
                "status" => cmds.push(IpcCommand::Status),
                "restart" => cmds.push(IpcCommand::Restart),
                "dashboard" | "view_dashboard" => cmds.push(IpcCommand::ViewDashboard),
                "view_browse" => cmds.push(IpcCommand::ViewBrowse),
                "view_queue" => cmds.push(IpcCommand::ViewQueue),
                "view_history" => cmds.push(IpcCommand::ViewHistory),
                "view_help" => cmds.push(IpcCommand::ViewHelp),
                "view_settings" => cmds.push(IpcCommand::ViewSettings),
                "waveform" => cmds.push(IpcCommand::WaveformMode),
                "jump" => {
                    // Two shapes:
                    //   {"jump": 4}                          → playing deck (legacy)
                    //   {"jump": {"deck":"a","bars": 4}}    → per-deck
                    if let Some(bars) = val.as_i64() {
                        cmds.push(IpcCommand::Jump(bars as i32));
                    } else if let Some(obj) = val.as_object() {
                        let bars = obj.get("bars").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left"))
                            .unwrap_or(true);
                        cmds.push(IpcCommand::JumpDeck { is_a, bars });
                    }
                }
                "shiftgrid" => {
                    if let Some(ms) = val.as_f64() {
                        cmds.push(IpcCommand::ShiftGrid(ms));
                    } else if let Some(ms) = val.as_i64() {
                        cmds.push(IpcCommand::ShiftGrid(ms as f64));
                    }
                }
                "extend" => {
                    if let Some(bars) = val.as_i64() {
                        cmds.push(IpcCommand::Extend(bars as i32));
                    }
                }
                "setrate" => {
                    // Two shapes:
                    //   {"setrate": 1.05}                       → legacy, targets incoming
                    //   {"setrate": {"deck":"a","rate":1.05}}   → per-deck
                    if let Some(r) = val.as_f64() {
                        cmds.push(IpcCommand::SetRate {
                            deck: None,
                            rate: r,
                        });
                    } else if let Some(obj) = val.as_object() {
                        if let Some(rate) = obj.get("rate").and_then(|v| v.as_f64()) {
                            let deck = obj.get("deck").and_then(|v| v.as_str()).map(|s| {
                                matches!(s.to_ascii_lowercase().as_str(), "a" | "0" | "left")
                            });
                            cmds.push(IpcCommand::SetRate { deck, rate });
                        }
                    }
                }
                "volume" => {
                    let playing = val.get("playing").and_then(|v| v.as_f64());
                    let incoming = val.get("incoming").and_then(|v| v.as_f64());
                    cmds.push(IpcCommand::Volume { playing, incoming });
                }
                "setmixin" => {
                    if let Some(t) = val.as_f64() {
                        cmds.push(IpcCommand::SetMixIn(t));
                    }
                }
                "diagnose" => cmds.push(IpcCommand::Diagnose),
                "key" => {
                    // Single-char → SimulateKey(c); multi-char → named.
                    let n = str_val.chars().count();
                    if n == 1 {
                        let c = str_val.chars().next().unwrap();
                        cmds.push(IpcCommand::SimulateKey(c));
                    } else if n > 1 {
                        cmds.push(IpcCommand::SimulateKeyNamed(str_val));
                    }
                }
                "export" => cmds.push(IpcCommand::ExportHistory),
                "smart_shuffle" => cmds.push(IpcCommand::SmartShuffle),
                "favorite" => cmds.push(IpcCommand::Favorite),
                "filter" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::Filter(str_val));
                    }
                }
                "get_screen" => cmds.push(IpcCommand::GetScreen),
                "queue_track" => {
                    if let Some(id) = val.as_i64() {
                        cmds.push(IpcCommand::QueueTrack(id));
                    }
                }
                "eq" => {
                    // {"eq": {"deck": "a", "low": -6, "mid": 0, "high": 3}}
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        let low = obj.get("low").and_then(|v| v.as_f64()).map(|v| v as f32);
                        let mid = obj.get("mid").and_then(|v| v.as_f64()).map(|v| v as f32);
                        let high = obj.get("high").and_then(|v| v.as_f64()).map(|v| v as f32);
                        cmds.push(IpcCommand::SetEq {
                            is_a,
                            low,
                            mid,
                            high,
                        });
                    }
                }
                "deck_filter" => {
                    // {"deck_filter": {"deck": "a", "pos": -0.5}}
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        if let Some(pos) = obj.get("pos").and_then(|v| v.as_f64()) {
                            cmds.push(IpcCommand::SetDeckFilter {
                                is_a,
                                pos: pos as f32,
                            });
                        }
                    }
                }
                "fader" => {
                    // {"fader": {"a": 0.8, "b": 1.0}} — any subset
                    if let Some(obj) = val.as_object() {
                        if let Some(l) = obj.get("a").and_then(|v| v.as_f64()) {
                            cmds.push(IpcCommand::SetChannelFader {
                                is_a: true,
                                level: l as f32,
                            });
                        }
                        if let Some(l) = obj.get("b").and_then(|v| v.as_f64()) {
                            cmds.push(IpcCommand::SetChannelFader {
                                is_a: false,
                                level: l as f32,
                            });
                        }
                    }
                }
                "crossfader" => {
                    if let Some(p) = val.as_f64() {
                        cmds.push(IpcCommand::SetCrossfader(p as f32));
                    }
                }
                "transition" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::SetTransition(str_val));
                    }
                }
                "loop" => {
                    // {"loop": {"deck": "a", "beats": 4}} or {"loop": {"deck": "a", "release": true}}
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        if obj
                            .get("release")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        {
                            cmds.push(IpcCommand::LoopRelease { is_a });
                        } else if let Some(beats) = obj.get("beats").and_then(|v| v.as_f64()) {
                            cmds.push(IpcCommand::LoopBeats { is_a, beats });
                        }
                    }
                }
                "pitch_stretch" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::PitchStretch(str_val));
                    }
                }
                "monitor_device" => {
                    // Empty string is valid — it disables the monitor.
                    cmds.push(IpcCommand::MonitorDevice(str_val));
                }
                "local_library_dir" => {
                    // Empty string disables the local library entry.
                    cmds.push(IpcCommand::LocalLibraryDir(str_val));
                }
                "rekordbox_xml" => {
                    // Empty string disables the rekordbox menu entry.
                    cmds.push(IpcCommand::RekordboxXml(str_val));
                }
                "engine_dj_db" => {
                    cmds.push(IpcCommand::EngineDjDb(str_val));
                }
                "serato_db" => {
                    cmds.push(IpcCommand::SeratoDb(str_val));
                }
                "monitor_source" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::MonitorSource(str_val));
                    }
                }
                "playlist_create" => {
                    if !str_val.is_empty() {
                        cmds.push(IpcCommand::PlaylistCreate(str_val));
                    }
                }
                "playlist_delete" => {
                    // Two shapes accepted:
                    //   1. Bare number → soft-reject (toast asks for confirm).
                    //      Stops accidental {"playlist_delete":42} calls cold.
                    //   2. Object {"id":N,"confirm":true} → actually delete.
                    if let Some(id) = val.as_i64() {
                        cmds.push(IpcCommand::PlaylistDeleteRequest(id));
                    } else if let Some(obj) = val.as_object() {
                        let id = obj.get("id").and_then(|v| v.as_i64());
                        let confirmed = obj
                            .get("confirm")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Some(id) = id {
                            if confirmed {
                                cmds.push(IpcCommand::PlaylistDelete(id));
                            } else {
                                cmds.push(IpcCommand::PlaylistDeleteRequest(id));
                            }
                        }
                    }
                }
                "claudedj" => {
                    // Whole object forwarded — the handler merges it into
                    // the live settings so callers can flip one key or
                    // many at once.
                    cmds.push(IpcCommand::ClaudeDjSettings(val.clone()));
                }
                "click" => {
                    // {"click": {"col": 12, "row": 5, "shift": false}}
                    if let Some(obj) = val.as_object() {
                        let col = obj.get("col").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let row = obj.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let shift = obj.get("shift").and_then(|v| v.as_bool()).unwrap_or(false);
                        cmds.push(IpcCommand::Click { col, row, shift });
                    }
                }
                "drag" => {
                    // {"drag": {"col": 12, "row": 5}}
                    if let Some(obj) = val.as_object() {
                        let col = obj.get("col").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let row = obj.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        cmds.push(IpcCommand::Drag { col, row });
                    }
                }
                "layout" => {
                    // {"layout":1} — dump labeled click targets.
                    cmds.push(IpcCommand::LayoutDump);
                }
                "quantize" => {
                    // {"quantize":{"on":true,"beats":1}} or shorthand
                    // {"quantize":false} / {"quantize":true} for the
                    // toggle alone. Accepts "bars" as a back-compat
                    // alias (treated as bars × 4 = beats).
                    if let Some(b) = val.as_bool() {
                        cmds.push(IpcCommand::Quantize { on: b, beats: 1.0 });
                    } else if let Some(obj) = val.as_object() {
                        let on = obj.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
                        let beats = if let Some(b) = obj.get("beats").and_then(|v| v.as_f64()) {
                            b
                        } else if let Some(bars) = obj.get("bars").and_then(|v| v.as_f64()) {
                            bars * 4.0 // back-compat
                        } else {
                            1.0
                        };
                        cmds.push(IpcCommand::Quantize { on, beats });
                    }
                }
                "rate_mix" => {
                    // Accepts bool, "+"/"-"/"good"/"bad", or a number
                    // (>=0 good, <0 bad).
                    let good = match val {
                        serde_json::Value::Bool(b) => Some(*b),
                        serde_json::Value::Number(n) => n.as_f64().map(|f| f >= 0.0),
                        serde_json::Value::String(s) => match s.as_str() {
                            "+" | "good" | "up" => Some(true),
                            "-" | "bad" | "down" => Some(false),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(b) = good {
                        cmds.push(IpcCommand::RateMix(b));
                    }
                }
                "profile" => {
                    let on = match val {
                        serde_json::Value::Bool(b) => Some(*b),
                        serde_json::Value::Number(n) => n.as_i64().map(|v| v != 0),
                        serde_json::Value::String(s) => match s.to_ascii_lowercase().as_str() {
                            "on" | "1" | "true" | "start" => Some(true),
                            "off" | "0" | "false" | "stop" => Some(false),
                            "toggle" => None,
                            _ => None,
                        },
                        _ => None,
                    };
                    cmds.push(IpcCommand::Profile(on));
                }
                "master_gain" | "master" | "gain" => {
                    if let Some(g) = val.as_f64() {
                        cmds.push(IpcCommand::MasterGain(g as f32));
                    }
                }
                "test_mix" => {
                    cmds.push(IpcCommand::TestMix);
                }
                "install_rubberband" => {
                    cmds.push(IpcCommand::InstallRubberband);
                }
                // FIXME: this `"cue"` arm is shadowed by the simpler one
                // earlier at line 455 — anyone who sends an `action` field
                // gets ignored. The fix is small: delete lines 455-471 (the
                // simple `"cue"` and `"cue_set"` arms that emit CueJump /
                // CueSet variants), then fold their handlers in
                // tui/ipc_handler.rs:75-84 into the existing
                // `IpcCommand::Cue { action }` arm at ipc_handler.rs:581
                // (which already handles all three actions). Defaults stay
                // the same (no action ⇒ Jump). The CueJump / CueSet
                // IpcCommand variants get deleted along the way; the
                // unified Cue variant covers everything.
                #[allow(unreachable_patterns)]
                "cue" => {
                    // {"cue": {"deck": "a", "slot": 1, "action": "set|jump|clear"}}
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        let slot_num =
                            obj.get("slot").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                        let slot = slot_num.saturating_sub(1).min(3);
                        let action =
                            match obj.get("action").and_then(|v| v.as_str()).unwrap_or("jump") {
                                "set" => CueAction::Set,
                                "clear" => CueAction::Clear,
                                _ => CueAction::Jump,
                            };
                        cmds.push(IpcCommand::Cue { is_a, slot, action });
                    }
                }
                "delay_feedback" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        if let Some(v) = obj.get("value").and_then(|v| v.as_f64()) {
                            cmds.push(IpcCommand::DelayFeedback {
                                is_a,
                                value: v as f32,
                            });
                        }
                    }
                }
                "delay_samples" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        if let Some(v) = obj.get("value").and_then(|v| v.as_u64()) {
                            cmds.push(IpcCommand::DelaySamples {
                                is_a,
                                value: v as usize,
                            });
                        }
                    }
                }
                "delay_sync" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        if let Some(v) = obj.get("beat_fraction").and_then(|v| v.as_f64()) {
                            cmds.push(IpcCommand::DelaySync {
                                is_a,
                                beat_fraction: v,
                            });
                        }
                    }
                }
                "loop_in" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        cmds.push(IpcCommand::LoopIn { is_a });
                    } else {
                        cmds.push(IpcCommand::LoopIn { is_a: true });
                    }
                }
                "loop_out" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        cmds.push(IpcCommand::LoopOut { is_a });
                    } else {
                        cmds.push(IpcCommand::LoopOut { is_a: true });
                    }
                }
                "stop_deck" => {
                    let is_a = val
                        .as_object()
                        .and_then(|o| o.get("deck"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.eq_ignore_ascii_case("a"))
                        .unwrap_or(true);
                    cmds.push(IpcCommand::StopDeck { is_a });
                }
                "seek_deck" => {
                    if let Some(obj) = val.as_object() {
                        let is_a = obj
                            .get("deck")
                            .and_then(|v| v.as_str())
                            .map(|s| s.eq_ignore_ascii_case("a"))
                            .unwrap_or(true);
                        let time = obj.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        cmds.push(IpcCommand::SeekDeck { is_a, time });
                    }
                }
                _ => tracing::debug!("Unknown command: {key}"),
            }
        }
    }
    cmds
}

/// Status writer with rate limiting.
pub struct StatusWriter {
    last_write: Instant,
}

impl StatusWriter {
    pub fn new() -> Self {
        Self {
            last_write: Instant::now() - std::time::Duration::from_secs(10),
        }
    }

    /// True if the 2-second interval has elapsed and a write is pending.
    /// Callers can use this to skip building expensive inputs.
    pub fn needs_write(&self) -> bool {
        self.last_write.elapsed() >= std::time::Duration::from_secs(2)
    }

    pub fn maybe_write(
        &mut self,
        info: &crate::audio::engine::NowPlayingInfo,
        screen_title: &str,
        screen_items: &[String],
        toast: Option<&str>,
        dash_section: Option<&str>,
        dash_focus: Option<&str>,
    ) {
        if self.last_write.elapsed() >= std::time::Duration::from_secs(2) {
            write_status_with_screen(
                info,
                screen_title,
                screen_items,
                toast,
                dash_section,
                dash_focus,
            );
            self.last_write = Instant::now();
        }
    }
}

#[cfg(test)]
mod shorthand_tests {
    use super::shorthand_to_json;
    use serde_json::Value;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("must be valid JSON")
    }

    #[test]
    fn bare_keyword_becomes_one() {
        // `mixr --command skip` historically meant {"skip":1}; preserve.
        assert_eq!(parse(&shorthand_to_json("skip"))["skip"], 1);
        assert_eq!(parse(&shorthand_to_json("export"))["export"], 1);
    }

    #[test]
    fn integer_value() {
        assert_eq!(parse(&shorthand_to_json("queue 12345"))["queue"], 12345);
        assert_eq!(parse(&shorthand_to_json("crossfade 32"))["crossfade"], 32);
    }

    #[test]
    fn float_value() {
        assert_eq!(parse(&shorthand_to_json("vol 0.8"))["vol"], 0.8);
        assert_eq!(parse(&shorthand_to_json("setrate 1.05"))["setrate"], 1.05);
    }

    #[test]
    fn bool_value() {
        assert_eq!(parse(&shorthand_to_json("profile true"))["profile"], true);
        assert_eq!(parse(&shorthand_to_json("profile false"))["profile"], false);
    }

    #[test]
    fn string_value_with_special_chars_round_trips() {
        // String wrapping uses serde so quote-escaping is correct.
        let v = parse(&shorthand_to_json("tx echoout"));
        assert_eq!(v["tx"], "echoout");

        let v = parse(&shorthand_to_json("playlist_create \"my set\""));
        assert_eq!(v["playlist_create"], "\"my set\"");
    }

    #[test]
    fn empty_input_is_safe_empty_object() {
        // The CLI/prompt should reject empty before calling, but the
        // helper itself shouldn't panic.
        assert_eq!(shorthand_to_json(""), "{}");
        assert_eq!(shorthand_to_json("   "), "{}");
    }
}

#[cfg(test)]
mod playlist_delete_confirm_tests {
    use super::*;

    fn parse(json_str: &str) -> Vec<IpcCommand> {
        let v: serde_json::Value = serde_json::from_str(json_str).unwrap();
        parse_command(&v)
    }

    #[test]
    fn bare_id_is_unconfirmed_request() {
        // Old shape — accidentally writing `{"playlist_delete":42}` should
        // NOT trigger the destructive path. Surfaces as a Request that
        // the handler shows as a toast.
        let cmds = parse(r#"{"playlist_delete":42}"#);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], IpcCommand::PlaylistDeleteRequest(42)));
    }

    #[test]
    fn object_without_confirm_is_request() {
        let cmds = parse(r#"{"playlist_delete":{"id":42}}"#);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], IpcCommand::PlaylistDeleteRequest(42)));
    }

    #[test]
    fn object_with_confirm_false_is_request() {
        let cmds = parse(r#"{"playlist_delete":{"id":42,"confirm":false}}"#);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], IpcCommand::PlaylistDeleteRequest(42)));
    }

    #[test]
    fn object_with_confirm_true_actually_deletes() {
        let cmds = parse(r#"{"playlist_delete":{"id":42,"confirm":true}}"#);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], IpcCommand::PlaylistDelete(42)));
    }

    #[test]
    fn malformed_payload_emits_no_command() {
        // String payload, missing id, etc. — silent ignore (parser is
        // tolerant: we don't want one bad line to nuke a queued batch).
        assert!(parse(r#"{"playlist_delete":"oops"}"#).is_empty());
        assert!(parse(r#"{"playlist_delete":{"confirm":true}}"#).is_empty());
    }
}
