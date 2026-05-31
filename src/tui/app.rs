use anyhow::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::audio::engine::{EngineEvent, MixEngine, NowPlayingInfo};
use crate::beatport::api::BeatportAPI;
use crate::beatport::auth::StoredAuth;
use crate::beatport::catalog::{self, BrowseScreen, MenuAction};

/// Cap on how many track titles we feed into the DJ's session memory per
/// `queue_all`. Keeps the system-prompt context bounded even when the DJ
/// queues a full 100-track screen. The memory itself is FIFO of size
/// `MEMORY_LEN` in `claude/dj.rs`, so only the last few survive anyway.
pub(crate) const MEMORY_RECENT_CAP: usize = 8;
use super::dashboard::{self, WaveformMode};
use super::screens;
use super::toast::Toast;
use crate::beatport::models::BeatportTrack;
use crate::beatport::stream::StreamDownloader;
use crate::config::AppConfig;
use crate::favorites::FavoritesDB;

// AppAction flows through an unbounded mpsc channel. The TrackDecoded /
// PreviewReady variants carry Vec<f32> sample buffers + AnalysisResult,
// dwarfing Toast(String). Lint suggests boxing those, but they fire at
// low frequency (once per track decode/preview, ~minutes apart) and are
// consumed immediately by the next tick — buffer-then-drain pattern, no
// long-lived AppActions. Boxing would force a heap alloc on every channel
// send including the cheap Toast/PushScreen ones, costing more than it saves.
#[allow(clippy::large_enum_variant)]
pub enum AppAction {
    Toast(String),
    PushScreen(BrowseScreen),
    AppendTracks(Vec<BeatportTrack>),
    AppendCharts(Vec<crate::beatport::models::BeatportChart>),
    AppendReleases(Vec<crate::beatport::models::BeatportRelease>),
    TrackDecoded {
        track: BeatportTrack,
        samples: Vec<f32>,
        sample_rate: u32,
        analysis: crate::audio::analyzer::AnalysisResult,
        as_incoming: bool,
    },
    /// Track decoded for preview (plays 4 bars from first_beat with metronome).
    PreviewReady {
        samples: Vec<f32>,
        sample_rate: u32,
        analysis: crate::audio::analyzer::AnalysisResult,
    },
    DownloadFailed(String),
    DjToolCalls(Vec<crate::claude::api::ToolCall>),
    DjContinue(Vec<(String, String)>), // tool results to send back
    /// Open the playlist picker for the given track.
    ShowPlaylistPicker {
        track_id: i64,
        playlists: Vec<crate::beatport::models::BeatportChart>,
    },
    /// AI alignment result — carries nudge data so the main loop can apply it.
    AlignmentResult {
        nudge_ms: f64,
        is_aligned: bool,
        rate_correction: Option<f64>,
        details: String,
    },
    /// Open the genre picker. `favorites=true` toggles the favorites-picker variant.
    ShowGenrePicker {
        favorites: bool,
        genres: Vec<crate::beatport::models::BeatportGenre>,
    },
}

pub(crate) enum ViewMode {
    Browse,
    Search,
    Queue,
    History,
    Dashboard,
    Help,
    Settings,
    GenrePicker,       // picking default genre
    FavoritesPicker,   // toggling favorite genres
    PlaylistPicker,    // adding track to playlist
    PlaylistNameInput, // typing new playlist name
    Mixer,             // virtual mixer control overlay
    TransitionRules,   // rule editor overlay
    ClaudeDj,          // full Claude DJ status / log screen
    MidiLearn,         // MIDI controller mapping editor — see midi_learn.rs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MixerRow {
    EqLow,
    EqMid,
    EqHigh,
    Filter,
    Fader,
}
impl MixerRow {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::EqLow => Self::EqMid,
            Self::EqMid => Self::EqHigh,
            Self::EqHigh => Self::Filter,
            Self::Filter => Self::Fader,
            Self::Fader => Self::EqLow,
        }
    }
    pub(crate) fn prev(self) -> Self {
        match self {
            Self::EqLow => Self::Fader,
            Self::EqMid => Self::EqLow,
            Self::EqHigh => Self::EqMid,
            Self::Filter => Self::EqHigh,
            Self::Fader => Self::Filter,
        }
    }
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::EqLow => "EQ Low",
            Self::EqMid => "EQ Mid",
            Self::EqHigh => "EQ High",
            Self::Filter => "Filter",
            Self::Fader => "Fader",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashFocus {
    Controller,
    Queue,
    History,
    Browse,
    Log,
}

/// MixEntry alongside an optional rating. Lets us mark history
/// entries the user already gave feedback on so duplicates don't
/// pile up in DJ memory if they hit the rating key twice. The
/// `rated_at` timestamp captures the exact moment the rating was
/// saved — used to find-and-remove the corresponding DJ memory
/// entry on undo (so the same `+` press toggles off cleanly).
#[derive(Debug, Clone)]
pub struct HistoryMix {
    pub entry: crate::claude::memory::MixEntry,
    pub rated: Option<bool>,
    pub rated_at: Option<i64>,
}

use super::dashboard::CtrlSection;
impl DashFocus {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Controller => Self::Queue,
            Self::Queue => Self::History,
            Self::History => Self::Browse,
            Self::Browse => Self::Log,
            Self::Log => Self::Controller,
        }
    }
}

pub(crate) struct PlaylistPickerState {
    pub(crate) playlists: Vec<crate::beatport::models::BeatportChart>,
    pub(crate) track_id: i64,
    pub(crate) selected: usize,
    pub(crate) new_name: String,
}

pub(crate) struct GenrePickerState {
    pub(crate) genres: Vec<crate::beatport::models::BeatportGenre>,
    pub(crate) selected: usize,
    pub(crate) scroll_offset: usize,
}

/// Destructive actions that require an explicit Y/N confirmation
/// before they fire. New variants slot in here; the keys handler
/// looks up the variant when the user presses Y to commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmAction {
    /// Reset every mixer control across both decks (EQ, filter,
    /// channel faders, crossfader). Triggered from the Virtual Mixer
    /// overlay's `0` key.
    ResetAllMixerControls,
    /// Clear every track from the queue. Bound to `X`. Worth a
    /// confirmation — un-queueing a 50-track set by hand is painful.
    ClearQueue,
}

/// Routing decision for a key press while a confirmation is pending.
/// Pure — no `App` state required, so it can be unit-tested directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmDecision {
    /// User pressed Y/y — fire the pending action.
    Commit,
    /// User pressed N/n or Esc — drop the pending action.
    Cancel,
    /// Anything else — keep waiting; key is absorbed.
    Ignore,
}

pub(crate) fn route_confirm_key(code: crossterm::event::KeyCode) -> ConfirmDecision {
    use crossterm::event::KeyCode;
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => ConfirmDecision::Commit,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ConfirmDecision::Cancel,
        _ => ConfirmDecision::Ignore,
    }
}

#[cfg(test)]
mod confirm_tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn y_commits() {
        assert_eq!(
            route_confirm_key(KeyCode::Char('y')),
            ConfirmDecision::Commit
        );
        assert_eq!(
            route_confirm_key(KeyCode::Char('Y')),
            ConfirmDecision::Commit
        );
    }

    #[test]
    fn n_cancels() {
        assert_eq!(
            route_confirm_key(KeyCode::Char('n')),
            ConfirmDecision::Cancel
        );
        assert_eq!(
            route_confirm_key(KeyCode::Char('N')),
            ConfirmDecision::Cancel
        );
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(route_confirm_key(KeyCode::Esc), ConfirmDecision::Cancel);
    }

    #[test]
    fn other_keys_ignored() {
        // Ensures stray keystrokes during a confirm don't fire the action.
        for c in ['a', 'q', '0', ' ', '\t'] {
            assert_eq!(
                route_confirm_key(KeyCode::Char(c)),
                ConfirmDecision::Ignore,
                "key '{c}' must be ignored during pending confirm",
            );
        }
        assert_eq!(route_confirm_key(KeyCode::Enter), ConfirmDecision::Ignore);
    }

    #[test]
    fn confirm_action_is_copy() {
        let a = ConfirmAction::ResetAllMixerControls;
        let b = a; // Copy — would error if not.
        assert_eq!(a, b);
    }
}

pub struct App {
    pub config: AppConfig,
    pub engine: MixEngine,
    pub toast: Toast,
    pub waveform_mode: WaveformMode,
    pub(crate) status_writer: crate::ipc::StatusWriter,

    // Browse navigation — stack of screens
    pub(crate) screen_stack: Vec<BrowseScreen>,
    /// Saved cursor positions for parent screens. Pushed when drilling
    /// into a sub-screen so popping back restores where the user was.
    /// Each entry: (selected, scroll_offset, selected_column, dash_browse_sel).
    /// Parallel to screen_stack — len() = screen_stack.len() - 1
    /// (root screen has no saved parent state).
    pub(crate) screen_stack_back: Vec<(usize, usize, i32, usize)>,
    pub(crate) selected: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) selected_column: i32, // -2 = whole row, -1 = title, 0 = artist, 1 = remixer, 2 = label, 3 = genre, 4 = date

    // View mode overlays
    pub(crate) view_mode: ViewMode,

    // Dashboard state
    pub(crate) dash_focus: DashFocus,
    pub(crate) dash_browse_sel: usize, // selected item in mini browser
    pub(crate) dash_help: bool,        // show help legend on dashboard
    pub(crate) dash_section: CtrlSection, // selected interactive section
    /// Overall dashboard layout (Full vs Panel). Cycled via `v`.
    pub(crate) dash_layout: super::dashboard::DashLayout,
    /// In Panel layout, which secondary section is visible below the
    /// controller. Cycled via `v` together with `dash_layout`.
    pub(crate) dash_panel_section: super::dashboard::PanelSection,

    // Search
    pub(crate) search_query: String,
    pub(crate) search_results: Vec<BeatportTrack>,

    // `:` command-prompt overlay. `Some` when the prompt is open and
    // accepting input; `None` when closed. The user types a command
    // (e.g. `queue 12345`, `tx echoout`, `bpm 128`, or any raw JSON)
    // and Enter submits — see `submit_command_prompt` for the parser.
    pub(crate) command_prompt: Option<String>,

    /// Pending Y/N confirmation. `Some` when a destructive action is
    /// awaiting confirmation; the next Y/y commits it, N/n/Esc
    /// cancels. Other keys are absorbed so a stray keystroke can't
    /// accidentally confirm.
    pub(crate) pending_confirm: Option<ConfirmAction>,

    /// Active right-click → MIDI map flow. Set when the user right-
    /// clicks a bindable UI element. Tick loop watches the MIDI
    /// listener; first event observed populates `captured`. User
    /// confirms with Y/Enter or cancels with Esc.
    pub(crate) pending_midi_map: Option<PendingMidiMap>,

    /// Shared handle to the MIDI listener thread. None when MIDI is
    /// unavailable (no ports, midir init failed). The MIDI learn
    /// screen reads `last_event` from this and writes new bindings
    /// via the contained `MidiMap`.
    pub(crate) midi: Option<std::sync::Arc<std::sync::Mutex<crate::midi::ListenerState>>>,
    /// In-progress action selection in the MIDI learn screen.
    /// Cursor index into the list returned by `midi::all_actions()`.
    pub(crate) midi_learn_action_sel: usize,
    /// The most-recent MIDI event we've shown the user in the learn
    /// screen. Decoupled from the listener's `last_event` so the user
    /// can take their time picking an action without races.
    pub(crate) midi_learn_captured: Option<crate::midi::MidiEvent>,

    // Help screen filter — type to narrow the keybind list.
    pub(crate) help_filter: String,
    // Help scroll offset (rows). Up/Down step, PgUp/PgDn jump.
    pub(crate) help_scroll: u16,

    // When user presses `f` on dashboard with both decks loaded and
    // mini-browse not focused, this flag opens a tiny picker overlay
    // (a / b / Esc) so they can pick which deck's track to favorite.
    pub(crate) dash_fav_picker: bool,

    // Local filter
    pub(crate) filtering: bool,
    pub(crate) filter_text: String,

    pub(crate) queue_grab_index: Option<usize>, // index of grabbed queue item for reordering
    pub(crate) genre_picker: Option<GenrePickerState>,

    pub(crate) claude_dj: Option<Arc<TokioMutex<crate::claude::dj::ClaudeDJ>>>,
    pub(crate) dj_ask_buffer: String,
    pub(crate) dj_asking: bool,

    pub(crate) api: Option<Arc<TokioMutex<BeatportAPI>>>,
    pub(crate) downloader: Arc<StreamDownloader>,
    pub(crate) download_in_flight: bool,
    pub favorites: FavoritesDB,
    pub(crate) playlist_picker: Option<PlaylistPickerState>,
    /// Current page number for paginated screens (per screen stack depth)
    pub(crate) current_page: u32,
    /// The action that loaded the current screen (for "Load More")
    pub(crate) last_load_action: Option<MenuAction>,
    pub(crate) action_tx: mpsc::UnboundedSender<AppAction>,
    pub(crate) cached_info: NowPlayingInfo,
    pub(crate) last_screen_dump: std::time::Instant,
    pub(crate) last_quick_status: std::time::Instant,
    /// Wall-clock time of the most recent `Queue is low` Claude DJ trigger.
    /// Debounces re-firing while the DJ is still working on the prior one.
    pub(crate) last_low_queue_trigger: std::time::Instant,
    pub(crate) last_engine_state: crate::audio::engine::EngineState,
    // Mixer overlay selection state
    pub(crate) mixer_deck_is_a: bool,
    pub(crate) mixer_row: MixerRow,
    /// Drives the `test_mix` IPC state machine across ticks. None = inactive.
    pub(crate) test_mix_state: Option<TestMixState>,
    /// Drives multi-segment browse paths one step per tick, waiting for
    /// each async screen-push to arrive before advancing. `None` when
    /// idle. Used by IPC `Browse` and by `TestMix` so automation can
    /// drill through API-backed lists (Genres → Techno → Top 100, etc.).
    pub(crate) browse_path_state: Option<BrowsePathState>,
    /// Transition-rules editor state (None unless the overlay is open).
    pub(crate) rules_editor: Option<crate::tui::rules_editor::RulesEditor>,
    /// Pending resume prompt — set when config.resume_behavior is `Ask`
    /// and a session file exists. The dashboard shows a Y/N banner;
    /// answering `y` applies `pending_resume_snapshot` and starts
    /// playback, `n`/`Esc` deletes the session file.
    pub(crate) pending_resume_prompt: bool,
    /// Loaded session waiting to be applied. Held until either the
    /// prompt is answered (Ask) or auto-apply runs (Always).
    pub(crate) pending_resume_snapshot: Option<crate::session::SessionSnapshot>,
    /// When a track first loads, override its start_time from this map
    /// (keyed by Beatport track id) so a resumed track jumps to its
    /// saved position instead of first_beat_time.
    pub(crate) resume_positions: std::collections::HashMap<i64, f64>,
    /// Throttle periodic session saves.
    pub(crate) last_session_save: std::time::Instant,
    /// Snapshot of the most-recently-completed crossfade, captured on
    /// `CrossfadeComplete`. Used by the `+`/`-` rating hotkeys and the
    /// `rate_mix` IPC to attribute a rating to the right mix. `None`
    /// on a fresh session before the first crossfade completes.
    pub(crate) last_mix_entry: Option<crate::claude::memory::MixEntry>,
    /// Per-history-position MixEntry, parallel to `cached_info.history`.
    /// Lets the user go back and rate any past mix from the History
    /// view, not just the most recent one. `None` for the opening
    /// track (no preceding mix) and any entry whose CrossfadeComplete
    /// event hasn't been processed yet.
    pub(crate) mix_entries: Vec<Option<HistoryMix>>,
    /// Mix entries built on CrossfadeComplete that haven't been
    /// matched to a history position yet — drained when cached_info
    /// catches up. FIFO so multiple crossfades in one tick stay
    /// ordered correctly.
    pub(crate) pending_mix_entries: std::collections::VecDeque<crate::claude::memory::MixEntry>,
    /// Progress thresholds we've already fired mid-mix phase-check
    /// triggers for on the current crossfade. Reset on every
    /// Playing→Crossfading transition. Prevents re-firing the same
    /// checkpoint on every tick.
    pub(crate) mix_checkpoints_fired: [bool; 2],
    /// Separate timer for the manual-mode crossfade stall watchdog.
    /// Previously shared `last_low_queue_trigger` which caused
    /// cross-contamination — a queue-low trigger suppressed the
    /// watchdog or vice versa.
    pub(crate) last_crossfade_trigger: std::time::Instant,
    /// How many lines back from the latest log entry to start the
    /// dashboard LOG panel's visible window. 0 = tail (default,
    /// matches old behavior). Up arrow when LOG is focused
    /// increments; Down decrements toward 0.
    pub(crate) log_scroll_offset: usize,
    /// Per-frame click hit-testing table. Renderers (currently the
    /// dashboard controller) push (Rect, ClickAction) entries here
    /// during render(); the next mouse-down looks up the cell. Cleared
    /// on every render so stale targets from a prior view can't fire.
    pub click_targets: Vec<ClickTarget>,
    /// Waveform zoom state: None = no zoom, Some(true) = deck A zoomed,
    /// Some(false) = deck B zoomed. Click a waveform row to toggle.
    pub(crate) waveform_zoom: Option<bool>,
    /// `true` when mixr is rendering into a tmnl native pane (set
    /// by `blit::run` after construction). Drives a small layout
    /// adjustment: 1-cell horizontal padding outside the dashboard
    /// border (so it doesn't kiss the tmnl window edge) + 2 reserved
    /// rows at the bottom (future `:` cmdline + gutter, mirroring
    /// mnml). Top stays flush because tmnl already gives breathing
    /// room via `MACOS_TAB_STRIP_PX_SINGLE`.
    pub native_mode: bool,
}

/// Rectangular hit-test target with an action to fire on left-click.
/// Rendered widgets that want to be clickable push one of these to
/// `App.click_targets` during render.
#[derive(Debug, Clone)]
pub struct ClickTarget {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub action: ClickAction,
    /// Optional stable name for smoke-test lookup. Set to
    /// `Some("tempoA")`, `Some("crossfader")`, etc. on every user-
    /// facing interactive control so the `layout` IPC command can
    /// dump `name → rect` and tests don't have to hardcode coords.
    pub label: Option<&'static str>,
    /// MIDI action this UI element represents. When set, right-
    /// clicking the element opens the mapping flow: "move a
    /// controller, press Y to save, Esc to cancel."
    pub midi_action: Option<crate::midi::Action>,
}

impl ClickTarget {
    pub fn new(x: u16, y: u16, w: u16, h: u16, action: ClickAction) -> Self {
        Self {
            x,
            y,
            w,
            h,
            action,
            label: None,
            midi_action: None,
        }
    }
    /// Attach a stable lookup name. See `ClickTarget::label` for why.
    pub fn labeled(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
    /// Mark this target as right-clickable to bind a MIDI controller.
    /// The action determines what the future event will dispatch.
    pub fn bindable(mut self, midi_action: crate::midi::Action) -> Self {
        self.midi_action = Some(midi_action);
        self
    }
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x && col < self.x + self.w && row >= self.y && row < self.y + self.h
    }
}

/// Active "right-click → map" flow. When `Some`, the next MIDI event
/// from the listener is captured + held here for confirmation. Y/
/// Enter writes the binding to `~/.mixr/midi-map.json`; N/Esc drops
/// it. Other keys are absorbed so a stray keystroke can't accept
/// the wrong event.
#[derive(Debug, Clone)]
pub(crate) struct PendingMidiMap {
    pub(crate) action: crate::midi::Action,
    pub(crate) captured: Option<crate::midi::MidiEvent>,
}

#[derive(Debug, Clone)]
pub enum ClickAction {
    /// Set the crossfader from a click within a known X-range. The
    /// dispatcher reads the click's X coord and linearly maps
    /// (x_min..x_max) → (−1..+1). Renderer pushes one of these per
    /// rendered crossfader bar with the bar's exact column extent.
    SetCrossfaderRange { x_min: u16, x_max: u16 },
    /// Synthesize a key press. Reuses every existing keyboard handler
    /// (PLAY = `p`, JUMP back = `<`, NUDGE fwd = `]`, etc.) so mouse
    /// clicks on dashboard controls don't need parallel logic. The
    /// renderer just declares "this rect = this key".
    SimulateKey(crossterm::event::KeyCode),
    /// Set the list selection cursor to a specific row index. Used
    /// by browse/queue/history/settings to make rows clickable.
    SetSelected(usize),
    /// Focus a controller section on the dashboard (EQ low/mid/high,
    /// Filter, etc.). Also flips dash_focus to Controller so the
    /// scroll wheel (= ↑↓) starts adjusting the focused section.
    FocusDashSection(crate::tui::dashboard::CtrlSection),
    /// Select a row in the dashboard mini-browse panel. First click
    /// moves the cursor + flips dash_focus to Browse; second click on
    /// the same row drills in (handles browse_enter or queues a
    /// track at that index, depending on screen).
    DashBrowseSelect(usize),
    /// Toggle waveform zoom for a deck (true = deck A, false = deck B).
    WaveformZoom(bool),
    /// Cycle the global `jump_bars` setting (4 → 8 → 16 → 32 → 4).
    /// Clicking the number on either deck's `◀ JUMP N ▶` button.
    CycleJumpBars,
    /// Engage a beat-loop on a specific physical deck — used by the
    /// per-deck loop button rows so deck A's row always targets A.
    LoopEngageDeck { is_a: bool, beats: f64 },
    /// Release any active loop on a specific deck.
    LoopOffDeck { is_a: bool },
    /// Click + drag a vertical strip on the dashboard. Maps the cursor
    /// Y within (y_min..y_max) to a value and applies it to the right
    /// control (tempo rate, channel fader, EQ dB, or filter). Works
    /// for both Down and Drag mouse events — clicking a strip sets
    /// the value AND focuses the section so the scroll wheel can
    /// fine-tune from there.
    SetVerticalRange {
        control: RangeControl,
        y_min: u16,
        y_max: u16,
    },
}

/// Which vertical control a drag target adjusts. Values are mapped
/// with row y_max at the bottom = min value, y_min at the top = max.
/// Only continuous vertical strips live here — EQ/filter are text
/// readouts (not upfaders) and use `FocusDashSection` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeControl {
    /// Playback rate. Range mapped to 1.0 ± (config.tempo_range / 100).
    TempoA,
    TempoB,
    /// Channel fader (0.0..=1.0).
    VolumeA,
    VolumeB,
}

#[derive(Debug, Clone)]
pub(crate) struct BrowsePathState {
    /// Segments still to select, in order.
    pub(crate) remaining: Vec<String>,
    /// `screen_stack.len()` captured *before* the most recent segment
    /// was fired. The segment's push lands (sync or async) when the
    /// current depth exceeds this value. Once that happens we advance
    /// the next segment.
    pub(crate) before_depth: usize,
    /// Ticks waited for the current segment to land. Times out to avoid
    /// hanging state forever if an API call fails silently.
    pub(crate) waited: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestMixState {
    /// Browse navigation just fired; wait a tick for the async track list to load.
    WaitForList { ticks: u32 },
    /// Queue is being populated; wait for two different tracks on both decks.
    WaitForBothLoaded { ticks: u32 },
    /// Both decks loaded; about to teleport.
    Teleport,
}

impl App {
    pub async fn new(
        config: AppConfig,
        action_tx: mpsc::UnboundedSender<AppAction>,
        auth: StoredAuth,
    ) -> Result<Self> {
        let engine = MixEngine::new(&config)?;
        engine.set_pitch_stretch_engine(config.pitch_stretch_engine);
        engine.set_limiter_mode(config.master_limiter);
        engine.apply_claude_dj_settings(&config.claude_dj, config.claude_dj_enabled);
        // Always wire up the API — main.rs already acquired the OAuth
        // PKCE access_token (via cached/refresh/fresh-sign-in) before
        // we reach here.
        let api = Some(Arc::new(TokioMutex::new(BeatportAPI::new(auth))));

        let dj = {
            let dj_settings = config.claude_dj.clone();
            crate::claude::dj::ClaudeDJ::from_key_file().map(|mut dj| {
                // Pull the persisted settings block into the fresh DJ so
                // the next trigger's system_prompt reflects the user's
                // saved mode/strictness choices without waiting for an
                // IPC patch.
                dj.apply_settings(dj_settings);
                Arc::new(TokioMutex::new(dj))
            })
        };

        let local_lib_present = !config.local_library_dir.is_empty();
        let rekordbox_present = !config.rekordbox_xml.is_empty();
        let engine_present = !config.engine_dj_db.is_empty();
        let serato_present = !config.serato_db.is_empty();
        // Snapshot the dashboard layout choice before `config` is moved
        // into the struct — restores the user's last `v`-cycle choice.
        let initial_dash_layout = config.dash_layout;
        let initial_dash_panel_section = config.dash_panel_section;
        let mut app = Self {
            config,
            engine,
            toast: Toast::new(),
            status_writer: crate::ipc::StatusWriter::new(),
            waveform_mode: WaveformMode::Phrase,
            screen_stack: vec![catalog::root_screen_v3(
                local_lib_present,
                rekordbox_present,
                engine_present,
                serato_present,
            )],
            screen_stack_back: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            selected_column: -2,
            view_mode: ViewMode::Browse,
            dash_focus: DashFocus::Controller,
            dash_browse_sel: 0,
            dash_help: false,
            dash_section: CtrlSection::Crossfader,
            dash_layout: initial_dash_layout,
            dash_panel_section: initial_dash_panel_section,
            search_query: String::new(),
            command_prompt: None,
            pending_confirm: None,
            pending_midi_map: None,
            midi: None, // wired by main.rs after construction
            midi_learn_action_sel: 0,
            midi_learn_captured: None,
            search_results: Vec::new(),
            help_filter: String::new(),
            help_scroll: 0,
            dash_fav_picker: false,
            filtering: false,
            filter_text: String::new(),
            queue_grab_index: None,
            genre_picker: None,
            claude_dj: dj,
            dj_ask_buffer: String::new(),
            dj_asking: false,
            favorites: FavoritesDB::load(),
            playlist_picker: None,
            last_mix_entry: None,
            mix_entries: Vec::new(),
            pending_mix_entries: std::collections::VecDeque::new(),
            mix_checkpoints_fired: [false, false],
            last_crossfade_trigger: std::time::Instant::now(),
            log_scroll_offset: 0,
            click_targets: Vec::new(),
            waveform_zoom: None,
            native_mode: false,
            current_page: 1,
            last_load_action: None,
            api,
            downloader: Arc::new(StreamDownloader::new()),
            download_in_flight: false,
            action_tx,
            cached_info: NowPlayingInfo::default(),
            last_screen_dump: std::time::Instant::now() - std::time::Duration::from_secs(10),
            last_quick_status: std::time::Instant::now() - std::time::Duration::from_secs(10),
            last_low_queue_trigger: std::time::Instant::now() - std::time::Duration::from_secs(60),
            last_engine_state: crate::audio::engine::EngineState::Idle,
            mixer_deck_is_a: true,
            mixer_row: MixerRow::EqLow,
            test_mix_state: None,
            browse_path_state: None,
            rules_editor: None,
            pending_resume_prompt: false,
            pending_resume_snapshot: None,
            resume_positions: std::collections::HashMap::new(),
            last_session_save: std::time::Instant::now(),
        };
        app.check_startup_resume();
        Ok(app)
    }

    /// Look at `config.resume_behavior` + `~/.mixr/session.json` and
    /// either auto-resume (Always), stage a Y/N prompt (Ask), or do
    /// nothing (Never). Called once at the end of `new`.
    fn check_startup_resume(&mut self) {
        use crate::config::ResumeBehavior;
        if matches!(self.config.resume_behavior, ResumeBehavior::Never) {
            return;
        }
        let Some(snap) = crate::session::load() else {
            return;
        };
        match self.config.resume_behavior {
            ResumeBehavior::Always => self.apply_resume(snap),
            ResumeBehavior::Ask => {
                let n = snap.queue.len()
                    + snap.playing.as_ref().map(|_| 1).unwrap_or(0)
                    + snap.incoming.as_ref().map(|_| 1).unwrap_or(0);
                let when = snap.saved_at.clone();
                self.pending_resume_snapshot = Some(snap);
                self.pending_resume_prompt = true;
                // Long-duration toast so the prompt stays visible until
                // answered. Keys handler intercepts Y/N/Esc before any
                // normal bindings fire.
                self.toast.show(
                    &format!("Resume last session ({n} tracks, {when})? [Y]es / [N]o"),
                    600.0,
                );
            }
            ResumeBehavior::Never => unreachable!(),
        }
    }

    /// Queue the snapshotted tracks and remember their source-time
    /// positions so `TrackDecoded` can seek to them instead of the
    /// usual first_beat_time. Playing goes first (becomes the
    /// current deck), incoming second (staged), then the rest of
    /// the queue preserved in order.
    pub(crate) fn apply_resume(&mut self, snap: crate::session::SessionSnapshot) {
        let mut loaded_n = 0;
        if let Some(ref p) = snap.playing {
            self.resume_positions.insert(p.track.id, p.position);
            self.engine
                .enqueue(crate::audio::engine::QueueEntry::from(p.track.clone()));
            loaded_n += 1;
        }
        if let Some(ref i) = snap.incoming {
            self.resume_positions.insert(i.track.id, i.position);
            self.engine
                .enqueue(crate::audio::engine::QueueEntry::from(i.track.clone()));
            loaded_n += 1;
        }
        for t in snap.queue.into_iter() {
            self.engine
                .enqueue(crate::audio::engine::QueueEntry::from(t));
            loaded_n += 1;
        }
        self.pending_resume_snapshot = None;
        self.pending_resume_prompt = false;
        if loaded_n > 0 {
            self.toast.show(&format!("Resumed: {loaded_n} tracks"), 2.5);
        }
    }

    pub(crate) fn current_screen(&self) -> &BrowseScreen {
        self.screen_stack.last().unwrap()
    }

    pub(crate) fn breadcrumb(&self) -> String {
        self.screen_stack
            .iter()
            .map(|s| s.title())
            .collect::<Vec<_>>()
            .join(" > ")
    }

    /// Apply a step to the currently-selected Mixer overlay control.
    /// Step is +1 / -1; each row maps it to its own unit (dB, 0.05 filter, 0.05 fader).
    pub(crate) fn adjust_mixer_row(&mut self, step: f32) {
        let is_a = self.mixer_deck_is_a;
        let info = &self.cached_info;
        let deck_lbl = if is_a { "A" } else { "B" };

        // All three EQ bands share the same read/clamp/apply/toast shape;
        // the only differences are which field to read and which set_eq
        // argument to populate. Table the variants rather than spelling
        // out three near-identical match arms.
        let eq = match self.mixer_row {
            MixerRow::EqLow => Some((
                "Low",
                if is_a {
                    info.deck_a_eq_low_db
                } else {
                    info.deck_b_eq_low_db
                },
                0usize,
            )),
            MixerRow::EqMid => Some((
                "Mid",
                if is_a {
                    info.deck_a_eq_mid_db
                } else {
                    info.deck_b_eq_mid_db
                },
                1usize,
            )),
            MixerRow::EqHigh => Some((
                "High",
                if is_a {
                    info.deck_a_eq_high_db
                } else {
                    info.deck_b_eq_high_db
                },
                2usize,
            )),
            _ => None,
        };
        if let Some((name, cur, band)) = eq {
            let v = (cur + step).clamp(-24.0, 12.0);
            let (lo, mid, hi) = match band {
                0 => (Some(v), None, None),
                1 => (None, Some(v), None),
                _ => (None, None, Some(v)),
            };
            self.engine.set_eq(is_a, lo, mid, hi);
            self.toast
                .show(&format!("{name} {deck_lbl}: {v:+.0} dB"), 0.5);
            return;
        }

        match self.mixer_row {
            MixerRow::Filter => {
                let cur = if is_a {
                    info.deck_a_filter_pos
                } else {
                    info.deck_b_filter_pos
                };
                let v = (cur + step * 0.05).clamp(-1.0, 1.0);
                self.engine.set_filter(is_a, v);
                self.toast.show(&format!("Filter {deck_lbl}: {v:+.2}"), 0.5);
            }
            MixerRow::Fader => {
                let cur = if is_a {
                    info.channel_fader_a
                } else {
                    info.channel_fader_b
                };
                let v = (cur + step * 0.05).clamp(0.0, 1.0);
                self.engine.set_channel_fader(is_a, v);
                self.toast.show(&format!("Fader {deck_lbl}: {v:.2}"), 0.5);
            }
            _ => {} // EQ handled above
        }
    }

    pub(crate) fn reset_mixer_row(&mut self) {
        let is_a = self.mixer_deck_is_a;
        match self.mixer_row {
            MixerRow::EqLow => self.engine.set_eq(is_a, Some(0.0), None, None),
            MixerRow::EqMid => self.engine.set_eq(is_a, None, Some(0.0), None),
            MixerRow::EqHigh => self.engine.set_eq(is_a, None, None, Some(0.0)),
            MixerRow::Filter => self.engine.set_filter(is_a, 0.0),
            MixerRow::Fader => self.engine.set_channel_fader(is_a, 1.0),
        }
        self.toast
            .show(&format!("{} reset", self.mixer_row.label()), 0.5);
    }

    pub(crate) fn reset_all_mixer_controls(&mut self) {
        for is_a in [true, false] {
            self.engine.set_eq(is_a, Some(0.0), Some(0.0), Some(0.0));
            self.engine.set_filter(is_a, 0.0);
            self.engine.set_channel_fader(is_a, 1.0);
        }
        self.engine.set_crossfader(0.0);
        self.toast.show("Mixer reset", 0.8);
    }

    pub fn render(&mut self, frame: &mut Frame) {
        // Clear last frame's hit-test table — stale targets from a
        // prior view (e.g. the dashboard controller before the user
        // pressed `b` to browse) shouldn't fire on a click.
        //
        // Optimization: non-dashboard views don't push click targets,
        // so once cleared they stay empty. Skip the clear + rebuild
        // when we're not on the dashboard and already empty.
        if matches!(self.view_mode, ViewMode::Dashboard) || !self.click_targets.is_empty() {
            self.click_targets.clear();
        }
        // Native mode (mixr hosted inside tmnl) reserves padding so
        // the dashboard border doesn't kiss the tmnl window edge,
        // plus 1 row at the bottom for the (future) `:` cmdline.
        // tmnl's own sub-cell letterbox at the very bottom acts as
        // the gutter, so we don't need a separate reserved row for
        // that.
        //
        // Horizontal padding is asymmetric: 1 cell on the left, 2
        // cells on the right. tmnl's wgpu pipeline sub-cell-snaps
        // the right edge so 1 cell there sometimes reads as
        // "barely there"; 2 cells lands reliably visible. The left
        // edge doesn't suffer the same snap so 1 cell is enough.
        //
        // Top stays flush; tmnl's `MACOS_TAB_STRIP_PX_SINGLE` (52px)
        // already gives breathing room above for the macOS
        // traffic-light buttons.
        let size = if self.native_mode {
            let a = frame.area();
            ratatui::layout::Rect::new(
                a.x + 1,
                a.y,
                a.width.saturating_sub(3),
                a.height.saturating_sub(1),
            )
        } else {
            frame.area()
        };
        let info = &self.cached_info;
        let np_height = if info.state == crate::audio::engine::EngineState::Crossfading
            && info.incoming_track.is_some()
        {
            2
        } else {
            1
        };
        // Pre-compute the hints string so we can size its chunk based
        // on whether it fits in one row. When it doesn't, allocate two
        // and let the Paragraph's word wrap split it.
        let hints = self.build_hints_string(info);
        let hints_height: u16 = if hints.chars().count() > size.width as usize {
            2
        } else {
            1
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(np_height),
                Constraint::Length(hints_height),
            ])
            .split(size);

        // Build breadcrumb for block titles
        let bc = self.breadcrumb();
        let item_count = self.current_screen().item_count();
        let pos = if item_count > 0 {
            format!("  [{}/{}]", self.selected + 1, item_count)
        } else {
            String::new()
        };
        let block_title = format!(" {bc}{pos} ");

        // Outer block with breadcrumb in border
        let outer_title = match self.view_mode {
            ViewMode::Help => " Beatport > Help ".to_string(),
            ViewMode::Mixer => " Mixer ".to_string(),
            ViewMode::TransitionRules => " Transition Rules ".to_string(),
            ViewMode::ClaudeDj => " Claude DJ ".to_string(),
            ViewMode::MidiLearn => " MIDI Learn ".to_string(),
            ViewMode::PlaylistPicker => " Add to Playlist ".to_string(),
            ViewMode::PlaylistNameInput => " New Playlist Name ".to_string(),
            ViewMode::Settings => " Beatport > Settings ".to_string(),
            ViewMode::GenrePicker => " Settings > Default Genre ".to_string(),
            ViewMode::FavoritesPicker => format!(
                " Settings > Favorite Genres ({} selected) ",
                self.config.favorite_genres.len()
            ),
            ViewMode::Queue => format!(" Queue{pos} "),
            ViewMode::History => " History ".to_string(),
            ViewMode::Search => format!(" {} > Search ", self.breadcrumb()),
            ViewMode::Dashboard => " Dashboard ".to_string(),
            ViewMode::Browse => block_title,
        };
        // Dashboard renders without outer border — boxes go edge to edge
        if matches!(self.view_mode, ViewMode::Dashboard) {
            let browse_items: Vec<String> = (0..self.current_screen().item_count().min(8))
                .map(|i| self.current_screen().item_label(i))
                .collect();
            let browse_bc = self.breadcrumb();
            let browse_is_tracks = matches!(self.current_screen(), BrowseScreen::TrackList { .. });
            let sel_section = if self.dash_focus == DashFocus::Controller {
                Some(self.dash_section)
            } else {
                None
            };
            let dj_log: Vec<String> = self
                .claude_dj
                .as_ref()
                .map(|dj| {
                    let Ok(dj) = dj.try_lock() else {
                        return Vec::new();
                    };
                    dj.log_entries()
                        .iter()
                        .rev()
                        .take(4)
                        .rev()
                        .map(|e| {
                            let icon = match e.entry_type {
                                crate::claude::dj::LogEntryType::Action => "ACT",
                                crate::claude::dj::LogEntryType::Track => "TRK",
                                crate::claude::dj::LogEntryType::Phase => "PHZ",
                                crate::claude::dj::LogEntryType::Flow => "FLO",
                                crate::claude::dj::LogEntryType::User => "USR",
                                crate::claude::dj::LogEntryType::Error => "ERR",
                                crate::claude::dj::LogEntryType::Info => "INF",
                            };
                            let msg = if e.message.len() > 80 {
                                format!("{}…", &e.message[..79])
                            } else {
                                e.message.clone()
                            };
                            format!("{icon} {msg}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            let dj_ask = if self.dj_asking {
                Some(self.dj_ask_buffer.as_str())
            } else {
                None
            };
            dashboard::render_dashboard(
                frame,
                chunks[0],
                info,
                self.waveform_mode,
                &browse_items,
                &browse_bc,
                self.dash_browse_sel,
                browse_is_tracks,
                self.dash_help,
                sel_section,
                self.download_in_flight,
                &dj_log,
                dj_ask,
                &mut self.click_targets,
                self.dash_focus,
                self.log_scroll_offset,
                self.waveform_zoom,
                self.dash_layout,
                self.dash_panel_section,
            );
            if self.dash_fav_picker {
                self.render_fav_picker(frame, chunks[0]);
            }
        } else {
            // All other modes get the outer border
            let outer_block = Block::default()
                .title(outer_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray));
            let content_area = outer_block.inner(chunks[0]);
            frame.render_widget(outer_block, chunks[0]);

            // Clickable [← back] button in the top-right of the box
            // border. Mouse-only users need a target for Esc; keyboard
            // users still have Esc. Only visible on non-Dashboard
            // views since the Dashboard IS the back-target.
            let back_label = " [← back] ";
            let back_w = back_label.chars().count() as u16;
            if chunks[0].width > back_w + 4 {
                let back_x = chunks[0].x + chunks[0].width - back_w - 1;
                let back_y = chunks[0].y;
                use ratatui::style::{Color as Clr, Style as Sty};
                use ratatui::widgets::Paragraph;
                let back_para = Paragraph::new(back_label).style(Sty::default().fg(Clr::Cyan));
                frame.render_widget(
                    back_para,
                    ratatui::layout::Rect {
                        x: back_x,
                        y: back_y,
                        width: back_w,
                        height: 1,
                    },
                );
                self.click_targets.push(
                    ClickTarget::new(
                        back_x,
                        back_y,
                        back_w,
                        1,
                        ClickAction::SimulateKey(crossterm::event::KeyCode::Esc),
                    )
                    .labeled("back_button"),
                );
            }

            match self.view_mode {
                ViewMode::Dashboard => unreachable!(),
                ViewMode::Help => {
                    screens::render_help(
                        frame,
                        content_area,
                        &self.help_filter,
                        &mut self.help_scroll,
                    );
                }
                ViewMode::Mixer => {
                    render_mixer_overlay(
                        frame,
                        content_area,
                        info,
                        self.mixer_deck_is_a,
                        self.mixer_row,
                    );
                }
                ViewMode::TransitionRules => {
                    if let Some(ref ed) = self.rules_editor {
                        super::rules_editor::render(frame, content_area, ed);
                    }
                }
                ViewMode::MidiLearn => {
                    // Pull bindings from the listener so the screen shows
                    // current state without bus latency. Locked briefly
                    // during render — listener thread blocks on us only
                    // long enough to clone the binding list (~µs).
                    let (captured, captured_value, bindings) = if let Some(midi) = &self.midi {
                        if let Ok(state) = midi.lock() {
                            let cap_val = state.last_event.as_ref().map(|(_, v)| *v);
                            (
                                self.midi_learn_captured
                                    .clone()
                                    .or_else(|| state.last_event.clone().map(|(e, _)| e)),
                                cap_val,
                                state.map.bindings.clone(),
                            )
                        } else {
                            (None, None, vec![])
                        }
                    } else {
                        (None, None, vec![])
                    };
                    super::midi_learn::render(
                        frame,
                        content_area,
                        captured.as_ref(),
                        captured_value,
                        self.midi_learn_action_sel,
                        &bindings,
                    );
                }
                ViewMode::PlaylistPicker => {
                    if let Some(ref picker) = self.playlist_picker {
                        let mut items = vec!["+ Create New Playlist...".to_string()];
                        items.extend(picker.playlists.iter().map(|p| p.name.clone()));
                        screens::render_menu(frame, content_area, &items, picker.selected, 0);
                    }
                }
                ViewMode::PlaylistNameInput => {
                    if let Some(ref picker) = self.playlist_picker {
                        let input = Paragraph::new(Line::from(vec![
                            Span::styled("Name: ", Style::default().fg(Color::Yellow)),
                            Span::raw(&picker.new_name),
                            Span::styled("█", Style::default().fg(Color::White)),
                        ]));
                        frame.render_widget(input, content_area);
                    }
                }
                ViewMode::Settings => {
                    super::settings::render_settings(
                        frame,
                        content_area,
                        &self.config,
                        self.selected,
                    );
                    // Click targets — only register rects for editable Row
                    // items (skip Section headers). `selected` is a row-index
                    // (sections don't count) so `ClickAction::SetSelected(N)`
                    // gets the right slot.
                    let items = super::settings::build_settings(&self.config);
                    let mut row_idx = 0usize;
                    for (line_idx, item) in items.iter().enumerate() {
                        if let super::settings::SettingItem::Row(_) = item {
                            let y = content_area.y + line_idx as u16;
                            if y < content_area.y + content_area.height {
                                self.click_targets.push(ClickTarget::new(
                                    content_area.x,
                                    y,
                                    content_area.width,
                                    1,
                                    ClickAction::SetSelected(row_idx),
                                ));
                            }
                            row_idx += 1;
                        }
                    }
                }
                ViewMode::GenrePicker | ViewMode::FavoritesPicker => {
                    if let Some(ref picker) = self.genre_picker {
                        let is_favs = matches!(self.view_mode, ViewMode::FavoritesPicker);
                        let items: Vec<String> = picker
                            .genres
                            .iter()
                            .map(|g| {
                                if is_favs {
                                    let star = if self.config.favorite_genres.contains(&g.name) {
                                        "★"
                                    } else {
                                        "·"
                                    };
                                    format!("{star} {}", g.name)
                                } else {
                                    g.name.clone()
                                }
                            })
                            .collect();
                        screens::render_menu(
                            frame,
                            content_area,
                            &items,
                            picker.selected,
                            picker.scroll_offset,
                        );
                    }
                }
                ViewMode::Queue => {
                    let refs: Vec<&BeatportTrack> =
                        info.queue.iter().map(|e| e.track.as_ref()).collect();
                    screens::render_track_list_refs(
                        frame,
                        content_area,
                        &refs,
                        self.selected,
                        self.scroll_offset,
                        -1,
                        true,
                    );
                    Self::push_list_row_targets(
                        &mut self.click_targets,
                        content_area,
                        refs.len(),
                        self.scroll_offset,
                    );
                }
                ViewMode::History => {
                    let refs: Vec<&BeatportTrack> =
                        info.history.iter().map(|e| e.track.as_ref()).collect();
                    screens::render_track_list_refs(
                        frame,
                        content_area,
                        &refs,
                        self.selected,
                        self.scroll_offset,
                        -1,
                        true,
                    );
                    Self::push_list_row_targets(
                        &mut self.click_targets,
                        content_area,
                        refs.len(),
                        self.scroll_offset,
                    );
                }
                ViewMode::Search => {
                    self.render_search(frame, content_area);
                }
                ViewMode::Browse => {
                    self.render_browse(frame, content_area);
                    let count = self.current_screen().item_count();
                    Self::push_list_row_targets(
                        &mut self.click_targets,
                        content_area,
                        count,
                        self.scroll_offset,
                    );
                }
                ViewMode::ClaudeDj => {
                    super::claude_screen::render_claude_screen(
                        frame,
                        content_area,
                        self.claude_dj.as_ref(),
                        self.scroll_offset,
                    );
                }
            }
        } // end else (non-dashboard)

        // Now playing bar (+ incoming line during crossfade)
        if np_height == 2 {
            // Split the now-playing area into 2 lines
            let np_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(chunks[1]);
            // Line 1: playing track
            let track_name = info
                .playing_track
                .as_ref()
                .map(|t| format!("{} - {}", t.artist_name(), t.full_title()));
            screens::render_now_playing(
                frame,
                np_chunks[0],
                track_name.as_deref(),
                info.playing_bpm,
                info.playing_duration - info.playing_time,
                0.0,
            );
            // Line 2: incoming track (yellow)
            if let Some(ref inc) = info.incoming_track {
                let inc_name = format!("{} - {}", inc.artist_name(), inc.full_title());
                let native_bpm = inc.bpm.unwrap_or(0.0);
                let target_bpm = info.playing_bpm.unwrap_or(native_bpm);
                let bpm_str = if (native_bpm - target_bpm).abs() > 0.5 {
                    format!("{:.0}→{:.0} BPM", native_bpm, target_bpm)
                } else {
                    format!("{:.0} BPM", native_bpm)
                };
                let xf_pct = format!("{:.0}%", info.crossfade_progress * 100.0);
                let text = format!("◀ {inc_name}  {bpm_str}  {xf_pct}");
                let widget = Paragraph::new(text).style(Style::default().fg(Color::Yellow));
                frame.render_widget(widget, np_chunks[1]);
            }
        } else {
            let track_name = info
                .playing_track
                .as_ref()
                .map(|t| format!("{} - {}", t.artist_name(), t.full_title()));
            let progress = if info.playing_duration > 0.0 {
                info.playing_time / info.playing_duration
            } else {
                0.0
            };
            screens::render_now_playing(
                frame,
                chunks[1],
                track_name.as_deref(),
                info.playing_bpm,
                info.playing_duration - info.playing_time,
                progress,
            );
        }

        // Toast — float above the bottom strip (now-playing + legend).
        // Bottom of toast lands one row above now-playing so it never
        // covers the legend or the playback bar regardless of how many
        // rows either takes (legend is 1 or 2; np is 1 or 2).
        if let Some(msg) = self.toast.current() {
            let w = (msg.len() as u16 + 4).min(size.width);
            let toast_h: u16 = 3;
            // Compute reserved bottom strip from chunk heights.
            let bottom_strip = np_height + hints_height;
            // Place toast bottom one row above the bottom strip.
            // saturating_sub handles small terminals gracefully.
            let toast_y = size.height.saturating_sub(bottom_strip + toast_h + 1);
            let area = Rect {
                x: size.width.saturating_sub(w) / 2,
                y: toast_y,
                width: w,
                height: toast_h,
            };
            let tw = Paragraph::new(format!(" {msg} "))
                .style(Style::default().fg(Color::Black).bg(Color::Yellow))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(tw, area);
        }

        // Hints bar: wraps to 2 rows automatically when the text would
        // truncate in 1 row. Height was decided at the top of render()
        // based on char count vs terminal width.
        let hints_widget = Paragraph::new(hints.as_str().to_string())
            .style(Style::default().fg(Color::DarkGray))
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(hints_widget, chunks[2]);

        // `:` command-prompt overlay (vim-style). Renders over the
        // hints bar at the very bottom — single line, full width,
        // visible until Enter/Esc. The cursor is just a trailing block.
        if let Some(buf) = &self.command_prompt {
            let prompt_y = size.height.saturating_sub(1);
            let area = Rect {
                x: 0,
                y: prompt_y,
                width: size.width,
                height: 1,
            };
            let display = format!(":{buf}█");
            let widget =
                Paragraph::new(display).style(Style::default().fg(Color::Black).bg(Color::Yellow));
            frame.render_widget(widget, area);
        }
    }

    /// Build the bottom hints bar string for the current view mode.
    /// Pulled out of render() so the layout can pre-size the hints
    /// chunk based on the string's length.
    fn build_hints_string(&self, info: &NowPlayingInfo) -> String {
        let esc_label = if self.screen_stack.len() <= 1 {
            "[Esc] Quit"
        } else {
            "[Esc] Back"
        };
        let play_pause = if info.playing_track.is_some() {
            "[p] Pause"
        } else {
            "[p] Play"
        };
        match self.view_mode {
            ViewMode::PlaylistPicker => "[Enter] Select  [Esc] Back".into(),
            ViewMode::PlaylistNameInput => "[Type] Name  [Enter] Create  [Esc] Back".into(),
            ViewMode::GenrePicker => format!("[Enter] Select  {esc_label}"),
            ViewMode::FavoritesPicker => format!("[Enter] Toggle ★  {esc_label}"),
            ViewMode::Settings => format!("[Enter] Change  [,] Close  {esc_label}"),
            ViewMode::ClaudeDj => format!("[C] Toggle  [/] Ask  {esc_label}"),
            ViewMode::Dashboard => format!(
                "[?] Help  [Tab] Focus  {play_pause}  [n] Skip  [t] Teleport  [T] Rewind  [w] Waveform  [c] DJ Screen  [C] Toggle DJ  [/] Ask DJ  [d] Close  {esc_label}"
            ),
            ViewMode::Search => {
                "[Type] Search  [↑↓] Navigate  [Space] Preview  [Enter] Queue  [Esc] Back".into()
            }
            ViewMode::Queue => format!(
                "[←→] Columns  [f] Fav  [o] Web  {play_pause}  [n] Skip  [X] Clear  [d] Dashboard  [q] Close  {esc_label}"
            ),
            ViewMode::History => format!("[h] Close  {esc_label}"),
            ViewMode::Help => esc_label.to_string(),
            ViewMode::Mixer => {
                "[Tab] Deck  [↑↓] Row  [←→] Adjust  [r] Reset  [R] Reset All  [Esc] Close".into()
            }
            ViewMode::TransitionRules => {
                "[↑↓] Nav  [Enter] Edit  [i] Insert  [D] Delete  [{/}] Reorder  [Esc] Save+Close"
                    .into()
            }
            ViewMode::MidiLearn => {
                "[Touch a control]  [↑↓] Pick action  [Enter] Bind  [U] Unbind  [Esc] Close".into()
            }
            ViewMode::Browse => match self.current_screen() {
                BrowseScreen::TrackList { .. } if self.selected_column >= -1 => {
                    match self.selected_column {
                        -1 => "[Enter] Release  [o] Open  [←] Back  [→] Next Column".to_string(),
                        0 => "[Enter] Artist  [o] Open  [←] Back  [→] Next Column".to_string(),
                        1 => "[Enter] Remixer  [o] Open  [←] Back  [→] Next Column".to_string(),
                        2 => "[Enter] Label  [o] Open  [←] Back  [→] Next Column".to_string(),
                        3 => "[Enter] Genre  [o] Open  [←] Back  [→] Next Column".to_string(),
                        4 => "[Enter] Year  [o] Open  [←] Back".to_string(),
                        _ => "[←→] Columns".to_string(),
                    }
                }
                BrowseScreen::TrackList { .. } => format!(
                    "[Space] Preview  [Enter] Queue  [←→] Columns  [a] All  [f] Fav  [o] Web  [+] Playlist  [d] Dashboard  [q] Queue  [/] Search  {esc_label}"
                ),
                BrowseScreen::ArtistList { .. } | BrowseScreen::LabelList { .. } => format!(
                    "[Enter] Select  [o] Web  [d] Dashboard  [q] Queue  [/] Search  {esc_label}"
                ),
                _ => format!(
                    "[Enter] Select  [o] Web  [q] Queue  [d] Dashboard  [,] Settings  [/] Search  {esc_label}"
                ),
            },
        }
    }

    /// Centered modal asking which deck's track to favorite when both
    /// are loaded. Lists artist - title for each side; user picks A/B
    /// or Esc to cancel. Triggered from the dashboard `f`/`*` handler.
    fn render_fav_picker(&self, frame: &mut Frame, area: Rect) {
        use ratatui::layout::Alignment;
        use ratatui::style::Modifier;
        use ratatui::widgets::Clear;
        let line_for =
            |t: &Option<std::sync::Arc<crate::beatport::models::BeatportTrack>>| -> String {
                match t {
                    Some(tr) => format!("{} - {}", tr.artist_name(), tr.full_title()),
                    None => "(empty)".into(),
                }
            };
        let a = line_for(&self.cached_info.deck_a_track);
        let b = line_for(&self.cached_info.deck_b_track);

        let lines = vec![
            Line::from(Span::styled(
                " Favorite which deck? ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "  [a] ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(a),
            ]),
            Line::from(vec![
                Span::styled(
                    "  [b] ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(b),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let w = 70u16.min(area.width.saturating_sub(4));
        let h = (lines.len() as u16) + 2;
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let modal = Rect {
            x,
            y,
            width: w,
            height: h,
        };

        frame.render_widget(Clear, modal);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        let para = Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left);
        frame.render_widget(para, modal);
    }

    fn render_browse(&self, frame: &mut Frame, area: Rect) {
        let has_filter = !self.filter_text.is_empty() || self.filtering;

        // Split area for filter input if active
        let (content_area, filter_area) = if has_filter {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            (chunks[0], Some(chunks[1]))
        } else {
            (area, None)
        };

        let screen = self.current_screen();

        if has_filter {
            // Render filtered items
            let indices = self.filtered_indices();
            let items: Vec<String> = indices.iter().map(|&i| screen.item_label(i)).collect();
            screens::render_menu(
                frame,
                content_area,
                &items,
                self.selected,
                self.scroll_offset,
            );

            // Filter input line
            if let Some(fa) = filter_area {
                let cursor = if self.filtering { "█" } else { "" };
                let widget = Paragraph::new(Line::from(vec![
                    Span::styled("  Filter: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&self.filter_text),
                    Span::styled(cursor, Style::default().fg(Color::White)),
                ]));
                frame.render_widget(widget, fa);
            }
        } else {
            match screen {
                BrowseScreen::TrackList { tracks, .. } => {
                    screens::render_track_list(
                        frame,
                        content_area,
                        tracks,
                        self.selected,
                        self.scroll_offset,
                        self.selected_column,
                        self.config.compact_view,
                    );
                }
                BrowseScreen::GenreList { genres, .. } => {
                    let favs = &self.config.favorite_genres;
                    let items: Vec<String> = genres
                        .iter()
                        .map(|g| {
                            if favs.iter().any(|f| f.eq_ignore_ascii_case(&g.name)) {
                                format!("{} ★", g.name)
                            } else {
                                g.name.clone()
                            }
                        })
                        .collect();
                    screens::render_menu(
                        frame,
                        content_area,
                        &items,
                        self.selected,
                        self.scroll_offset,
                    );
                }
                _ => {
                    let items: Vec<String> = (0..screen.item_count())
                        .map(|i| screen.item_label(i))
                        .collect();
                    screens::render_menu(
                        frame,
                        content_area,
                        &items,
                        self.selected,
                        self.scroll_offset,
                    );
                }
            }
        }
    }

    fn render_search(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);

        let input = Paragraph::new(Line::from(vec![
            Span::styled("  Search: ", Style::default().fg(Color::Yellow)),
            Span::raw(&self.search_query),
            Span::styled("█", Style::default().fg(Color::White)),
        ]));
        frame.render_widget(input, chunks[0]);

        if !self.search_results.is_empty() {
            screens::render_track_list(
                frame,
                chunks[1],
                &self.search_results,
                self.selected,
                self.scroll_offset,
                -1,
                self.config.compact_view,
            );
        }
    }

    /// Push one click target per visible row in a list view so each
    /// row becomes clickable. Takes `targets` by &mut so it doesn't
    /// require a fresh &mut self reborrow when the caller already has
    /// `&self.cached_info` borrowed.
    fn push_list_row_targets(
        targets: &mut Vec<ClickTarget>,
        area: ratatui::layout::Rect,
        count: usize,
        scroll_offset: usize,
    ) {
        let visible = (area.height as usize).min(count.saturating_sub(scroll_offset));
        for vis_row in 0..visible {
            let abs_row = scroll_offset + vis_row;
            targets.push(ClickTarget::new(
                area.x,
                area.y + vis_row as u16,
                area.width,
                1,
                ClickAction::SetSelected(abs_row),
            ));
        }
    }

    pub(crate) fn load_more(&mut self, action: &MenuAction) {
        let Some(api) = self.require_api() else {
            return;
        };
        self.current_page += 1;
        let page = self.current_page;
        let tx = self.action_tx.clone();
        let action = action.clone();
        self.toast.show(&format!("Loading page {page}..."), 2.0);

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match catalog::execute_action_page(&action, &mut api, page).await {
                Ok(Some(screen)) => match screen {
                    BrowseScreen::TrackList { tracks, .. } => {
                        if tracks.is_empty() {
                            tx.send(AppAction::Toast("No more".into())).ok();
                        } else {
                            tx.send(AppAction::AppendTracks(tracks)).ok();
                        }
                    }
                    BrowseScreen::ChartList { charts, .. } => {
                        if charts.is_empty() {
                            tx.send(AppAction::Toast("No more".into())).ok();
                        } else {
                            tx.send(AppAction::AppendCharts(charts)).ok();
                        }
                    }
                    BrowseScreen::ReleaseList { releases, .. } => {
                        if releases.is_empty() {
                            tx.send(AppAction::Toast("No more".into())).ok();
                        } else {
                            tx.send(AppAction::AppendReleases(releases)).ok();
                        }
                    }
                    _ => {
                        tx.send(AppAction::Toast("Load More not supported".into()))
                            .ok();
                    }
                },
                Ok(None) => {
                    tx.send(AppAction::Toast("Load More not supported".into()))
                        .ok();
                }
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                }
            }
        });
    }

    pub(crate) fn open_playlist_picker(&mut self, track_id: i64) {
        let Some(api) = self.require_api() else {
            return;
        };
        let tx = self.action_tx.clone();
        self.toast.show("Loading playlists...", 2.0);

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match api.my_playlists().await {
                Ok(playlists) => {
                    tx.send(AppAction::ShowPlaylistPicker {
                        track_id,
                        playlists,
                    })
                    .ok();
                }
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                }
            }
        });
    }

    pub(crate) fn add_track_to_playlist(
        &mut self,
        playlist_id: i64,
        track_id: i64,
        playlist_name: &str,
    ) {
        let Some(api) = self.api.clone() else {
            return;
        };
        let tx = self.action_tx.clone();
        let pname = playlist_name.to_string();
        self.playlist_picker = None;
        self.view_mode = ViewMode::Browse;

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match api.add_to_playlist(playlist_id, &[track_id]).await {
                Ok(()) => {
                    tx.send(AppAction::Toast(format!("Added to {pname}"))).ok();
                }
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                }
            }
        });
    }

    pub(crate) fn create_and_add_to_playlist(&mut self, name: String, track_id: i64) {
        let Some(api) = self.api.clone() else {
            return;
        };
        let tx = self.action_tx.clone();
        let pname = name.clone();
        self.playlist_picker = None;
        self.view_mode = ViewMode::Browse;

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match api.create_playlist(&name).await {
                Ok(pid) => match api.add_to_playlist(pid, &[track_id]).await {
                    Ok(()) => {
                        tx.send(AppAction::Toast(format!(
                            "Created '{pname}' and added track"
                        )))
                        .ok();
                    }
                    Err(e) => {
                        tx.send(AppAction::Toast(format!(
                            "Created playlist but add failed: {e}"
                        )))
                        .ok();
                    }
                },
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Create playlist failed: {e}")))
                        .ok();
                }
            }
        });
    }

    pub(crate) fn open_genre_picker(&mut self, favorites: bool) {
        let Some(api) = self.require_api() else {
            return;
        };
        let tx = self.action_tx.clone();
        self.toast.show("Loading genres...", 2.0);

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match api.genres().await {
                Ok(genres) => {
                    tx.send(AppAction::ShowGenrePicker { favorites, genres })
                        .ok();
                }
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                }
            }
        });
    }

    /// Build context string for Claude DJ.
    /// Apply a settings row change and perform any engine sync or side-effect.
    /// Returns `true` if the caller should return early (picker/editor opened).
    /// "A" when `is_a` is true, else "B". Shared by toast/log formatters.
    pub(crate) fn deck_label(is_a: bool) -> &'static str {
        if is_a { "A" } else { "B" }
    }

    /// Grab the Beatport API handle, toasting "Not authenticated" and
    /// returning None when the user hasn't signed in. Collapses the
    /// 7-site `let Some(api) = self.api.clone() else { toast; return; }`
    /// pattern to a single call.
    pub(crate) fn require_api(&mut self) -> Option<Arc<TokioMutex<BeatportAPI>>> {
        match self.api.clone() {
            Some(a) => Some(a),
            None => {
                self.toast.show("Not authenticated", 2.0);
                None
            }
        }
    }

    /// Push every relevant `self.config` value down into the audio
    /// engine + Claude DJ + save to disk. Used by the reset-all
    /// paths (Enter on the sentinel row + capital `R` in Settings)
    /// where `apply_and_sync_setting` can't be called per-row
    /// because the whole config was just blown away to defaults.
    /// Mirror this list with the engine sync calls scattered through
    /// `apply_and_sync_setting`'s match arms.
    pub(crate) fn resync_all_engine_settings(&mut self) {
        self.engine
            .set_enabled_transitions(self.config.enabled_transitions.clone());
        self.engine
            .set_pitch_stretch_engine(self.config.pitch_stretch_engine);
        self.engine
            .set_train_wreck_mode(self.config.train_wreck_mode);
        self.engine.set_crossfade_bars(self.config.crossfade_bars);
        self.engine
            .set_crossfade_bars_auto(self.config.crossfade_bars_auto);
        self.engine
            .set_quantize(self.config.quantize_on, self.config.quantize_beats);
        self.engine.set_jump_bars(self.config.jump_bars);
        self.engine.set_limiter_mode(self.config.master_limiter);
        let new = self.config.claude_dj.clone();
        if let Some(dj) = &self.claude_dj
            && let Ok(mut dj) = dj.try_lock()
        {
            dj.apply_settings(new.clone());
        }
        self.engine
            .apply_claude_dj_settings(&new, self.config.claude_dj_enabled);
        self.config.save();
    }

    pub(crate) fn apply_and_sync_setting(&mut self, key: &str, option_idx: usize) -> bool {
        if let Some(action) = super::settings::apply_setting(&mut self.config, key, option_idx) {
            match action {
                "logout" => {
                    // Delete the OAuth tokens from disk and spawn the
                    // logout subprocess to clear the WebView's
                    // persistent cookie store too (so the next sign-in
                    // starts truly fresh).
                    crate::beatport::auth::StoredAuth::delete();
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe)
                            .arg("--logout")
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                    if let Some(api) = &self.api
                        && let Ok(mut api) = api.try_lock() {
                            api.clear_browse_cache();
                        }
                    self.toast.show("Logged out — restart mixr to sign in again", 4.0);
                }
                "pick_genre" => { self.open_genre_picker(false); return true; }
                "pick_favorites" => { self.open_genre_picker(true); return true; }
                "transitions_changed" => self.engine.set_enabled_transitions(self.config.enabled_transitions.clone()),
                "pitch_stretch_changed" => self.engine.set_pitch_stretch_engine(self.config.pitch_stretch_engine),
                "train_wreck_changed" => self.engine.set_train_wreck_mode(self.config.train_wreck_mode),
                "rubberband_unavailable" => self.spawn_install_rubberband(),
                "timestretch_unavailable" => self.toast.show(
                    "Timestretch needs `cargo build --features timestretch`",
                    3.0,
                ),
                "crossfade_bars_changed" => {
                    self.engine.set_crossfade_bars(self.config.crossfade_bars);
                    self.engine.set_crossfade_bars_auto(self.config.crossfade_bars_auto);
                }
                "quantize_changed" => {
                    self.engine.set_quantize(self.config.quantize_on, self.config.quantize_beats);
                }
                "jump_bars_changed" => {
                    self.engine.set_jump_bars(self.config.jump_bars);
                }
                "stratum_fallback" => self.toast.show(
                    "Stratum not compiled — falling back to built-in. Rebuild with `--features stratum` to use it.",
                    4.0),
                "master_limiter_changed" => self.engine.set_limiter_mode(self.config.master_limiter),
                "claudedj_changed" => {
                    let new = self.config.claude_dj.clone();
                    if let Some(dj) = &self.claude_dj
                        && let Ok(mut dj) = dj.try_lock() { dj.apply_settings(new.clone()); }
                    self.engine.apply_claude_dj_settings(&new, self.config.claude_dj_enabled);
                    self.toast.show(&format!("Claude DJ: {:?} mode", new.mode), 1.5);
                }
                "monitor_device_changed" => {
                    self.config.save();
                    self.toast.show("Monitor device changed — restarting to apply…", 3.0);
                    let _ = std::fs::write(
                        dirs::home_dir().unwrap_or_default().join(".mixr/command"),
                        b"{\"restart\":1}",
                    );
                }
                "output_device_changed" => {
                    self.config.save();
                    self.toast.show("Output device changed — restarting to apply…", 3.0);
                    let _ = std::fs::write(
                        dirs::home_dir().unwrap_or_default().join(".mixr/command"),
                        b"{\"restart\":1}",
                    );
                }
                "open_rules_editor" => {
                    self.rules_editor = Some(super::rules_editor::RulesEditor::new(self.engine.rules_config()));
                    self.view_mode = ViewMode::TransitionRules;
                    return true;
                }
                _ => {}
            }
        }
        self.config.save();
        self.toast.show("Setting changed", 0.5);
        false
    }

    /// Run the platform's package manager to install librubberband, then
    /// `cargo build --release --features rubberband`, then trigger restart.
    /// All non-blocking; progress via toast.
    pub(crate) fn spawn_install_rubberband(&mut self) {
        self.toast
            .show("Installing rubberband — this may take a minute…", 8.0);
        let tx = self.action_tx.clone();
        tokio::spawn(async move {
            crate::platform::install_rubberband(move |msg| {
                let _ = tx.send(AppAction::Toast(msg.to_string()));
            });
        });
    }

    /// Get filtered indices for the current screen.
    pub(crate) fn filtered_indices(&self) -> Vec<usize> {
        if self.filter_text.is_empty() {
            return (0..self.current_screen().item_count()).collect();
        }
        let filter = self.filter_text.to_lowercase();
        (0..self.current_screen().item_count())
            .filter(|&i| {
                self.current_screen()
                    .item_label(i)
                    .to_lowercase()
                    .contains(&filter)
            })
            .collect()
    }

    pub(crate) fn filtered_item_count(&self) -> usize {
        self.filtered_indices().len()
    }

    pub(crate) fn open_in_browser(&mut self) {
        let url = match self.current_screen() {
            BrowseScreen::TrackList { tracks, .. } => {
                if let Some(track) = tracks.get(self.selected) {
                    match self.selected_column {
                        0 => {
                            // Artist
                            track.artists.first().map(|a| {
                                format!(
                                    "https://www.beatport.com/artist/{}/{}",
                                    a.name.to_lowercase().replace(' ', "-"),
                                    a.id
                                )
                            })
                        }
                        1 => {
                            // Remixer
                            track.remixers.first().map(|r| {
                                format!(
                                    "https://www.beatport.com/artist/{}/{}",
                                    r.name.to_lowercase().replace(' ', "-"),
                                    r.id
                                )
                            })
                        }
                        2 => {
                            // Label
                            track.label_id.map(|lid| {
                                let slug = track
                                    .label_name
                                    .as_deref()
                                    .unwrap_or("label")
                                    .to_lowercase()
                                    .replace(' ', "-");
                                format!("https://www.beatport.com/label/{slug}/{lid}")
                            })
                        }
                        3 => {
                            // Genre
                            track.genre_id.map(|gid| {
                                let slug = track.genre_slug.as_deref().unwrap_or("genre");
                                format!("https://www.beatport.com/genre/{slug}/{gid}")
                            })
                        }
                        4 => {
                            // Release
                            track
                                .release_id
                                .map(|rid| format!("https://www.beatport.com/release/r/{rid}"))
                        }
                        _ => {
                            // Title or whole row → open track
                            Some(format!("https://www.beatport.com/track/t/{}", track.id))
                        }
                    }
                } else {
                    None
                }
            }
            BrowseScreen::ArtistList { artists, .. } => artists.get(self.selected).map(|a| {
                let slug = a.name.to_lowercase().replace(' ', "-");
                format!("https://www.beatport.com/artist/{slug}/{}", a.id)
            }),
            BrowseScreen::LabelList { labels, .. } => labels.get(self.selected).map(|l| {
                let slug = l.name.to_lowercase().replace(' ', "-");
                format!("https://www.beatport.com/label/{slug}/{}", l.id)
            }),
            BrowseScreen::ReleaseList { releases, .. } => releases
                .get(self.selected)
                .map(|r| format!("https://www.beatport.com/release/r/{}", r.id)),
            BrowseScreen::GenreList { genres, .. } => genres
                .get(self.selected)
                .map(|g| format!("https://www.beatport.com/genre/{}/{}", g.slug, g.id)),
            _ => None,
        };

        if let Some(url) = url {
            let browser = &self.config.browser;
            let app = match browser.as_str() {
                "Safari" => "Safari",
                "Firefox" => "Firefox",
                "Arc" => "Arc",
                _ => "Google Chrome",
            };
            match std::process::Command::new("open")
                .args(["-a", app, &url])
                .spawn()
            {
                Ok(_) => self.toast.show(&format!("Opening in {app}"), 1.5),
                Err(e) => self.toast.show(&format!("Failed to open: {e}"), 2.0),
            }
        } else {
            self.toast.show("Nothing to open", 1.0);
        }
    }

    /// Navigate to a genre/artist/label with canonical breadcrumb path.
    /// Jump to a 3-level browse path: root → list → detail. The middle
    /// list is a placeholder (real contents would arrive via API fetch
    /// after the user hits Esc/Back). Used for direct drill-in to an
    /// artist/label/genre from a track column.
    /// Pop to browse root and drill-in along a slash-separated label path.
    /// Shared by IPC `Browse` and the `TestMix` harness. Reports via toast
    /// whether each segment matched. Returns true if all segments resolved.
    /// Start drilling into a slash-separated browse path from the root.
    /// Segments are processed one at a time — the first fires immediately,
    /// the rest are parked in `browse_path_state` and driven from the
    /// tick loop as each async screen-push lands. Critical for API-backed
    /// lists (Genres → Techno → Top 100) where drilling all segments
    /// synchronously races the async loader.
    pub(crate) fn browse_path(&mut self, segments: &[&str]) -> bool {
        // Reset to the root screen — clears parent-cursor history too,
        // since we're about to drill in fresh from a CLI/IPC path.
        while self.screen_stack.len() > 1 {
            self.screen_stack.pop();
        }
        self.screen_stack_back.clear();
        self.selected = 0;
        self.view_mode = ViewMode::Browse;
        let mut iter = segments.iter();
        let Some(first) = iter.next() else {
            return true;
        };
        let before = self.screen_stack.len();
        if !self.browse_path_select(first) {
            return false;
        }
        let remaining: Vec<String> = iter.map(|s| s.to_string()).collect();
        if remaining.is_empty() {
            self.browse_path_state = None;
            return true;
        }
        self.browse_path_state = Some(BrowsePathState {
            remaining,
            before_depth: before,
            waited: 0,
        });
        true
    }

    /// Select an item on the current screen whose label matches `segment`
    /// (case-insensitive). Prefers exact match so "Techno" doesn't hit
    /// "Melodic House & Techno" first; falls back to `contains` only
    /// when no exact match is found.
    pub(crate) fn browse_path_select(&mut self, segment: &str) -> bool {
        let lower = segment.to_lowercase();
        let count = self.current_screen().item_count();
        let exact =
            (0..count).find(|&i| self.current_screen().item_label(i).to_lowercase() == lower);
        let found = exact.or_else(|| {
            (0..count).find(|&i| {
                self.current_screen()
                    .item_label(i)
                    .to_lowercase()
                    .contains(&lower)
            })
        });
        match found {
            Some(idx) => {
                self.selected = idx;
                self.handle_browse_enter();
                true
            }
            None => {
                self.toast.show(&format!("Not found: {segment}"), 1.0);
                false
            }
        }
    }

    /// Tick-loop body: advance the browse-path state machine if one is
    /// active. No-op when no path is running. Waits until the most
    /// recent segment's screen has been pushed (either synchronously or
    /// via an async `AppAction::PushScreen`) before firing the next
    /// segment. Times out after ~3s so a failed load doesn't hang state.
    pub(crate) fn browse_path_tick(&mut self) {
        let Some(mut state) = self.browse_path_state.take() else {
            return;
        };
        let depth = self.screen_stack.len();
        if depth <= state.before_depth {
            state.waited += 1;
            // 180 ticks ≈ 3 s at 60 Hz.
            if state.waited > 180 {
                self.toast.show("Browse path timed out", 2.0);
                return; // drop state
            }
            self.browse_path_state = Some(state);
            return;
        }
        // The previous segment's push has arrived. Fire the next one
        // (if any) and record the pre-fire depth so the tick after this
        // can detect its push.
        if state.remaining.is_empty() {
            return;
        }
        let segment = state.remaining.remove(0);
        let before = self.screen_stack.len();
        if self.browse_path_select(&segment) {
            state.before_depth = before;
            state.waited = 0;
            if !state.remaining.is_empty() {
                self.browse_path_state = Some(state);
            }
        }
        // else: select failed (already toasted); drop state.
    }

    pub(crate) fn navigate_to_detail(&mut self, list: BrowseScreen, detail: BrowseScreen) {
        self.screen_stack = vec![
            catalog::root_screen_v3(
                !self.config.local_library_dir.is_empty(),
                !self.config.rekordbox_xml.is_empty(),
                !self.config.engine_dj_db.is_empty(),
                !self.config.serato_db.is_empty(),
            ),
            list,
            detail,
        ];
        self.selected = 0;
        self.scroll_offset = 0;
        self.selected_column = -2;
    }

    pub(crate) fn navigate_to_genre(&mut self, genre_id: i64, name: &str) {
        self.navigate_to_detail(
            BrowseScreen::GenreList {
                title: "Genres".into(),
                genres: Vec::new(),
            },
            catalog::genre_detail_screen(genre_id, name),
        );
    }

    pub(crate) fn navigate_to_artist(&mut self, artist_id: i64, name: &str) {
        self.navigate_to_detail(
            BrowseScreen::ArtistList {
                title: "Artists".into(),
                artists: Vec::new(),
            },
            catalog::artist_detail_screen(artist_id, name),
        );
    }

    pub(crate) fn navigate_to_label(&mut self, label_id: i64, name: &str) {
        self.navigate_to_detail(
            BrowseScreen::LabelList {
                title: "Labels".into(),
                labels: Vec::new(),
            },
            catalog::label_detail_screen(label_id, name),
        );
    }

    pub(crate) fn push_screen(&mut self, screen: BrowseScreen) {
        // Save cursor state of the screen we're leaving so Esc can
        // restore it. Without this, popping back lands at the top of
        // the parent list — losing the user's place in the chart.
        self.screen_stack_back.push((
            self.selected,
            self.scroll_offset,
            self.selected_column,
            self.dash_browse_sel,
        ));
        self.screen_stack.push(screen);
        self.selected = 0;
        self.scroll_offset = 0;
        self.selected_column = -2;
        self.filter_text.clear();
        self.filtering = false;
    }

    /// Pop one screen from the navigation stack, restoring the
    /// cursor positions that were active before the drill-in.
    /// Returns true if a screen was popped.
    pub(crate) fn pop_screen(&mut self) -> bool {
        if self.screen_stack.len() <= 1 {
            return false;
        }
        self.screen_stack.pop();
        if let Some((sel, scroll, col, dash_sel)) = self.screen_stack_back.pop() {
            self.selected = sel;
            self.scroll_offset = scroll;
            self.selected_column = col;
            self.dash_browse_sel = dash_sel;
        } else {
            // Stack desync — fall back to the old reset behaviour.
            self.selected = 0;
            self.scroll_offset = 0;
            self.selected_column = -2;
            self.dash_browse_sel = 0;
        }
        true
    }

    pub(crate) fn execute_menu_action(&mut self, action: MenuAction) {
        // Check if it's a static push (no API needed)
        // Favorites — handled locally, not via catalog
        if matches!(action, MenuAction::PushFavorites) {
            let tracks = self.favorites.all_tracks();
            let count = tracks.len();
            let screen = BrowseScreen::TrackList {
                title: format!("Favorites ({count})"),
                tracks,
            };
            self.push_screen(screen);
            return;
        }

        // Local Library — push the folder-drill-down screen rooted at
        // the configured directory. Each subfolder is a Menu entry
        // that pushes a deeper folder; leaves resolve to a rich
        // TrackList (BPM/key/duration). See `local_library::folder_screen`
        // for the three-shape decision (only-folders / only-tracks /
        // mixed).
        if matches!(action, MenuAction::PushLocalLibrary) {
            let dir = self.config.local_library_dir.clone();
            if dir.is_empty() {
                self.toast.show(
                    "Local Library: set Settings → Local Library Directory first",
                    3.0,
                );
                return;
            }
            let root = std::path::PathBuf::from(&dir);
            if !root.is_dir() {
                self.toast
                    .show(&format!("Local Library: {dir} is not a directory"), 3.0);
                return;
            }
            let screen = crate::local_library::folder_screen(&root, &root);
            self.push_screen(screen);
            return;
        }

        // Drill into a sub-folder of the local library.
        if let MenuAction::PushLocalFolder(path) = &action {
            let root = std::path::PathBuf::from(&self.config.local_library_dir);
            let screen = crate::local_library::folder_screen(&root, path);
            self.push_screen(screen);
            return;
        }

        // "Tracks here (N)" — the audio files at exactly one folder
        // level, rendered as a rich TrackList.
        if let MenuAction::LoadLocalFolderTracks(path) = &action {
            let (_folders, tracks) = crate::local_library::list_folder(path);
            let count = tracks.len();
            let title = folder_track_list_title(path, &self.config.local_library_dir, count);
            self.push_screen(BrowseScreen::TrackList { title, tracks });
            return;
        }

        // "All tracks (recursive)" — every audio file under the given
        // folder, flat-listed. Same shape as the legacy
        // `PushLocalLibrary` flat dump, but scoped to any folder.
        if let MenuAction::LoadLocalLibraryRecursive(path) = &action {
            let tracks = crate::local_library::scan_library(path);
            let count = tracks.len();
            let title = folder_track_list_title(path, &self.config.local_library_dir, count);
            self.push_screen(BrowseScreen::TrackList {
                title: format!("{title} (recursive)"),
                tracks,
            });
            return;
        }

        // Rekordbox — parse the configured XML export. Synchronous
        // since rekordbox.xml is typically <50MB and parses in
        // under a second; large libraries may take longer (toast
        // covers it).
        if matches!(action, MenuAction::PushRekordbox) {
            let xml = self.config.rekordbox_xml.clone();
            let path = std::path::Path::new(&xml);
            match crate::library_import::import_rekordbox_xml(path) {
                Ok(tracks) => {
                    let count = tracks.len();
                    let screen = BrowseScreen::TrackList {
                        title: format!("Rekordbox ({count})"),
                        tracks,
                    };
                    self.push_screen(screen);
                }
                Err(e) => {
                    self.toast
                        .show(&format!("Rekordbox import failed: {e}"), 3.0);
                }
            }
            return;
        }

        // Engine DJ — parse the SQLite m.db at the configured path.
        if matches!(action, MenuAction::PushEngineDj) {
            let db = self.config.engine_dj_db.clone();
            let path = std::path::Path::new(&db);
            match crate::library_import::import_engine_db(path) {
                Ok(tracks) => {
                    let count = tracks.len();
                    self.push_screen(BrowseScreen::TrackList {
                        title: format!("Engine DJ ({count})"),
                        tracks,
                    });
                }
                Err(e) => self
                    .toast
                    .show(&format!("Engine DJ import failed: {e}"), 3.0),
            }
            return;
        }

        // Serato — parse the binary `database V2` at the configured
        // path. Same shape as Engine DJ on the UI side.
        if matches!(action, MenuAction::PushSerato) {
            let db = self.config.serato_db.clone();
            let path = std::path::Path::new(&db);
            match crate::library_import::import_serato_database(path) {
                Ok(tracks) => {
                    let count = tracks.len();
                    self.push_screen(BrowseScreen::TrackList {
                        title: format!("Serato ({count})"),
                        tracks,
                    });
                }
                Err(e) => self.toast.show(&format!("Serato import failed: {e}"), 3.0),
            }
            return;
        }

        // USB stick — auto-detected via usb_libraries; the action
        // payload carries the mount point, so we can route to the
        // right importer based on what's at that mount.
        if let MenuAction::PushUsbStick(mount) = &action {
            // Find which kind it is in the current detected list.
            let kind = crate::usb_libraries::detected_sticks()
                .into_iter()
                .find(|s| &s.mount == mount)
                .map(|s| s.kind);
            let result = match kind {
                Some(crate::usb_libraries::StickKind::EngineDj) => {
                    // Try the standard + legacy paths.
                    let candidates = [
                        mount.join("Engine Library/Database2/m.db"),
                        mount.join("Engine Library/m.db"),
                        mount.join("m.db"),
                    ];
                    candidates
                        .iter()
                        .find(|p| p.exists())
                        .ok_or_else(|| anyhow::anyhow!("no Engine DB at {}", mount.display()))
                        .and_then(|p| crate::library_import::import_engine_db(p))
                }
                Some(crate::usb_libraries::StickKind::Rekordbox) => {
                    let pdb = mount.join("PIONEER/rekordbox/export.pdb");
                    if pdb.exists() {
                        crate::library_import::import_rekordbox_pdb(&pdb)
                    } else {
                        Err(anyhow::anyhow!("no export.pdb at {}", pdb.display()))
                    }
                }
                None => Err(anyhow::anyhow!(
                    "Stick {} no longer detected",
                    mount.display()
                )),
            };
            match result {
                Ok(tracks) => {
                    let count = tracks.len();
                    let label = mount.file_name().and_then(|n| n.to_str()).unwrap_or("USB");
                    self.push_screen(BrowseScreen::TrackList {
                        title: format!("USB: {label} ({count})"),
                        tracks,
                    });
                }
                Err(e) => self.toast.show(&format!("USB import failed: {e}"), 3.0),
            }
            return;
        }

        let static_screen = match &action {
            MenuAction::PushDiscover => Some(catalog::discover_screen()),
            MenuAction::PushTrending => Some(catalog::trending_screen()),
            MenuAction::PushGenreTrending(id, name) => {
                Some(catalog::genre_trending_screen(*id, name))
            }
            MenuAction::PushDecades => Some(catalog::decades_screen(None)),
            MenuAction::PushGenreDecades(gid) => Some(catalog::decades_screen(Some(*gid))),
            MenuAction::PushDecade(range, name, gid) => {
                Some(catalog::decade_detail_screen(name, range, *gid))
            }
            MenuAction::PushDecadeYears(name, range, gid) => {
                Some(catalog::decade_years_screen(name, range, *gid))
            }
            MenuAction::PushYear(name, range, gid) => {
                Some(catalog::year_detail_screen(name, range, *gid))
            }
            MenuAction::PushMyBeatport => Some(catalog::my_beatport_screen()),
            MenuAction::PushMyLibrary => Some(catalog::my_library_screen()),
            _ => None,
        };

        if let Some(screen) = static_screen {
            self.push_screen(screen);
            return;
        }

        // Needs API call — spawn async
        let Some(api) = self.require_api() else {
            return;
        };

        // Store action for Load More pagination
        self.last_load_action = Some(action.clone());
        self.current_page = 1;

        let tx = self.action_tx.clone();
        self.toast.show("Loading...", 2.0);

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match catalog::execute_action(&action, &mut api).await {
                Ok(Some(screen)) => {
                    tx.send(AppAction::PushScreen(screen)).ok();
                }
                Ok(None) => {}
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                }
            }
        });
    }

    pub(crate) fn trigger_search(&mut self) {
        let Some(api) = self.require_api() else {
            return;
        };
        let query = self.search_query.clone();
        let tx = self.action_tx.clone();
        self.toast.show(&format!("Searching: {query}"), 1.0);

        tokio::spawn(async move {
            let mut api = api.lock().await;
            match api.search(&query).await {
                Ok(tracks) => {
                    let screen = BrowseScreen::TrackList {
                        title: format!("Search: {query}"),
                        tracks,
                    };
                    tx.send(AppAction::PushScreen(screen)).ok();
                }
                Err(e) => {
                    tx.send(AppAction::Toast(format!("Search failed: {e}")))
                        .ok();
                }
            }
        });
    }

    pub(crate) fn download_for_preview(&mut self, track: BeatportTrack) {
        let Some(api) = self.require_api() else {
            return;
        };
        let name = format!("{} - {}", track.artist_name(), track.full_title());
        self.toast.show(&format!("Loading preview: {name}"), 2.0);
        super::download::download_for_preview(
            super::download::Pipeline {
                api,
                downloader: Arc::clone(&self.downloader),
                tx: self.action_tx.clone(),
                quality: self.config.audio_quality,
                ai_beat: false,
                ai_grid: false,
                ai_phrases: false,
                analyzer_engine: self.config.analyzer_engine,
            },
            track,
        );
    }

    pub(crate) fn download_and_play(
        &mut self,
        track: std::sync::Arc<BeatportTrack>,
        as_incoming: bool,
    ) {
        if self.download_in_flight {
            tracing::info!(
                "Download BLOCKED — already in flight: {} - {}",
                track.artist_name(),
                track.full_title()
            );
            self.engine
                .enqueue(crate::audio::engine::QueueEntry { track });
            return;
        }
        let track: BeatportTrack =
            std::sync::Arc::try_unwrap(track).unwrap_or_else(|a| (*a).clone());
        self.download_in_flight = true;
        tracing::info!(
            "Download START: {} - {} (as_incoming={as_incoming})",
            track.artist_name(),
            track.full_title()
        );

        let Some(api) = self.api.clone() else {
            self.download_in_flight = false;
            self.toast.show("Not authenticated", 2.0);
            return;
        };
        super::download::download_and_play(
            super::download::Pipeline {
                api,
                downloader: Arc::clone(&self.downloader),
                tx: self.action_tx.clone(),
                quality: self.config.audio_quality,
                ai_beat: self.config.ai_beat_detection,
                ai_grid: self.config.ai_grid_validation,
                ai_phrases: self.config.ai_phrase_detection,
                analyzer_engine: self.config.analyzer_engine,
            },
            track,
            as_incoming,
        );
    }

    pub async fn handle_action(&mut self, action: AppAction) {
        match action {
            AppAction::Toast(msg) => self.toast.show(&msg, 2.0),
            AppAction::ShowPlaylistPicker {
                track_id,
                playlists,
            } => {
                self.playlist_picker = Some(PlaylistPickerState {
                    playlists,
                    track_id,
                    selected: 0,
                    new_name: String::new(),
                });
                self.view_mode = ViewMode::PlaylistPicker;
            }
            AppAction::ShowGenrePicker { favorites, genres } => {
                self.genre_picker = Some(GenrePickerState {
                    genres,
                    selected: 0,
                    scroll_offset: 0,
                });
                self.view_mode = if favorites {
                    ViewMode::FavoritesPicker
                } else {
                    ViewMode::GenrePicker
                };
            }
            AppAction::PushScreen(mut screen) => {
                // Sort favorite genres to top
                if let BrowseScreen::GenreList { ref mut genres, .. } = screen {
                    let favs = &self.config.favorite_genres;
                    if !favs.is_empty() {
                        genres.sort_by(|a, b| {
                            let a_fav = favs.iter().any(|f| f.eq_ignore_ascii_case(&a.name));
                            let b_fav = favs.iter().any(|f| f.eq_ignore_ascii_case(&b.name));
                            b_fav.cmp(&a_fav)
                        });
                    }
                }
                let title = screen.title().to_string();
                let count = screen.item_count();
                self.push_screen(screen);
                self.current_page = 1;
                // Switch to browse mode if we were searching
                if matches!(self.view_mode, ViewMode::Search) {
                    self.view_mode = ViewMode::Browse;
                }
                self.toast.show(&format!("{title}: {count} items"), 1.0);
            }
            AppAction::AppendTracks(new_tracks) => {
                let count = new_tracks.len();
                if let Some(BrowseScreen::TrackList { tracks, .. }) = self.screen_stack.last_mut() {
                    tracks.extend(new_tracks);
                    self.toast
                        .show(&format!("+{count} tracks ({} total)", tracks.len()), 1.0);
                }
            }
            AppAction::AppendCharts(new_charts) => {
                let count = new_charts.len();
                if let Some(BrowseScreen::ChartList { charts, .. }) = self.screen_stack.last_mut() {
                    charts.extend(new_charts);
                    self.toast
                        .show(&format!("+{count} charts ({} total)", charts.len()), 1.0);
                }
            }
            AppAction::AppendReleases(new_releases) => {
                let count = new_releases.len();
                if let Some(BrowseScreen::ReleaseList { releases, .. }) =
                    self.screen_stack.last_mut()
                {
                    releases.extend(new_releases);
                    self.toast.show(
                        &format!("+{count} releases ({} total)", releases.len()),
                        1.0,
                    );
                }
            }
            AppAction::TrackDecoded {
                track,
                samples,
                sample_rate,
                analysis,
                as_incoming,
            } => {
                self.download_in_flight = false;
                let name = format!("{} - {}", track.artist_name(), track.full_title());
                // Session resume overrides the normal start_time with the
                // track's saved source-time position, so a relaunch drops
                // the user back where they were instead of at the intro.
                let resume_pos = self.resume_positions.remove(&track.id);
                let start_time = if let Some(raw) = resume_pos {
                    // Clamp to a safe pre-end range. If we saved at
                    // (or very near) the end of a track — which is
                    // what happens when the previous session quit
                    // mid-mix or after a track played out — seeking
                    // there would put position past samples.len() on
                    // the first fill and immediately flip playing
                    // to false. Cap at duration − 30s, fall back to
                    // first_beat for tiny tracks. 30s is roughly the
                    // typical mix runway so resume drops you "before
                    // the next mix" rather than in the dead zone.
                    let duration = analysis.beat_grid.bpm.max(1.0); // placeholder, real below
                    let _ = duration;
                    let total = (samples.len() as f64) / (sample_rate as f64);
                    let safe_end = (total - 30.0).max(0.0);
                    let pos = if raw > safe_end {
                        analysis.beat_grid.first_beat_time
                    } else {
                        raw
                    };
                    tracing::info!(
                        "Resume: seeking {name} to {pos:.1}s (raw saved = {raw:.1}s, duration {total:.1}s)"
                    );
                    pos
                } else if as_incoming {
                    let playing_bpm = self.cached_info.playing_bpm.unwrap_or(128.0);
                    let incoming_bpm = analysis.beat_grid.bpm;
                    let playing_key = self
                        .cached_info
                        .playing_track
                        .as_ref()
                        .and_then(|t| t.key.clone());
                    let incoming_key = track.key.clone();
                    let transition = crate::audio::transition::TransitionType::choose(
                        playing_bpm,
                        incoming_bpm,
                        playing_key.as_deref(),
                        incoming_key.as_deref(),
                    );
                    if transition == crate::audio::transition::TransitionType::EchoOut {
                        analysis.first_audio
                    } else {
                        analysis.beat_grid.first_beat_time
                    }
                } else {
                    analysis.first_audio
                };
                if as_incoming {
                    self.engine
                        .load_incoming(samples, sample_rate, analysis, track, start_time);
                    self.toast.show(&format!("Loaded: {name}"), 1.5);
                } else {
                    self.engine
                        .play_track(samples, sample_rate, analysis, track, start_time);
                    self.toast.show(&format!("Playing: {name}"), 2.0);
                    // Auto-switch to the dashboard on the first-play
                    // transition. Once audio starts you usually want to
                    // watch the decks, not stay on whatever browse/queue/
                    // settings panel triggered the play. Skipped for
                    // subsequent tracks (they arrive via crossfade, which
                    // is triggered from any view without needing a
                    // mode change).
                    if matches!(
                        self.view_mode,
                        ViewMode::Browse
                            | ViewMode::Queue
                            | ViewMode::History
                            | ViewMode::Settings
                            | ViewMode::Search
                    ) {
                        self.view_mode = ViewMode::Dashboard;
                        self.dash_focus = DashFocus::Controller;
                    }
                }
            }
            AppAction::PreviewReady {
                samples,
                sample_rate,
                analysis,
            } => {
                let bpm = analysis.beat_grid.bpm;
                let first_beat = analysis.beat_grid.first_beat_time;
                self.engine.preview_track(samples, sample_rate, analysis);
                self.toast.show(
                    &format!("Preview: {bpm:.0} BPM, beat at {first_beat:.3}s"),
                    3.0,
                );
            }
            AppAction::DjToolCalls(tools) => {
                // Execute each tool and collect results
                let mut results: Vec<(String, String)> = Vec::new();
                for tool in &tools {
                    let output = self.execute_dj_tool(tool);
                    results.push((tool.id.clone(), output));
                }
                // If there were tool calls, send results back for continuation
                if !results.is_empty() {
                    let tx = self.action_tx.clone();
                    tx.send(AppAction::DjContinue(results)).ok();
                }
            }
            AppAction::DjContinue(results) => {
                self.continue_dj(results);
            }
            AppAction::AlignmentResult {
                nudge_ms,
                is_aligned,
                rate_correction,
                details,
            } => {
                if !is_aligned && nudge_ms.abs() > 2.0 {
                    // Positive nudge_ms = incoming is early = nudge backward
                    let dir = if nudge_ms > 0.0 { -1 } else { 1 };
                    let nudge_count = (nudge_ms.abs() / 5.0).ceil() as i32;
                    for _ in 0..nudge_count.min(5) {
                        self.engine.nudge(dir);
                    }
                    self.toast.show(
                        &format!("Mix: aligned ({nudge_ms:+.1}ms nudge applied)"),
                        2.0,
                    );
                } else {
                    self.toast
                        .show(&format!("Mix: already aligned ({nudge_ms:+.1}ms)"), 2.0);
                }
                if let Some(rc) = rate_correction
                    && (rc - 1.0).abs() > 0.001
                {
                    tracing::info!(
                        "AI rate correction suggested: {rc:.4} (not auto-applied, PLL manages)"
                    );
                }
                tracing::info!("AI alignment: {details}");
            }
            AppAction::DownloadFailed(msg) => {
                self.download_in_flight = false;
                self.toast.show(&msg, 3.0);
            }
        }
    }

    /// Apply CLI startup args — navigate, queue, search, etc.
    pub async fn apply_cli_args(&mut self, cli: &crate::CliArgs) {
        // --browse "Genres/Techno/Top 100" — navigate the path
        if let Some(path) = &cli.browse {
            let segments: Vec<&str> = path.split('/').collect();
            for segment in segments {
                let screen = self.current_screen().clone();
                let count = screen.item_count();
                // Find matching item (case-insensitive)
                let found = (0..count).find(|&i| {
                    screen
                        .item_label(i)
                        .to_lowercase()
                        .contains(&segment.to_lowercase())
                });
                if let Some(idx) = found {
                    self.selected = idx;
                    self.handle_browse_enter();
                    // Wait for async screen loads
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    // Process any pending actions
                    // (handled by main loop, but we can't access the channel here)
                } else {
                    tracing::warn!("Browse: '{segment}' not found");
                    break;
                }
            }
        }

        // --play — queue a genre chart
        if cli.play {
            let genre = cli.genre.as_deref().unwrap_or_else(|| {
                if !self.config.favorite_genres.is_empty() {
                    // Pick random favorite
                    &self.config.favorite_genres[0]
                } else {
                    &self.config.default_genre
                }
            });
            self.toast.show(&format!("Loading {genre} charts..."), 3.0);

            if let Some(api) = self.api.clone() {
                let tx = self.action_tx.clone();
                let genre = genre.to_string();
                tokio::spawn(async move {
                    let mut api = api.lock().await;
                    match api.genres().await {
                        Ok(genres) => {
                            if let Some(g) =
                                genres.iter().find(|g| g.name.eq_ignore_ascii_case(&genre))
                            {
                                match api.genre_top_100(g.id).await {
                                    Ok(tracks) => {
                                        let title = format!("{} Top 100", g.name);
                                        tx.send(AppAction::PushScreen(
                                            catalog::BrowseScreen::TrackList { title, tracks },
                                        ))
                                        .ok();
                                    }
                                    Err(e) => {
                                        tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                                    }
                                }
                            } else {
                                tx.send(AppAction::Toast(format!("Genre '{genre}' not found")))
                                    .ok();
                            }
                        }
                        Err(e) => {
                            tx.send(AppAction::Toast(format!("Error: {e}"))).ok();
                        }
                    }
                });
            }
        }

        // --search "query"
        if let Some(query) = &cli.search {
            self.search_query = query.clone();
            self.view_mode = ViewMode::Search;
            self.trigger_search();
        }

        // --dashboard
        if cli.dashboard || cli.play {
            self.view_mode = ViewMode::Dashboard;
        }
    }

    pub async fn tick(&mut self) {
        // Advance any in-flight browse-path state machine first. This
        // runs before engine/IPC work so a still-loading drill-in has a
        // chance to see freshly-arrived screens and fire the next segment
        // within the same tick.
        self.browse_path_tick();

        // Right-click → MIDI map: while a mapping is pending and we
        // haven't captured an event yet, watch the listener for the
        // first incoming event. Once captured, freeze it on the
        // pending state and stop polling — Y/Esc handles the rest.
        if let Some(pm) = &mut self.pending_midi_map {
            if pm.captured.is_none() {
                if let Some(midi) = &self.midi {
                    if let Ok(state) = midi.lock() {
                        if let Some((event, _value)) = state.last_event.as_ref() {
                            pm.captured = Some(event.clone());
                            self.toast.show(
                                &format!(
                                    "Captured: {} → {}\nPress Y to save, Esc to cancel",
                                    event.label(),
                                    pm.action.label()
                                ),
                                10.0,
                            );
                        }
                    }
                }
            }
        }

        // Engine tick
        let events = self.engine.tick(&self.config);
        for event in events {
            match event {
                EngineEvent::NeedFirstTrack(entry) => {
                    self.download_and_play(entry.track, false);
                }
                EngineEvent::NeedNextTrack(entry) => {
                    self.download_and_play(entry.track, true);
                }
                EngineEvent::PlaybackEnded => {
                    self.toast.show("Playback ended", 2.0);
                }
                EngineEvent::CrossfadeComplete { track, bpm } => {
                    tracing::info!(
                        "Finished: {} - {} ({bpm:.0} BPM)",
                        track.artist_name(),
                        track.full_title()
                    );
                    // Snapshot the finished mix for training feedback.
                    // `track` is the one that just finished (outgoing);
                    // the new playing track is whatever the engine
                    // swapped to. Read it fresh since cached_info hasn't
                    // refreshed for this tick yet.
                    let new_playing = self.engine.now_playing();
                    let incoming_title = new_playing
                        .playing_track
                        .as_ref()
                        .map(|t| format!("{} - {}", t.artist_name(), t.full_title()))
                        .unwrap_or_else(|| "?".into());
                    let outgoing_title =
                        format!("{} - {}", track.artist_name(), track.full_title());
                    let inc_bpm = new_playing.playing_bpm.unwrap_or(0.0);
                    let key_pair = match (
                        track.key.as_deref(),
                        new_playing
                            .playing_track
                            .as_ref()
                            .and_then(|t| t.key.as_deref()),
                    ) {
                        (Some(a), Some(b)) => Some(format!("{a}→{b}")),
                        _ => None,
                    };
                    let mix_entry = crate::claude::memory::MixEntry {
                        pair: format!("{outgoing_title} → {incoming_title}"),
                        bpm: Some([bpm, inc_bpm]),
                        key: key_pair,
                        transition: Some(new_playing.transition_type_name.to_string()),
                        note: None,
                        rated_at: None,
                    };
                    self.last_mix_entry = Some(mix_entry.clone());
                    // Queue for the History-view rate-from-list path —
                    // drained once cached_info catches up to the new
                    // history.len() in the post-tick sync below.
                    self.pending_mix_entries.push_back(mix_entry);
                }
                EngineEvent::TrainWreckDetected { rms_ms, bailed } => {
                    let msg = if bailed {
                        format!("⚠ Train wreck — bailed to EchoOut (RMS {rms_ms:.0}ms)")
                    } else {
                        format!("⚠ Train wreck detected (RMS {rms_ms:.0}ms)")
                    };
                    self.toast.show(&msg, 4.0);
                }
                EngineEvent::AutoMixPaused => {
                    self.toast
                        .show("Auto-mix paused — resumes after this track or :auto", 2.5);
                }
            }
        }

        // IPC: drain queued commands. read_command is atomic-rename
        // safe and may return multiple commands when callers append
        // newline-delimited JSON between ticks.
        for cmd_json in crate::ipc::read_command() {
            for cmd in crate::ipc::parse_command(&cmd_json) {
                self.handle_ipc_command(cmd);
            }
        }

        // Refresh cached NowPlayingInfo once per tick (avoids locking in render/handle_key)
        self.cached_info = self.engine.now_playing();
        // Sync the parallel mix_entries Vec to history. New history
        // entries with a mix_score were arrived-at via a crossfade —
        // pull the matching MixEntry from pending_mix_entries (built
        // in CrossfadeComplete above). Score=None means an opening
        // track with no preceding mix → no rating data.
        while self.mix_entries.len() < self.cached_info.history.len() {
            let idx = self.mix_entries.len();
            let mix = if self.cached_info.history[idx].mix_score.is_some() {
                self.pending_mix_entries
                    .pop_front()
                    .map(|entry| HistoryMix {
                        entry,
                        rated: None,
                        rated_at: None,
                    })
            } else {
                None
            };
            self.mix_entries.push(mix);
        }
        // IPC: write status + screen dump every 2s. Build the preview list
        // only when the writer actually wants it — saves 20× item_label()
        // string allocations per tick on the 29/30 ticks where nothing writes.
        let screen_title = self.current_screen().title().to_string();
        let item_count = self.current_screen().item_count();
        if self.status_writer.needs_write() {
            let screen_items: Vec<String> = (0..item_count.min(20))
                .map(|i| self.current_screen().item_label(i))
                .collect();
            let dash_focus_label = match self.dash_focus {
                DashFocus::Controller => "Controller",
                DashFocus::Queue => "Queue",
                DashFocus::History => "History",
                DashFocus::Browse => "Browse",
                DashFocus::Log => "Log",
            };
            self.status_writer.maybe_write(
                &self.cached_info,
                &screen_title,
                &screen_items,
                self.toast.peek(),
                Some(self.dash_section.label()),
                Some(dash_focus_label),
            );
        }
        // Quick status: blocking fs::write every 250ms instead of every tick.
        // Good enough for external scripts polling quick.txt; avoids 60Hz
        // disk I/O on the event loop.
        if self.last_quick_status.elapsed() >= std::time::Duration::from_millis(250) {
            let view_name = match self.view_mode {
                ViewMode::Browse => "browse",
                ViewMode::Dashboard => "dashboard",
                ViewMode::Queue => "queue",
                ViewMode::History => "history",
                ViewMode::Settings => "settings",
                ViewMode::Help => "help",
                ViewMode::Search => "search",
                _ => "other",
            };
            crate::ipc::write_quick_status(&self.cached_info, view_name);
            self.last_quick_status = std::time::Instant::now();
        }

        // Session snapshot: write ~/.mixr/session.json every 5s when
        // the engine has something worth persisting. Skipped while
        // the startup resume prompt is pending so we don't clobber
        // the file we're asking the user to restore from.
        if !self.pending_resume_prompt
            && self.last_session_save.elapsed() >= std::time::Duration::from_secs(5)
        {
            if let Some(snap) = self.engine.session_snapshot()
                && let Err(e) = crate::session::save(&snap)
            {
                tracing::warn!("Session save failed: {e}");
            }
            self.last_session_save = std::time::Instant::now();
        }

        // Claude DJ auto-triggers
        if let Some(ref dj) = self.claude_dj {
            let (dj_enabled, can_call) = match dj.try_lock() {
                Ok(dj) => (dj.is_enabled(), dj.can_call()),
                Err(_) => (false, false), // DJ is busy with an API call
            };
            // Suppress new triggers while a previous chain is still
            // in flight — overlapping conversations cancel-and-restart
            // each other, exhausting the Anthropic input-token rate
            // limit (50k tokens/min on Haiku).
            let dj_busy = self
                .claude_dj
                .as_ref()
                .and_then(|d| d.try_lock().ok().map(|d| d.is_busy()))
                .unwrap_or(false);
            if dj_enabled && can_call && !dj_busy {
                let state = self.cached_info.state;
                // Trigger on crossfade start (state change to Crossfading)
                if state == crate::audio::engine::EngineState::Crossfading
                    && self.last_engine_state != crate::audio::engine::EngineState::Crossfading
                {
                    // Reset the debounce so the manual-mode stall
                    // watchdog (below) gives Claude a full window to
                    // respond before re-nagging. Without this, the
                    // watchdog fires 1-3 s after the initial trigger
                    // because the elapsed clock is from an earlier
                    // queue-low trigger.
                    self.last_crossfade_trigger = std::time::Instant::now();
                    self.mix_checkpoints_fired = [false, false];
                    let manual = self.config.claude_dj.mode == crate::config::ClaudeDjMode::Manual;
                    // Direction is determined by which deck is playing.
                    // A playing → sweep TOWARD +1 (B side) to complete.
                    // B playing → sweep TOWARD −1 (A side) to complete.
                    // Without naming the direction explicitly Claude
                    // gets confused after each swap (the convention
                    // flips every mix) and either sweeps the wrong way
                    // or decides the mix "is already done."
                    let playing_is_a = self.cached_info.playing_is_a;
                    let target: f32 = if playing_is_a { 1.0 } else { -1.0 };
                    let playing = if playing_is_a { "A" } else { "B" };
                    let incoming = if playing_is_a { "B" } else { "A" };
                    let msg_owned;
                    let msg = if manual {
                        msg_owned = format!(
                            "Crossfade started — MANUAL MODE. Deck {playing} is playing \
                             live, deck {incoming} is cued. Make ONE call: \
                             sweep_crossfader(target={target:+}, bars=8). The engine \
                             paces the sweep over 8 bars; do NOT call set_crossfader \
                             repeatedly. When the sweep finishes the engine swaps \
                             decks — the mix is done. You may also call read_alignment \
                             first to spot 1s/phrase mismatches."
                        );
                        &*msg_owned
                    } else {
                        "Crossfade just started. Check phase alignment with read_phase — nudge if needed."
                    };
                    self.trigger_dj(msg);
                }
                // Trigger when queue is low. Debounce so we don't refire
                // every 2s while the DJ is mid-search; one trigger
                // every 30s is plenty (a single chain can queue several
                // tracks before exiting).
                //
                // Suppressed while Crossfading — otherwise the DJ abandons
                // a half-done manual crossfader sweep to go search for
                // tracks, leaving the mix stuck at the partial position.
                else if state != crate::audio::engine::EngineState::Crossfading
                    && self.cached_info.queue.len() < 3
                    && self.cached_info.playing_track.is_some()
                    && self.last_low_queue_trigger.elapsed() >= std::time::Duration::from_secs(30)
                {
                    self.last_low_queue_trigger = std::time::Instant::now();
                    self.trigger_dj("Queue is low — find and queue a track that flows well with what's playing.");
                }
                // Manual-mode watchdog: if we've been Crossfading for >5s
                // and the progress has barely moved, the DJ's previous
                // sweep chain got cut (rate limit, tool error, etc).
                // Re-trigger with an urgent reminder so it resumes the
                // sweep rather than silently leaving the mix stuck.
                // Mid-mix phase-check triggers at ~30% and ~70% progress
                // so Claude gets a natural reason to read_alignment +
                // nudge during the sweep. Each threshold fires at most
                // once per crossfade (gated by mix_checkpoints_fired).
                // Runs in manual OR assist mode; auto mode has the
                // rate-correction controller doing this itself.
                else if state == crate::audio::engine::EngineState::Crossfading
                    && matches!(
                        self.config.claude_dj.mode,
                        crate::config::ClaudeDjMode::Manual | crate::config::ClaudeDjMode::Assist
                    )
                    && {
                        let p = self.cached_info.crossfade_progress;
                        let idx = if (0.30..0.70).contains(&p) && !self.mix_checkpoints_fired[0] {
                            Some(0)
                        } else if (0.70..0.95).contains(&p) && !self.mix_checkpoints_fired[1] {
                            Some(1)
                        } else {
                            None
                        };
                        idx.is_some()
                    }
                {
                    let p = self.cached_info.crossfade_progress;
                    let idx = if p < 0.70 { 0 } else { 1 };
                    self.mix_checkpoints_fired[idx] = true;
                    self.last_crossfade_trigger = std::time::Instant::now();
                    self.trigger_dj(&format!(
                        "Mid-mix phase check ({}%): phase={:+.1}ms. Call \
                         read_alignment. If phase |drift| > 5ms or beat_in_bar \
                         mismatched, call nudge or jump_beats NOW — the sweep \
                         finishes in a few bars.",
                        (p * 100.0) as i32,
                        self.cached_info.phase_offset_ms,
                    ));
                } else if state == crate::audio::engine::EngineState::Crossfading
                    && self.config.claude_dj.mode == crate::config::ClaudeDjMode::Manual
                    && self.cached_info.crossfade_progress < 0.9
                    && self.last_crossfade_trigger.elapsed() >= std::time::Duration::from_secs(10)
                {
                    let pos = self.cached_info.crossfader_pos;
                    let playing_is_a = self.cached_info.playing_is_a;
                    let target: f32 = if playing_is_a { 1.0 } else { -1.0 };
                    let playing = if playing_is_a { "A" } else { "B" };
                    self.last_crossfade_trigger = std::time::Instant::now();
                    self.trigger_dj(&format!(
                        "URGENT: crossfade stalled. Deck {playing} is playing. \
                         Crossfader is at {pos:+.2}, progress is {:.0}%. Call \
                         sweep_crossfader(target={target:+}, bars=4) — ONE call, \
                         the engine paces it. Completion happens automatically \
                         when the sweep reaches {target:+}.",
                        self.cached_info.crossfade_progress * 100.0,
                    ));
                }
            }
        }
        self.last_engine_state = self.cached_info.state;

        // Track the playing-deck's track-id and, when recording, register a
        // cue mark whenever it changes to a new non-empty track. Covers
        // Drive the test_mix state machine.
        if let Some(state) = self.test_mix_state {
            match state {
                TestMixState::WaitForList { ticks } => {
                    // Give the async track-list load up to ~5 s to populate.
                    let has_tracks = self.current_screen().item_count() > 2
                        && self.current_screen().tracks().is_some();
                    if has_tracks {
                        self.engine.clear_queue();
                        // Queue every track on the screen.
                        if let Some(tracks) = self.current_screen().tracks() {
                            // to_vec() releases the immutable borrow before
                            // the engine.enqueue() mutable borrow. clippy
                            // suggests `iter().cloned()` but that holds the
                            // immutable borrow across the loop body.
                            #[allow(clippy::unnecessary_to_owned)]
                            for t in tracks.to_vec() {
                                self.engine
                                    .enqueue(crate::audio::engine::QueueEntry::from(t));
                            }
                        }
                        self.toast.show("test_mix: queued, loading decks…", 1.0);
                        self.test_mix_state = Some(TestMixState::WaitForBothLoaded { ticks: 0 });
                    } else if ticks > 300 {
                        self.toast.show("test_mix: list didn't load", 2.0);
                        self.test_mix_state = None;
                    } else {
                        self.test_mix_state = Some(TestMixState::WaitForList { ticks: ticks + 1 });
                    }
                }
                TestMixState::WaitForBothLoaded { ticks } => {
                    let info = &self.cached_info;
                    let both_ready = info.deck_a_track.is_some()
                        && info.deck_b_track.is_some()
                        && info.deck_a_track.as_ref().map(|t| t.id)
                            != info.deck_b_track.as_ref().map(|t| t.id);
                    if both_ready {
                        self.test_mix_state = Some(TestMixState::Teleport);
                    } else if ticks > 1500 {
                        // ~25 s to load 2 tracks — give up if still not ready.
                        self.toast.show("test_mix: decks never loaded", 2.0);
                        self.test_mix_state = None;
                    } else {
                        self.test_mix_state =
                            Some(TestMixState::WaitForBothLoaded { ticks: ticks + 1 });
                    }
                }
                TestMixState::Teleport => {
                    self.view_mode = ViewMode::Dashboard;
                    self.engine.teleport(&self.config);
                    self.toast
                        .show("test_mix: teleported — crossfade imminent", 2.0);
                    self.test_mix_state = None;
                }
            }
        }

        // Audio callback profile — internal 10s rate-limit; one INFO line per period.
        self.engine.maybe_log_profile();

        if self.last_screen_dump.elapsed() >= std::time::Duration::from_millis(500) {
            match self.view_mode {
                ViewMode::Dashboard => {
                    // Render dashboard as text for screen dump
                    let dash_lines = crate::tui::dashboard::render_dashboard_text(
                        &self.cached_info,
                        self.waveform_mode,
                        120,
                    );
                    crate::ipc::write_screen_lines("Dashboard", &dash_lines);
                }
                _ => {
                    // Build the full screen label list only when we're about
                    // to write it — previously this allocated every tick but
                    // was consumed at most every 500 ms.
                    let all_items: Vec<String> = (0..item_count)
                        .map(|i| self.current_screen().item_label(i))
                        .collect();
                    let bc = self.breadcrumb();
                    crate::ipc::write_screen_dump(&screen_title, &bc, &all_items, self.selected);
                }
            }
            self.last_screen_dump = std::time::Instant::now();
        }
    }
}

/// Build the title for a local-folder TrackList screen — "Local
/// Library (N)" at the root, otherwise "Local · <rel-path> (N)".
fn folder_track_list_title(path: &std::path::Path, root_dir: &str, count: usize) -> String {
    let root = std::path::Path::new(root_dir);
    if path == root {
        return format!("Local Library ({count})");
    }
    match path.strip_prefix(root) {
        Ok(rel) => format!("Local · {} ({count})", rel.display()),
        Err(_) => format!(
            "{} ({count})",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Folder")
        ),
    }
}

fn render_mixer_overlay(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    info: &crate::audio::engine::NowPlayingInfo,
    deck_is_a: bool,
    row: MixerRow,
) {
    use ratatui::style::Modifier;
    use ratatui::widgets::Paragraph;

    let rows = [
        MixerRow::EqLow,
        MixerRow::EqMid,
        MixerRow::EqHigh,
        MixerRow::Filter,
        MixerRow::Fader,
    ];

    let format_value = |r: MixerRow, is_a: bool| -> String {
        match r {
            MixerRow::EqLow => format!(
                "{:+.0} dB",
                if is_a {
                    info.deck_a_eq_low_db
                } else {
                    info.deck_b_eq_low_db
                }
            ),
            MixerRow::EqMid => format!(
                "{:+.0} dB",
                if is_a {
                    info.deck_a_eq_mid_db
                } else {
                    info.deck_b_eq_mid_db
                }
            ),
            MixerRow::EqHigh => format!(
                "{:+.0} dB",
                if is_a {
                    info.deck_a_eq_high_db
                } else {
                    info.deck_b_eq_high_db
                }
            ),
            MixerRow::Filter => format!(
                "{:+.2}",
                if is_a {
                    info.deck_a_filter_pos
                } else {
                    info.deck_b_filter_pos
                }
            ),
            MixerRow::Fader => format!(
                "{:.2}",
                if is_a {
                    info.channel_fader_a
                } else {
                    info.channel_fader_b
                }
            ),
        }
    };

    let sel_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let active_col_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let inactive_col_style = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("          "),
        Span::styled(
            "  Deck A  ",
            if deck_is_a {
                active_col_style
            } else {
                inactive_col_style
            },
        ),
        Span::raw("    "),
        Span::styled(
            "  Deck B  ",
            if !deck_is_a {
                active_col_style
            } else {
                inactive_col_style
            },
        ),
    ]));
    lines.push(Line::from(""));

    for r in rows {
        let label = format!("  {:<8}", r.label());
        let val_a = format_value(r, true);
        let val_b = format_value(r, false);
        let (style_a, style_b) = if r == row {
            if deck_is_a {
                (sel_style, Style::default().fg(Color::Gray))
            } else {
                (Style::default().fg(Color::Gray), sel_style)
            }
        } else {
            (
                Style::default().fg(Color::Gray),
                Style::default().fg(Color::Gray),
            )
        };
        lines.push(Line::from(vec![
            Span::styled(label, Style::default().fg(Color::White)),
            Span::styled(format!("  {val_a:>8}  "), style_a),
            Span::raw("    "),
            Span::styled(format!("  {val_b:>8}  "), style_b),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  Transition: {}    Crossfader: {:+.2}",
            info.transition_type_name, info.crossfader_pos
        ),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Tab] switch deck    [↑↓] select row    [←→] adjust    [r] reset row    [R] reset all    [Esc] close",
        Style::default().fg(Color::DarkGray),
    )));

    // No inner Block::borders — the outer render() wrap already draws
    // one titled " Mixer ". Adding another here caused a visible
    // double outline.
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod click_tests {
    use super::*;

    #[test]
    fn click_target_contains_inclusive_left_top_exclusive_right_bottom() {
        let t = ClickTarget::new(
            10,
            5,
            4,
            2,
            ClickAction::SimulateKey(crossterm::event::KeyCode::Esc),
        );
        // Inside.
        assert!(t.contains(10, 5));
        assert!(t.contains(13, 6));
        // Just past right/bottom edge → outside.
        assert!(!t.contains(14, 5));
        assert!(!t.contains(10, 7));
        // Just before left/top → outside.
        assert!(!t.contains(9, 5));
        assert!(!t.contains(10, 4));
    }

    #[test]
    fn crossfader_range_maps_x_to_minus1_plus1() {
        // SetCrossfaderRange dispatch math: linear map (x_min..x_max) → (−1..+1).
        // Mirrors the logic in dispatch_click_action.
        let map = |click: u16, lo: u16, hi: u16| -> f32 {
            let span = (hi - lo) as f32;
            let rel = (click.saturating_sub(lo) as f32 / span).clamp(0.0, 1.0);
            rel * 2.0 - 1.0
        };
        assert_eq!(map(10, 10, 30), -1.0, "left edge → −1");
        assert_eq!(map(20, 10, 30), 0.0, "midpoint → 0");
        assert_eq!(map(30, 10, 30), 1.0, "right edge → +1");
        // Out-of-range clicks clamp.
        assert_eq!(map(5, 10, 30), -1.0);
        assert_eq!(map(40, 10, 30), 1.0);
    }

    /// Mirrors the log_scroll_offset arithmetic from handle_key:
    ///   Up:   (offset + 1).min(1000)
    ///   Down: offset.saturating_sub(1)
    /// Pure functions so we can pin the cap and the saturating-zero
    /// behavior without spinning up an App.
    fn scroll_up(o: usize) -> usize {
        (o + 1).min(1000)
    }
    fn scroll_down(o: usize) -> usize {
        o.saturating_sub(1)
    }

    #[test]
    fn log_scroll_up_caps_at_1000() {
        assert_eq!(scroll_up(0), 1);
        assert_eq!(scroll_up(999), 1000);
        assert_eq!(scroll_up(1000), 1000, "must not exceed cap");
        assert_eq!(
            scroll_up(usize::MAX / 2),
            1000,
            "extreme value still capped"
        );
    }

    #[test]
    fn log_scroll_down_saturates_at_zero() {
        assert_eq!(scroll_down(10), 9);
        assert_eq!(scroll_down(1), 0);
        assert_eq!(scroll_down(0), 0, "must not underflow at 0");
    }

    #[test]
    fn log_scroll_round_trip_returns_to_start() {
        let start = 42_usize;
        assert_eq!(scroll_down(scroll_up(start)), start);
    }

    #[test]
    fn click_target_with_zero_size_never_contains() {
        // Defensive: a degenerate 0-width or 0-height target shouldn't
        // ever match a click. Guards against renderer bugs that push
        // empty rects.
        let t = ClickTarget::new(
            5,
            5,
            0,
            0,
            ClickAction::SimulateKey(crossterm::event::KeyCode::Esc),
        );
        assert!(!t.contains(5, 5));
        assert!(!t.contains(4, 4));
    }
}
